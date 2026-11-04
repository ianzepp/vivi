use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use vivi_error::VivariumError;

pub fn secure_create_dir_all(path: &Path) -> Result<(), VivariumError> {
    fs::create_dir_all(path)?;
    secure_dir(path)
}

fn secure_dir(path: &Path) -> Result<(), VivariumError> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
