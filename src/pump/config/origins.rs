//! Where a mailspace's merged config came from.

use std::fs;

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::inherit::{self, ConfigOrigins};

pub(super) fn origins_of(mailspace: &Mailspace) -> Result<ConfigOrigins, VivariumError> {
    let path = mailspace.config_path();
    let raw = fs::read_to_string(&path)?;
    Ok(inherit::load_with_origins(&path, &raw)?.origins)
}
