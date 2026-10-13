//! CLI surface for the `vivi mcp` command.

use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
pub struct McpCommand {
    /// Base URL of the project server (default: VIVI_MCP_SERVER, then
    /// [sync] server in mailspace.toml)
    #[arg(long, value_name = "URL")]
    pub server: Option<String>,

    /// The project's id ON THE SERVER (default: VIVI_MCP_PROJECT_ID, then
    /// [sync] project) — not the mailspace root that --project names
    #[arg(long, value_name = "ID")]
    pub project_id: Option<String>,

    /// Registered member name for an identity credential (default:
    /// VIVI_MCP_NAME, then [sync] name); not needed with an API key
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// 32-byte member seed file (default: VIVI_MCP_KEY_FILE); needs --name
    #[arg(long, value_name = "PATH")]
    pub key_file: Option<PathBuf>,

    /// Project root that owns .vivi/, used only to find a [sync] table and
    /// the mailspace's own key (also accepted globally: vivi --project
    /// <ROOT> mcp); never created
    #[arg(long)]
    pub project: Option<PathBuf>,
}
