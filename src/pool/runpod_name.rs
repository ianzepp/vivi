//! The name of a pod: the only mark RunPod lets a pod carry.
//!
//! A pod has no labels, its name is free text, and names are not unique. So
//! the name holds everything Vivi needs to know about the pod without asking
//! anyone else:
//!
//! ```text
//! vivi-rp1-<mailspace>-<pool>-<mlen>-<index>-<task>-<claimed>-<deadline>-<nonce>
//! ```
//!
//! - `mailspace` and `pool` are slugs and may hold dashes, so `mlen` (the
//!   mailspace's length) is what splits them. The six fields after them hold
//!   no dash and are read from the right.
//! - `index` is the slot index, `task` the task handle (or `x` and ten hex
//!   digits of its hash when the handle is not plain letters and digits),
//!   `claimed` the dispatch time and `deadline` the time after which the pod
//!   must not exist, both in epoch seconds.
//! - `nonce` is twelve random hex digits. It tells this dispatch's pod from
//!   any other pod of the same slot and task, and finds the pod again after a
//!   create whose answer was lost.
//!
//! The characters are lowercase letters, digits and dashes only. Anything
//! that does not decode is not a pod of Vivi's, whatever it starts with.

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use super::slot::PoolId;
use crate::error::VivariumError;

/// What every pod of Vivi's name starts with; the `1` is the layout version.
pub const PREFIX: &str = "vivi-rp1-";
/// The longest name the provider accepts.
pub const MAX_NAME_LEN: usize = 191;
const NONCE_LEN: usize = 12;
const TIME_DIGITS: usize = 10;
const MAX_TASK_LEN: usize = 24;
const MAX_INDEX_DIGITS: usize = 3;
/// The fields after the pool, each one dash-free word.
const TAIL_FIELDS: usize = 6;

/// A pod's name, in parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodName {
    pub mailspace: String,
    pub pool: String,
    pub index: u32,
    pub task: String,
    pub claimed: u64,
    pub deadline: u64,
    pub nonce: String,
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_word(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// A canonical decimal number: digits only, no sign, no leading zero.
fn is_number(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.chars().all(|c| c.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn is_time(value: &str) -> bool {
    value.len() >= TIME_DIGITS && is_number(value, TIME_DIGITS + 1)
}

/// The task field for `handle`: the handle itself when it is a short plain
/// word, else `x` and ten hex digits of its hash.
#[must_use]
pub fn task_field(handle: &str) -> String {
    if is_word(handle, MAX_TASK_LEN) {
        return handle.to_string();
    }
    format!("x{}", &hex::encode(Sha256::digest(handle))[..10])
}

/// Twelve random hex digits.
#[must_use]
pub fn new_nonce() -> String {
    let mut bytes = [0_u8; NONCE_LEN / 2];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

impl PodName {
    /// The name as sent to the provider.
    ///
    /// # Errors
    /// Returns an error if a part is not what the layout allows, or the name
    /// would be longer than the provider takes.
    pub fn encode(&self) -> Result<String, VivariumError> {
        let ok = is_slug(&self.mailspace)
            && is_slug(&self.pool)
            && is_word(&self.task, MAX_TASK_LEN)
            && is_hex(&self.nonce, NONCE_LEN)
            && is_time(&self.claimed.to_string())
            && is_time(&self.deadline.to_string())
            && self.index.to_string().len() <= MAX_INDEX_DIGITS;
        if !ok {
            return Err(VivariumError::Message(format!(
                "cannot name a pod for pool {} task {}: a part is outside the name layout",
                self.pool, self.task
            )));
        }
        let name = format!(
            "{PREFIX}{}-{}-{}-{}-{}-{}-{}-{}",
            self.mailspace,
            self.pool,
            self.mailspace.len(),
            self.index,
            self.task,
            self.claimed,
            self.deadline,
            self.nonce
        );
        if name.len() > MAX_NAME_LEN {
            return Err(VivariumError::Message(format!(
                "the pod name for pool {} would be {} characters; the provider takes at most {MAX_NAME_LEN}",
                self.pool,
                name.len()
            )));
        }
        Ok(name)
    }

    /// The parts of `name`, or `None` when it is not a name this layout made.
    #[must_use]
    pub fn decode(name: &str) -> Option<Self> {
        let rest = name.strip_prefix(PREFIX)?;
        let words: Vec<&str> = rest.rsplitn(TAIL_FIELDS + 1, '-').collect();
        let [nonce, deadline, claimed, task, index, mlen, head] = words.as_slice() else {
            return None;
        };
        if !(is_hex(nonce, NONCE_LEN) && is_time(deadline) && is_time(claimed)) {
            return None;
        }
        let numbers = is_number(index, MAX_INDEX_DIGITS) && is_number(mlen, MAX_INDEX_DIGITS);
        if !is_word(task, MAX_TASK_LEN) || !numbers || !is_slug(head) {
            return None;
        }
        let (mailspace, pool) = split_head(head, number(mlen)?)?;
        Some(Self {
            mailspace: mailspace.to_string(),
            pool: pool.to_string(),
            index: number(index)?,
            task: (*task).to_string(),
            claimed: number(claimed)?,
            deadline: number(deadline)?,
            nonce: (*nonce).to_string(),
        })
    }

    /// True when this pod belongs to pool `id`.
    #[must_use]
    pub fn is_of(&self, id: &PoolId) -> bool {
        self.mailspace == id.mailspace && self.pool == id.pool
    }
}

/// A checked decimal word as a number.
fn number<T: std::str::FromStr>(word: &str) -> Option<T> {
    word.parse::<T>().into_iter().next()
}

/// `<mailspace>-<pool>` split where the mailspace's length says.
fn split_head(head: &str, mlen: usize) -> Option<(&str, &str)> {
    let mailspace = head.get(..mlen)?;
    let pool = head.get(mlen..)?.strip_prefix('-')?;
    (is_slug(mailspace) && is_slug(pool)).then_some((mailspace, pool))
}

/// Refuse a pool whose longest possible pod name would not fit.
///
/// # Errors
/// Returns a configuration error naming the length.
pub fn check_fits(id: &PoolId) -> Result<(), VivariumError> {
    let widest = PodName {
        mailspace: id.mailspace.clone(),
        pool: id.pool.clone(),
        index: 999,
        task: "z".repeat(MAX_TASK_LEN),
        claimed: 99_999_999_999,
        deadline: 99_999_999_999,
        nonce: "f".repeat(NONCE_LEN),
    };
    widest.encode().map(drop).map_err(|_| {
        VivariumError::Config(format!(
            "the mailspace slug and the pool name are too long for a pod name (at most {MAX_NAME_LEN} characters in all); shorten the pool name"
        ))
    })
}

#[cfg(test)]
#[path = "runpod_name_test.rs"]
mod tests;
