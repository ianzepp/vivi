//! The `vivi mcp` command: resolve flags, environment, and the mailspace
//! fallback, then serve the bridge on locked stdin and stdout, diagnostics
//! on stderr, until stdin closes.

use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use vivi::VivariumError;
use vivi::cli::McpCommand;
use vivi::mailspace::Mailspace;
use vivi::mcp::resolve::{self, MailspaceSource, McpFlags, PendingCredential};
use vivi::mcp::{BridgeConfig, Credential, serve};
use vivi::sync::identity::SyncIdentity;

pub(crate) fn handle_mcp_command(command: &McpCommand) -> Result<(), VivariumError> {
    let flags = McpFlags {
        server: command.server.clone(),
        project_id: command.project_id.clone(),
        name: command.name.clone(),
        key_file: command.key_file.clone(),
        project: command.project.clone(),
    };
    let env_lookup = |name: &str| match std::env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        Ok(_) | Err(std::env::VarError::NotPresent | std::env::VarError::NotUnicode(_)) => None,
    };
    let mailspace_lookup = || mailspace_source(command.project.as_deref());
    let resolved = resolve::resolve(&flags, env_lookup, mailspace_lookup)?;
    let config = BridgeConfig {
        settings: resolved.settings,
        credential: load_credential(resolved.credential)?,
    };
    serve_quietly(&config)
}

/// Serve one session. A write to a stdout whose reader is gone ends the
/// bridge as quietly as a closed stdin.
fn serve_quietly(config: &BridgeConfig) -> Result<(), VivariumError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let served = serve(
        config,
        BufReader::new(stdin.lock()),
        stdout.lock(),
        stderr.lock(),
    );
    match served {
        Err(VivariumError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// Turn the pending credential into a live one. A missing mailspace key is
/// never minted here; the error says what to run instead.
fn load_credential(pending: PendingCredential) -> Result<Credential, VivariumError> {
    match pending {
        PendingCredential::ApiKey(key) => Ok(Credential::ApiKey(key)),
        PendingCredential::KeyFile(path) => {
            Ok(Credential::Member(SyncIdentity::from_key_file(&path)?))
        }
        PendingCredential::MailspaceKey(path) => {
            if !path.is_file() {
                return Err(VivariumError::Message(format!(
                    "no mailspace key at {}; run vivi sync key to mint one, or set \
                     VIVI_MCP_API_KEY, or give --name and --key-file",
                    path.display()
                )));
            }
            Ok(Credential::Member(SyncIdentity::from_key_file(&path)?))
        }
    }
}

/// What the mailspace named by `--project` (or the nearest ancestor of the
/// current directory holding `.vivi/mailspace.toml`) offers. An absent
/// mailspace is `None`, not an error: the bridge may run with no mailspace
/// at all, and nothing is created here.
fn mailspace_source(project: Option<&Path>) -> Result<Option<MailspaceSource>, VivariumError> {
    let root = match project {
        Some(root) => Some(root.to_path_buf()),
        None => mailspace_root(std::env::current_dir()?),
    };
    let Some(root) = root else {
        return Ok(None);
    };
    let mailspace = Mailspace::open_root(root)?;
    let sync = mailspace.config.sync.clone().unwrap_or_default();
    Ok(Some(MailspaceSource {
        server: sync.server,
        project: sync.project,
        name: sync.name,
        vivi_dir: mailspace.dir,
    }))
}

/// The nearest ancestor of `start` holding `.vivi/mailspace.toml`.
fn mailspace_root(start: PathBuf) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join(".vivi").join("mailspace.toml").is_file())
        .map(Path::to_path_buf)
}
