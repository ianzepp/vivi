//! The board pass on its own worker thread.
//!
//! One board pass is `vivi sync pull`, which can take minutes. Running it
//! inline in the tick blocks dispatch, collect and stop for that whole time.
//! [`BoardPass`] runs it on one named thread instead: the tick thread sends a
//! clone of the state, keeps writing the live state while the pass runs, and
//! folds the worker's result back through [`merge_back`].
//!
//! The worker never touches the live state. [`merge_back`] copies back only
//! `board.at`, the one field the pass owns.

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
        if self.send.send((ctx.clone(), clone)).is_err() {
            return;
        }
        std::mem::take(&mut st.board.sync_pending);
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
                merge_back(st, &done);
                self.busy = false;
            }
            Err(TryRecvError::Empty) => {}
            // The worker is gone; the next tick may request again rather
            // than waiting forever on a pass that will never land.
            Err(TryRecvError::Disconnected) => {
                ctx.log("board worker is gone; the pass is lost");
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

/// Fold one finished pass into the live state: the pass owns only the
/// interval stamp, which restarts at completion.
pub(crate) fn merge_back(st: &mut State, done: &State) {
    st.board.at = done.board.at;
}
