//! `vivi server init`: make the server home `$VIVI_HOME/server` ready to run.
//!
//! Writes a starter `config.toml` (only when none exists), creates `creds/`
//! (mode 700) with each missing secret (mode 600), and the data, projects and
//! lanes directories. An existing secret is never replaced: the store key
//! must not change for an existing data directory. No secret is printed.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use base64::Engine;

use crate::server::config::{ServerConfig, vivi_home};

/// Run init and print what it did.
///
/// # Errors
/// Returns a message naming the first path that could not be written.
pub fn run(port: Option<u16>) -> Result<(), String> {
    let home = vivi_home()?.join("server");
    fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;

    let config = home.join("config.toml");
    if config.exists() {
        println!("kept    {}", config.display());
    } else {
        let port = port.unwrap_or(8080);
        let text = format!(
            "# vivi server config. Every key is optional; `vivi server run --help` lists them.\n\
             # Relative paths are under this directory.\n\
             bind = \"127.0.0.1\"\n\
             port = {port}\n"
        );
        fs::write(&config, text).map_err(|e| format!("cannot write {}: {e}", config.display()))?;
        println!("wrote   {}", config.display());
    }

    let creds = home.join("creds");
    fs::create_dir_all(&creds).map_err(|e| format!("cannot create {}: {e}", creds.display()))?;
    fs::set_permissions(&creds, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot chmod {}: {e}", creds.display()))?;
    secret(&creds, "STORE_ENCRYPTION_KEY", || {
        base64::engine::general_purpose::STANDARD.encode(random::<32>())
    })?;
    secret(&creds, "MODERATION_TOKEN", || hex::encode(random::<24>()))?;
    secret(
        &creds,
        "HOST_OPERATOR_TOKEN",
        || hex::encode(random::<24>()),
    )?;

    add_host_key(&home)?;

    // Read the config back so the directories match what `run` will use.
    let loaded = ServerConfig::load_from(&home)?;
    for dir in [
        Some(loaded.data_dir),
        loaded.host.projects_root,
        loaded.host.lanes_root,
    ]
    .into_iter()
    .flatten()
    {
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        println!("dir     {}", dir.display());
    }
    println!(
        "ready   vivi server run  (listens on {}:{})",
        loaded.bind, loaded.port
    );
    Ok(())
}

/// List this machine's key in `host_keys` unless it is already there.
fn add_host_key(home: &Path) -> Result<(), String> {
    let key = crate::machine_key::MachineKey::load_or_create().map_err(|e| e.to_string())?;
    let public = key.public_key();
    let path = home.join("host_keys");
    if crate::server::config::host_key_list(home).contains(&public) {
        println!("kept    {}", path.display());
        return Ok(());
    }
    let name = crate::role_status::local_host_name().unwrap_or_else(|| "this machine".into());
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let needs_newline = fs::read(&path).is_ok_and(|b| !b.is_empty() && !b.ends_with(b"\n"));
    let prefix = if needs_newline { "\n" } else { "" };
    writeln!(file, "{prefix}{public} {name} (added by vivi server init)")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("host    {} added to host_keys", key.fingerprint());
    Ok(())
}

fn secret(creds: &Path, name: &str, make: impl FnOnce() -> String) -> Result<(), String> {
    let path = creds.join(name);
    if path.exists() {
        println!("kept    {}", path.display());
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    writeln!(file, "{}", make()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("secret  {}", path.display());
    Ok(())
}

fn random<const N: usize>() -> [u8; N] {
    let mut raw = [0u8; N];
    getrandom::fill(&mut raw).expect("the OS random source is available");
    raw
}
