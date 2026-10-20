//! The planner board pass on its own worker thread, merge-lane style.
//!
//! One board pass is minutes of serial `vivi` calls (`sync pull`, `need
//! list`, `need show`, `mail reply`). Running it inline in the tick blocks
//! dispatch, collect and stop for that whole time. [`BoardPass`] runs it on
//! one named thread instead: the tick thread sends a clone of the state,
//! keeps writing the live state while the pass runs, and folds the worker's
//! result back through [`merge_back`].
//!
//! The worker never touches the live state. Every field it reads is a
//! snapshot, so [`merge_back`] copies back only what the board pass itself
//! owns, and the rules below name who else mutates each field concurrently.
//! Newly queued planner jobs return through the outbox merge without
//! rewinding tick-side dispatch or reconciliation.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;

use crate::pump::board;
use crate::pump::host::Ctx;
use crate::pump::state::State;

/// One board-pass request: the live context at request time, plus a state clone.
type BoardRequest = (Ctx, State);

/// The board pass worker thread and the tick thread's view of it.
pub struct BoardPass {
    /// Live context plus a state clone go to the worker through this channel.
    send: Sender<BoardRequest>,
    /// Finished passes come back through this channel.
    results: Receiver<State>,
    /// A pass is in flight; no second one is requested until it lands.
    busy: bool,
    /// The exact state clone the worker holds.
    snapshot: Option<State>,
}

impl BoardPass {
    /// Start the `pump-board` thread. It runs until this `BoardPass` is
    /// dropped: dropping closes the request channel, which ends the loop.
    /// Each request carries the live [`Ctx`] so a config reload applies to
    /// the next pass without restarting the pump.
    #[must_use]
    pub fn start(ctx: &Ctx) -> BoardPass {
        let (send, requests) = channel::<BoardRequest>();
        let (out, results) = channel::<State>();
        let spawned = thread::Builder::new()
            .name(String::from("pump-board"))
            .spawn(move || board_loop(&requests, &out));
        if let Err(error) = spawned {
            ctx.log(&format!("board worker did not start: {error}"));
        }
        BoardPass {
            send,
            results,
            busy: false,
            snapshot: None,
        }
    }

    /// One detached pass: the channel ends a test drives itself, no thread
    /// started.
    #[cfg(test)]
    pub(crate) fn detached() -> (BoardPass, Receiver<BoardRequest>, Sender<State>) {
        let (send, requests) = channel();
        let (out, results) = channel();
        let pass = BoardPass {
            send,
            results,
            busy: false,
            snapshot: None,
        };
        (pass, requests, out)
    }

    /// Test seam: serve one requested pass on the calling thread, exactly as
    /// the worker would, so a test tick stays deterministic. When a request
    /// arrived, run the board pass with the context sent on that request.
    #[cfg(test)]
    pub(crate) fn serve(
        &mut self,
        ctx: &Ctx,
        st: &mut State,
        requests: &Receiver<BoardRequest>,
        out: &Sender<State>,
    ) {
        let Ok((worker_ctx, mut clone)) = requests.try_recv() else {
            return;
        };
        board::board(&worker_ctx, &mut clone);
        if out.send(clone).is_err() {
            return;
        }
        self.apply(ctx, st);
    }

    /// Ask the worker for a board pass when the interval is due and no pass
    /// is running. The live `board.at` stays put: the worker sets the
    /// completion time on its own clone ([`merge_back`] copies it back).
    /// The pending sync push travels to the worker on the clone and is
    /// cleared on the live state, so a record posted during the pass sets it
    /// again (`attempt_record` owns that write).
    pub fn request(&mut self, ctx: &Ctx, st: &mut State) {
        if self.busy || ctx.now().saturating_sub(st.board.at) < ctx.cfg.board.every {
            return;
        }
        let mut clone = st.clone();
        // The tick thread owns due-ness; a zero stamp makes the worker's own
        // interval check in `board` pass.
        clone.board.at = 0;
        if self.send.send((ctx.clone(), clone.clone())).is_err() {
            return;
        }
        std::mem::take(&mut st.board.sync_pending);
        self.snapshot = Some(clone);
        self.busy = true;
    }

    /// Fold a finished pass back into the live state. An empty channel means
    /// the worker is still busy.
    pub fn apply(&mut self, ctx: &Ctx, st: &mut State) {
        if !self.busy {
            return;
        }
        match self.results.try_recv() {
            Ok(done) => {
                if let Some(snapshot) = self.snapshot.take() {
                    merge_back(st, &snapshot, &done);
                }
                self.busy = false;
            }
            Err(TryRecvError::Empty) => {}
            // The worker is gone; the next tick may request again rather
            // than waiting forever on a pass that will never land.
            Err(TryRecvError::Disconnected) => {
                ctx.log("board worker is gone; the pass is lost");
                self.snapshot = None;
                self.busy = false;
            }
        }
    }
}

/// The worker loop: block on one request, run the board pass with the live
/// context from that request, hand the state back. The loop ends when the
/// tick thread drops its sender.
fn board_loop(requests: &Receiver<BoardRequest>, out: &Sender<State>) {
    while let Ok((ctx, mut clone)) = requests.recv() {
        clone.board.at = 0;
        board::board(&ctx, &mut clone);
        if out.send(clone).is_err() {
            break;
        }
    }
}

/// Fold one finished pass into the live state: only the fields the board pass
/// itself owns move, and only for the handles the pass touched.
pub(crate) fn merge_back(st: &mut State, snap: &State, done: &State) {
    merge_needs(st, snap, done);
    merge_plans_seen(st, snap, done);
    merge_planning(st, snap, done);
    merge_jobs(st, snap, done);
    merge_replied(st, snap, done);
    // Only the board pass writes `replan`.
    st.board.replan.clone_from(&done.board.replan);
    merge_revision_planning(st, snap, done);
    // The interval restarts at completion.
    st.board.at = done.board.at;
}

/// Fold worker-created jobs into the live outbox without rewinding jobs that
/// the tick thread dispatched or reconciled while the board pass was running.
fn merge_jobs(st: &mut State, snap: &State, done: &State) {
    for (id, completed) in &done.jobs {
        let Some(snapshot) = snap.jobs.get(id) else {
            st.jobs
                .entry(id.clone())
                .or_insert_with(|| completed.clone());
            continue;
        };
        if snapshot == completed {
            continue;
        }
        if st.jobs.get(id).is_none_or(|live| live == snapshot) {
            st.jobs.insert(id.clone(), completed.clone());
        }
    }
}

/// (a) A need the worker dropped (replan surgery) leaves the live state; a
/// need the tick thread added during the pass is not in `done`, but it is not
/// in `snap` either, so it stays. (b) `amend` only mutates the three thread
/// fields of a record, so only those come back; verdict, units and revision
/// changed on the tick thread during the pass survive.
fn merge_needs(st: &mut State, snap: &State, done: &State) {
    let dropped: Vec<String> = snap
        .needs
        .keys()
        .filter(|handle| !done.needs.contains_key(*handle))
        .cloned()
        .collect();
    for handle in dropped {
        st.needs.remove(&handle);
    }
    for (handle, record) in &done.needs {
        if !snap.needs.contains_key(handle) {
            continue;
        }
        let Some(live) = st.needs.get_mut(handle) else {
            continue;
        };
        live.thread_seen_at = record.thread_seen_at;
        if live.thread_sha256 != record.thread_sha256 {
            live.thread_sha256.clone_from(&record.thread_sha256);
        }
        live.amendment_pending = record.amendment_pending;
    }
}

/// (c) Entries the pass dropped (replan surgery); one `plan::ingest` added
/// during the pass stays.
fn merge_plans_seen(st: &mut State, snap: &State, done: &State) {
    st.plans_seen
        .retain(|seen| !(snap.plans_seen.contains(seen) && !done.plans_seen.contains(seen)));
}

/// (d) Planning entries the pass expired go, entries it launched land with
/// the worker's timestamps — except for a need that entered the live state
/// during the pass: that one is already being planned by the tick thread and
/// must not gain a second planner.
fn merge_planning(st: &mut State, snap: &State, done: &State) {
    st.board.planning.retain(|handle, _| {
        !(snap.board.planning.contains_key(handle) && !done.board.planning.contains_key(handle))
    });
    for (handle, at) in &done.board.planning {
        if snap.board.planning.contains_key(handle) {
            continue;
        }
        if st.needs.contains_key(handle) && !snap.needs.contains_key(handle) {
            continue;
        }
        st.board.planning.insert(handle.clone(), *at);
    }
}

/// (e) Reply keys the pass did not record are dropped, the ones it recorded
/// are appended in its order.
fn merge_replied(st: &mut State, snap: &State, done: &State) {
    st.board
        .replied
        .retain(|key| !(snap.board.replied.contains(key) && !done.board.replied.contains(key)));
    for key in &done.board.replied {
        if snap.board.replied.contains(key) || st.board.replied.contains(key) {
            continue;
        }
        st.board.replied.push(key.clone());
    }
}

/// (g) Revision planners the pass expired leave; the ones it launched land
/// with the worker's timestamps.
fn merge_revision_planning(st: &mut State, snap: &State, done: &State) {
    st.board.revision_planning.retain(|handle, _| {
        !(snap.board.revision_planning.contains_key(handle)
            && !done.board.revision_planning.contains_key(handle))
    });
    for (handle, at) in &done.board.revision_planning {
        if snap.board.revision_planning.contains_key(handle) {
            continue;
        }
        st.board.revision_planning.insert(handle.clone(), *at);
    }
}
