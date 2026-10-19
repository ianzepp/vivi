//! The post-merge pool reclaim on its own worker thread, board-pass style.
//!
//! Merging a unit recycles its pool work lane (`after_merged`): real
//! filesystem work over worktrees, caches and scratch branches. Running it
//! inline in the merge step blocked collect, dispatch and the status
//! heartbeat for that whole time. [`Reclaimer`] runs it on one named thread
//! instead: the tick thread sends the merged [`Unit`], and a request that
//! arrives while one reclaim is still running is logged and dropped — the
//! next request folds the worker's completion first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::pump::host::Ctx;
use crate::pump::state::Unit;

/// The pool-reclaim worker thread and the tick thread's view of it.
pub struct Reclaimer {
    /// Units to reclaim go to the worker thread through this channel.
    send: Sender<Unit>,
    /// The worker posts one `()` here after each reclaim lands.
    done: Receiver<()>,
    /// A reclaim is in flight; further requests defer until it is folded.
    busy: AtomicBool,
    /// Test seam: the receiving half of `send` and the sending half of
    /// `done`, so `serve` can run a reclaim on the calling thread.
    #[cfg(test)]
    inline: Option<(Receiver<Unit>, Sender<()>)>,
}

impl Reclaimer {
    /// Start the `pump-reclaim` thread. It runs until this `Reclaimer` is
    /// dropped: dropping closes the request channel, which ends the loop.
    #[must_use]
    pub fn start(ctx: &Ctx) -> Reclaimer {
        let (send, requests) = channel::<Unit>();
        let (done_tx, done) = channel::<()>();
        let worker = ctx.clone();
        let spawned = thread::Builder::new()
            .name(String::from("pump-reclaim"))
            .spawn(move || {
                while let Ok(unit) = requests.recv() {
                    reclaim_one(&worker, &unit);
                    let _ = done_tx.send(());
                }
            });
        if let Err(error) = spawned {
            ctx.log(&format!("reclaim worker did not start: {error}"));
        }
        Reclaimer {
            send,
            done,
            busy: AtomicBool::new(false),
            #[cfg(test)]
            inline: None,
        }
    }

    /// Ask the worker to reclaim `unit`'s pool work lane when none is
    /// running. First folds any landed reclaim, so `busy` reflects the
    /// worker; a request that still finds one in flight is logged and
    /// dropped rather than queued.
    pub fn request(&self, ctx: &Ctx, unit: &Unit) {
        self.fold();
        if self.busy.load(Ordering::Relaxed) {
            ctx.log("work-lane clean deferred: reclaim still running");
            return;
        }
        if let Err(e) = self.send.send(unit.clone()) {
            ctx.log(&format!("work-lane clean {}: {e}", unit.need));
            return;
        }
        self.busy.store(true, Ordering::Relaxed);
    }

    /// Drain the worker's completions and clear `busy` when one landed. A
    /// drain that finds nothing leaves `busy` alone: the reclaim is still in
    /// flight. Shared with [`Self::request`]; separate tick paths may call
    /// it directly.
    pub(crate) fn fold(&self) {
        let mut landed = false;
        while self.done.try_recv().is_ok() {
            landed = true;
        }
        if landed {
            self.busy.store(false, Ordering::Relaxed);
        }
    }

    /// One detached reclaimer: the channel halves a test drives itself, no
    /// thread started.
    #[cfg(test)]
    pub(crate) fn detached() -> Reclaimer {
        let (send, requests) = channel();
        let (done_tx, done) = channel();
        Reclaimer {
            send,
            done,
            busy: AtomicBool::new(false),
            inline: Some((requests, done_tx)),
        }
    }

    /// Test seam: serve one requested reclaim on the calling thread, exactly
    /// as the worker would, so a test stays deterministic. A unit must be
    /// waiting on the inline channel.
    #[cfg(test)]
    pub(crate) fn serve(&self, ctx: &Ctx) {
        let Some((requests, done_tx)) = self.inline.as_ref() else {
            return;
        };
        let Ok(unit) = requests.try_recv() else {
            return;
        };
        reclaim_one(ctx, &unit);
        let _ = done_tx.send(());
    }
}

/// Recycle the unit's pool work lane now that it is on the merge lane.
///
/// Missing mailspace or pool is a no-op; a failure is logged, not raised.
fn reclaim_one(ctx: &Ctx, unit: &Unit) {
    match crate::pool::clean::after_merged(&ctx.project, unit) {
        Ok(report) => {
            for action in &report.actions {
                ctx.log(&format!(
                    "work-lane clean {} {} {} {}",
                    action.kind.word(),
                    action.slot,
                    action.bytes,
                    action.note
                ));
            }
        }
        Err(e) => ctx.log(&format!("work-lane clean {}: {e}", unit.need)),
    }
}
