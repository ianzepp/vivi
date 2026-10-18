//! Split an HTTP byte stream into SSE events and flush each one.

use bytes::Bytes;
use futures_util::Stream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// One SSE frame, including its trailing blank line when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub raw: String,
}

impl Event {
    /// `data:` payload joined across continuation lines, or `None`.
    #[must_use]
    pub fn data(&self) -> Option<String> {
        let mut data = Vec::new();
        for line in self.raw.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        if data.is_empty() {
            None
        } else {
            Some(data.join("\n"))
        }
    }

    /// `event:` field, if the frame has one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.raw
            .lines()
            .find_map(|line| line.strip_prefix("event:"))
            .map(str::trim)
    }

    /// Serialize back to wire bytes with a trailing blank line.
    #[must_use]
    pub fn to_bytes(&self) -> Bytes {
        let mut raw = self.raw.clone();
        if !raw.ends_with("\n\n") && !raw.ends_with("\r\n\r\n") {
            if raw.ends_with('\n') {
                raw.push('\n');
            } else {
                raw.push_str("\n\n");
            }
        }
        Bytes::from(raw)
    }
}

/// Pull complete events off `buffer`, leaving a partial tail.
#[must_use]
pub fn take_events(buffer: &mut Vec<u8>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(end) = event_end(buffer) {
        let raw = buffer.drain(..=end).collect::<Vec<_>>();
        if let Ok(text) = String::from_utf8(raw)
            && !text.trim().is_empty()
        {
            out.push(Event { raw: text });
        }
    }
    out
}

fn event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 3)
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| index + 1)
        })
}

/// Map a byte stream into one flushed SSE event at a time.
pub fn map_events<S, E, F>(
    mut incoming: S,
    mut map: F,
) -> impl Stream<Item = Result<Bytes, std::io::Error>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnMut(Event) -> Option<Event> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        let mut buffer = Vec::new();
        while let Some(chunk) = incoming.next().await {
            match chunk {
                Ok(bytes) => push_chunk(&mut buffer, &bytes, &mut map, &tx).await,
                Err(error) => {
                    let _sent = tx.send(Err(std::io::Error::other(error.to_string()))).await;
                    return;
                }
            }
        }
        flush_tail(buffer, map, &tx).await;
    });
    ReceiverStream::new(rx)
}

async fn push_chunk<F>(
    buffer: &mut Vec<u8>,
    bytes: &Bytes,
    map: &mut F,
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
) where
    F: FnMut(Event) -> Option<Event>,
{
    buffer.extend_from_slice(bytes);
    for event in take_events(buffer) {
        if let Some(mapped) = map(event)
            && tx.send(Ok(mapped.to_bytes())).await.is_err()
        {
            return;
        }
    }
}

async fn flush_tail<F>(
    buffer: Vec<u8>,
    mut map: F,
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
) where
    F: FnMut(Event) -> Option<Event>,
{
    if buffer.is_empty() {
        return;
    }
    if let Ok(text) = String::from_utf8(buffer)
        && let Some(mapped) = map(Event { raw: text })
    {
        let _sent = tx.send(Ok(mapped.to_bytes())).await;
    }
}

/// Pass every event through unchanged (still split so each flush is one event).
pub fn passthrough<S, E>(incoming: S) -> impl Stream<Item = Result<Bytes, std::io::Error>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    map_events(incoming, Some)
}
