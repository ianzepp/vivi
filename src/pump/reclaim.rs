//! The post-merge pool reclaim on its own worker thread, board-pass style.
//!
//! Cleaning a merged work lane can do filesystem work that would otherwise
//! block the pump tick. [`Reclaimer`] sends each reclaim to one named worker;
//! a request received while one is still running is deferred rather than
//! queued.

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
    /// Test seam: channel halves for serving a request without a thread.
    #[cfg(test)]
    inline: Option<(Receiver<Unit>, Sender<()>)>,
}

impl Reclaimer {
    /// Start the `pump-reclaim` thread. Dropping this reclaimer closes the
    /// request channel and ends the worker loop.
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
                    let _sent = done_tx.send(());
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
    /// running. A second request is logged and dropped while one is busy.
    pub fn request(&self, ctx: &Ctx, unit: &Unit) {
        self.fold();
        if self.busy.load(Ordering::Relaxed) {
            ctx.log("work-lane clean deferred: reclaim still running");
            return;
        }
        if let Err(error) = self.send.send(unit.clone()) {
            ctx.log(&format!("work-lane clean {}: {error}", unit.need));
            return;
        }
        self.busy.store(true, Ordering::Relaxed);
    }

    /// Drain completion signals and clear `busy` only if a reclaim landed.
    /// With no completion signal, the worker is still running.
    pub(crate) fn fold(&self) {
        let mut landed = false;
        while self.done.try_recv().is_ok() {
            landed = true;
        }
        if landed {
            self.busy.store(false, Ordering::Relaxed);
        }
    }

    /// Build a reclaimer whose worker channels can be served by a test.
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

    /// Serve one inline request on the calling thread, without starting a
    /// worker thread.
    #[cfg(test)]
    pub(crate) fn serve(&self, ctx: &Ctx) {
        let Some((requests, done_tx)) = self.inline.as_ref() else {
            return;
        };
        let Ok(unit) = requests.try_recv() else {
            return;
        };
        reclaim_one(ctx, &unit);
        let _sent = done_tx.send(());
    }
}

/// Recycle the unit's pool work lane after it reaches the merge lane.
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
        Err(error) => ctx.log(&format!("work-lane clean {}: {error}", unit.need)),
    }
}
