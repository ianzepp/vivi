//! Observe configured target branch heads, then fetch only pinned tips needed by callers.
use std::collections::BTreeMap;

use crate::pump::host::{Ctx, shell_quote};

const QUERY_SECS: u64 = 30;
const FETCH_SECS: u64 = 120;

/// One independently settled branch observation. Missing refs and query errors remain local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchHead {
    pub target: String,
    pub branch: String,
    pub sha: Option<String>,
    pub error: Option<String>,
}

/// Query configured main and intake refs together for each target.
/// A missing branch is represented by `sha == None`; malformed responses/errors by `error`.
#[must_use]
pub fn observe(ctx: &Ctx) -> Vec<BranchHead> {
    let mut heads = Vec::new();
    for id in ctx.cfg.target_configs().keys() {
        let Some(target) = ctx.for_target(id) else {
            continue;
        };
        let intake = ctx
            .cfg
            .target_configs()
            .get(id)
            .map(|c| c.intake.clone())
            .unwrap_or_default();
        let mut branches = vec![target.cfg.main.clone()];
        branches.extend(intake);
        branches.sort();
        branches.dedup();
        let refs: Vec<_> = branches.iter().map(|b| format!("refs/heads/{b}")).collect();
        let args = refs
            .iter()
            .map(|r| shell_quote(r))
            .collect::<Vec<_>>()
            .join(" ");
        let out = target.git(
            &format!("ls-remote {} {args}", shell_quote(&target.cfg.remote)),
            QUERY_SECS,
        );
        let mut found = BTreeMap::new();
        let mut bad = None;
        if out.code != 0 {
            bad = Some(format!("ls-remote exit {}: {}", out.code, out.text.trim()));
        } else {
            for line in out.text.lines() {
                let mut parts = line.split_whitespace();
                match (parts.next(), parts.next()) {
                    (Some(sha), Some(name)) if refs.iter().any(|r| r == name) => {
                        if !valid_sha(sha) {
                            bad = Some(format!("invalid object ID {sha}"));
                            break;
                        }
                        found.insert(name.to_owned(), sha.to_owned());
                    }
                    _ => {
                        bad = Some(format!("malformed ls-remote line: {line}"));
                        break;
                    }
                }
            }
        }
        for branch in branches {
            let key = format!("refs/heads/{branch}");
            heads.push(BranchHead {
                target: id.clone(),
                branch,
                sha: if bad.is_none() {
                    found.get(&key).cloned()
                } else {
                    None
                },
                error: bad.clone(),
            });
        }
    }
    heads
}

/// Fetch an observed full object ID only when the caller has established work is needed.
/// Returns an error unless `FETCH_HEAD` resolves to exactly the observation.
pub fn fetch_pinned(ctx: &Ctx, head: &BranchHead) -> Result<String, String> {
    if let Some(error) = &head.error {
        return Err(error.clone());
    }
    let sha = head
        .sha
        .as_deref()
        .ok_or_else(|| format!("{} is missing", head.branch))?;
    if !valid_sha(sha) {
        return Err(format!("invalid observed object ID {sha}"));
    }
    let target = ctx
        .for_target(&head.target)
        .ok_or_else(|| format!("unknown target {}", head.target))?;
    let out = target.git(
        &format!(
            "fetch {} {}",
            shell_quote(&target.cfg.remote),
            shell_quote(sha)
        ),
        FETCH_SECS,
    );
    if out.code != 0 {
        return Err(format!(
            "fetch pinned {sha} exit {}: {}",
            out.code,
            out.text.trim()
        ));
    }
    let resolved = target.git("rev-parse --verify FETCH_HEAD^{commit}", QUERY_SECS);
    let fetched = resolved.text.trim();
    if resolved.code != 0 || fetched != sha || !valid_sha(fetched) {
        return Err(format!(
            "fetch mismatch for {}: observed {sha}, resolved {}",
            head.branch, fetched
        ));
    }
    Ok(fetched.to_owned())
}

fn valid_sha(sha: &str) -> bool {
    (sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "git_test.rs"]
mod tests;
