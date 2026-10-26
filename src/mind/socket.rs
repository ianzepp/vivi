//! The attach socket: a Unix socket whose file permissions are the access
//! gate. Nothing listens on the network.
//!
//! The protocol is LF-delimited JSON. A client writes one [`Request`] per
//! line; the server answers with [`Reply`] records and ends every request
//! with `{"event":"done"}`. One connection may carry many requests, which
//! is what `vivi mind` (attach) does.
//!
//! Turn coordination: the pi session sits behind one async mutex, the turn
//! lock. An attached prompt that arrives during an autonomous wake queues
//! on that lock (the client is told it is waiting) and runs when the wake
//! ends; a wake that comes due during an attached turn waits the same way.
//! Whether attach should instead steer the in-flight turn is an open
//! question in the design doc.

use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc, watch};

use crate::VivariumError;

use super::pi::{PiSession, TurnEvent};
use super::store::{self, MindState};
use super::wake::WakeRecord;

/// Socket file mode: owner only.
pub const SOCKET_MODE: u32 = 0o600;
/// Mode for a socket directory the server creates.
pub const DIR_MODE: u32 = 0o700;
/// Longest request line accepted.
const MAX_REQUEST: usize = 64 * 1024;

/// One client request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Prompt { text: String },
    Status,
    Pause,
    Resume,
    Stop,
    Compact,
    History { limit: usize },
}

/// One server record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Reply {
    Text { text: String },
    Status { status: StatusView },
    Ok { message: String },
    Error { message: String },
    Done,
}

/// What `vivi mind status` shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusView {
    pub authority: String,
    pub paused: bool,
    pub busy: bool,
    pub session_id: Option<String>,
    pub last_wake: Option<WakeRecord>,
    pub ignored: usize,
}

impl StatusView {
    /// The view of `state` under `authority`, busy or not.
    #[must_use]
    pub fn of(authority: &str, busy: bool, state: &MindState) -> Self {
        Self {
            authority: authority.to_string(),
            paused: state.paused,
            busy,
            session_id: state.session_id.clone(),
            last_wake: state.last_wake.clone(),
            ignored: state.ignored.len(),
        }
    }
}

/// What every connection handler shares with the service loop.
pub struct Shared<P> {
    /// The turn lock around the one session.
    pub pi: Mutex<P>,
    /// True while a turn (autonomous or attached) holds the lock.
    pub busy: AtomicBool,
    pub authority: String,
    pub state_path: PathBuf,
    pub shutdown: watch::Sender<bool>,
}

impl<P> Shared<P> {
    #[must_use]
    pub fn new(pi: P, authority: &str, state_path: PathBuf) -> Self {
        Self {
            pi: Mutex::new(pi),
            busy: AtomicBool::new(false),
            authority: authority.to_string(),
            state_path,
            shutdown: watch::Sender::new(false),
        }
    }
}

/// Bind the socket at `path`: refuse when a live mind answers there,
/// clear a stale socket file, create a missing parent as owner-only, and
/// set the socket owner-only.
///
/// # Errors
/// Fails when another mind is listening, the path is not a socket, or I/O
/// fails.
pub fn bind(path: &Path) -> Result<UnixListener, VivariumError> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(parent)?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(VivariumError::Config(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(VivariumError::Message(String::from(
                "another vivi mind is listening on that socket",
            )));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    Ok(listener)
}

/// Accept connections until shutdown.
pub async fn serve<P: PiSession + 'static>(
    listener: UnixListener,
    socket: PathBuf,
    shared: Arc<Shared<P>>,
) {
    let mut stop = shared.shutdown.subscribe();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let shared = Arc::clone(&shared);
                    let socket = socket.clone();
                    tokio::spawn(async move {
                        if let Err(error) = connection(stream, &socket, shared).await {
                            tracing::debug!(%error, "mind attach connection ended");
                        }
                    });
                }
                Err(error) => tracing::warn!(%error, "mind accept failed"),
            },
            _ = stop.changed() => break,
        }
    }
}

/// Belt and braces on top of the file mode: the peer must be the user who
/// owns the socket file.
fn peer_allowed(stream: &UnixStream, socket: &Path) -> bool {
    let Ok(cred) = stream.peer_cred() else {
        return false;
    };
    std::fs::metadata(socket).is_ok_and(|meta| meta.uid() == cred.uid())
}

async fn connection<P: PiSession + 'static>(
    stream: UnixStream,
    socket: &Path,
    shared: Arc<Shared<P>>,
) -> Result<(), VivariumError> {
    let allowed = peer_allowed(&stream, socket);
    let (read, mut write) = stream.into_split();
    if !allowed {
        let refusal = Reply::Error {
            message: String::from("peer is not the socket owner"),
        };
        return send(&mut write, &refusal).await;
    }
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Read at most one byte past the cap, so an endless line costs
        // MAX_REQUEST of memory, not all of it.
        let limit = u64::try_from(MAX_REQUEST)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if (&mut reader)
            .take(limit)
            .read_until(b'\n', &mut line)
            .await?
            == 0
        {
            return Ok(());
        }
        if line.len() > MAX_REQUEST {
            // The rest of the line is still unread, so the stream cannot be
            // resynchronised: answer and hang up.
            send(&mut write, &error_reply("request too long")).await?;
            return send(&mut write, &Reply::Done).await;
        }
        match serde_json::from_slice::<Request>(line.trim_ascii_end()) {
            Ok(request) => handle(request, &shared, &mut write).await?,
            Err(error) => send(&mut write, &error_reply(&error.to_string())).await?,
        }
        send(&mut write, &Reply::Done).await?;
    }
}

async fn handle<P: PiSession + 'static, W: AsyncWrite + Unpin>(
    request: Request,
    shared: &Arc<Shared<P>>,
    write: &mut W,
) -> Result<(), VivariumError> {
    let reply = match request {
        Request::Prompt { text } => return prompt(text, shared, write).await,
        Request::Status => status(shared),
        Request::Pause => set_paused(shared, true),
        Request::Resume => set_paused(shared, false),
        Request::Stop => {
            shared.shutdown.send_replace(true);
            ok_reply("mind stopping")
        }
        Request::Compact => {
            let mut pi = lock(shared, write).await?;
            match pi.compact().await {
                Ok(summary) => ok_reply(&summary),
                Err(error) => error_reply(&error.to_string()),
            }
        }
        Request::History { limit } => {
            let mut pi = lock(shared, write).await?;
            match pi.history(limit).await {
                Ok(lines) => Reply::Text {
                    text: lines.join("\n"),
                },
                Err(error) => error_reply(&error.to_string()),
            }
        }
    };
    send(write, &reply).await
}

/// Take the turn lock, telling the client when it has to wait.
async fn lock<'a, P, W: AsyncWrite + Unpin>(
    shared: &'a Shared<P>,
    write: &mut W,
) -> Result<tokio::sync::MutexGuard<'a, P>, VivariumError> {
    if let Ok(guard) = shared.pi.try_lock() {
        return Ok(guard);
    }
    let waiting = Reply::Text {
        text: String::from("(waiting for the turn in flight to finish)\n"),
    };
    send(write, &waiting).await?;
    Ok(shared.pi.lock().await)
}

async fn prompt<P: PiSession + 'static, W: AsyncWrite + Unpin>(
    text: String,
    shared: &Arc<Shared<P>>,
    write: &mut W,
) -> Result<(), VivariumError> {
    let mut pi = lock(shared, write).await?;
    let _busy = BusyGuard::hold(&shared.busy);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let turn = pi.prompt(text, Some(tx));
    tokio::pin!(turn);
    let result = loop {
        tokio::select! {
            result = &mut turn => break result,
            Some(event) = rx.recv() => forward(write, event).await?,
        }
    };
    while let Ok(event) = rx.try_recv() {
        forward(write, event).await?;
    }
    if let Err(error) = result {
        send(write, &error_reply(&error.to_string())).await?;
    }
    Ok(())
}

/// Sets a busy flag for its lifetime, so an early return still clears it.
pub struct BusyGuard<'a>(&'a AtomicBool);

impl<'a> BusyGuard<'a> {
    #[must_use]
    pub fn hold(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(flag)
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn forward<W: AsyncWrite + Unpin>(
    write: &mut W,
    event: TurnEvent,
) -> Result<(), VivariumError> {
    match event {
        TurnEvent::Text(text) => send(write, &Reply::Text { text }).await,
        TurnEvent::Settled => Ok(()),
    }
}

fn status<P>(shared: &Shared<P>) -> Reply {
    match store::load_at(&shared.state_path) {
        Ok(state) => Reply::Status {
            status: view(shared, &state),
        },
        Err(error) => error_reply(&error.to_string()),
    }
}

/// The status view over a state snapshot.
#[must_use]
pub fn view<P>(shared: &Shared<P>, state: &MindState) -> StatusView {
    StatusView::of(&shared.authority, shared.busy.load(Ordering::SeqCst), state)
}

fn set_paused<P>(shared: &Shared<P>, paused: bool) -> Reply {
    let result = store::update_at(&shared.state_path, |state| {
        state.paused = paused;
        Ok(())
    });
    match (result, paused) {
        (Ok(()), true) => ok_reply("mind paused: no autonomous wakes, no mutating actions"),
        (Ok(()), false) => ok_reply("mind resumed"),
        (Err(error), _) => error_reply(&error.to_string()),
    }
}

fn ok_reply(message: &str) -> Reply {
    Reply::Ok {
        message: message.to_string(),
    }
}

fn error_reply(message: &str) -> Reply {
    Reply::Error {
        message: message.to_string(),
    }
}

async fn send<W: AsyncWrite + Unpin>(write: &mut W, reply: &Reply) -> Result<(), VivariumError> {
    let mut line = serde_json::to_string(reply)
        .map_err(|error| VivariumError::Parse(format!("encode reply: {error}")))?;
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
    write.flush().await?;
    Ok(())
}

/// A connected attach client.
pub struct Client {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    /// Connect to the mind at `path`.
    ///
    /// # Errors
    /// Fails when no mind is listening there.
    pub async fn connect(path: &Path) -> Result<Self, VivariumError> {
        let stream = UnixStream::connect(path).await.map_err(|error| {
            VivariumError::Message(format!(
                "no vivi mind at {} ({error}); is `vivi mind service run` up?",
                path.display()
            ))
        })?;
        let (read, write) = stream.into_split();
        Ok(Self {
            lines: BufReader::new(read).lines(),
            write,
        })
    }

    /// Send one request and hand each reply to `each` until `done`.
    ///
    /// # Errors
    /// Fails on I/O or an undecodable reply.
    pub async fn request(
        &mut self,
        request: &Request,
        mut each: impl FnMut(&Reply),
    ) -> Result<(), VivariumError> {
        let mut line = serde_json::to_string(request)
            .map_err(|error| VivariumError::Parse(format!("encode request: {error}")))?;
        line.push('\n');
        self.write.write_all(line.as_bytes()).await?;
        self.write.flush().await?;
        while let Some(line) = self.lines.next_line().await? {
            let reply: Reply = serde_json::from_str(&line)
                .map_err(|error| VivariumError::Parse(format!("mind reply: {error}")))?;
            if reply == Reply::Done {
                return Ok(());
            }
            each(&reply);
        }
        Err(VivariumError::Other("mind closed the connection".into()))
    }

    /// Send one request and collect every reply.
    ///
    /// # Errors
    /// As [`Client::request`].
    pub async fn collect(&mut self, request: &Request) -> Result<Vec<Reply>, VivariumError> {
        let mut out = Vec::new();
        self.request(request, |reply| out.push(reply.clone()))
            .await?;
        Ok(out)
    }
}

#[cfg(test)]
#[path = "socket_test.rs"]
mod tests;
