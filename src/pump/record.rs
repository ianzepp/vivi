//! The pump's side of pump records: opening the mailspace and replying on a
//! task the pump sent to itself. Nothing here fails the tick: a reply that
//! cannot run is logged and the next pass tries again.

use std::fs;

use crate::mailspace::Mailspace;
use crate::pump::host::{Ctx, shell_quote};

pub(crate) fn open_mailspace(ctx: &Ctx) -> Option<Mailspace> {
    match Mailspace::discover(Some(&ctx.project)) {
        Ok(mailspace) => Some(mailspace),
        Err(error) => {
            ctx.log(&format!("pump records unavailable: {error}"));
            None
        }
    }
}

/// Post a reply on a task the pump sent to itself (a flow step, a slice, the
/// main-rung task). The only other identity on that thread is the pump, so the
/// reply always goes `--to <reply_from>`: a bare `mail reply` has no other
/// recipient to resolve, and `board.reply_to` would send it to the operator.
pub fn reply_on_task(ctx: &Ctx, task: &str, need: &str, stage: &str, body: &str) -> bool {
    reply_with(ctx, task, need, stage, body, &ctx.cfg.reply_from)
}

/// The body-file and log code behind [`reply_on_task`]: `reply_to` is the
/// `--to` value, left off when empty.
fn reply_with(
    ctx: &Ctx,
    target: &str,
    need: &str,
    stage: &str,
    body: &str,
    reply_to: &str,
) -> bool {
    let dir = ctx.home.join("board");
    drop(fs::create_dir_all(&dir));
    let path = dir.join(format!("reply-{need}-{stage}.txt"));
    if fs::write(&path, format!("{body}\n")).is_err() {
        ctx.log(&format!(
            "board reply {need} {stage} rc=1 body file unwritable"
        ));
        return false;
    }
    let mut args = format!(
        "mail reply {} --from {}",
        shell_quote(target),
        shell_quote(&ctx.cfg.reply_from)
    );
    if !reply_to.is_empty() {
        args.push_str(&format!(" --to {}", shell_quote(reply_to)));
    }
    args.push_str(&format!(
        " --body-file {}",
        shell_quote(&path.to_string_lossy())
    ));
    let out = ctx.vivi(&args, 60);
    ctx.log(&format!(
        "board reply {need} {stage} rc={} {}",
        out.code,
        crate::pump::board::last_line(&out.text, 80)
    ));
    out.code == 0
}

#[cfg(test)]
#[path = "record_test.rs"]
mod tests;
