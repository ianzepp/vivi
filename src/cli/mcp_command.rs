//! CLI surface for the `vivi mcp` command.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// With no subcommand `vivi mcp` is the stdio bridge; the server and project
/// flags are global so they also follow `tools` and `call`. The credential is
/// an API key in VIVI_MCP_API_KEY.
#[derive(Debug, Args)]
pub struct McpCommand {
    #[command(subcommand)]
    pub command: Option<McpSubcommand>,

    /// Base URL of the project server (default: VIVI_MCP_SERVER, then
    /// [sync] server in mailspace.toml)
    #[arg(long, global = true, value_name = "URL")]
    pub server: Option<String>,

    /// The project id on the server, sent as the `project` argument of
    /// `vivi mcp call` when the tool takes one (default: VIVI_MCP_PROJECT_ID,
    /// then [sync] project); it is not part of the URL
    #[arg(long, global = true, value_name = "ID")]
    pub project_id: Option<String>,

    /// Project root that owns .vivi/, used only to find the [sync] table
    /// (also accepted globally: vivi --project <ROOT> mcp); never created
    #[arg(long, global = true)]
    pub project: Option<PathBuf>,
}

/// One-shot MCP client commands: no stdio session, one request, one answer.
#[derive(Debug, Subcommand)]
pub enum McpSubcommand {
    /// List the server's MCP tools (name and first description line)
    Tools {
        /// Print the raw tools array as JSON
        #[arg(long)]
        json: bool,
    },
    /// Call one MCP tool and print its result (exit 1 when the tool reports
    /// an error, 2 on a transport, auth, or JSON-RPC error)
    Call {
        /// The tool's name
        tool: String,

        /// The tool's arguments as a JSON object (default: {})
        #[arg(value_name = "ARGS_JSON", conflicts_with = "args_file")]
        args_json: Option<String>,

        /// Read the arguments object from a file; `-` reads stdin
        #[arg(long, value_name = "PATH|-")]
        args_file: Option<String>,

        /// Print the full JSON-RPC result object instead of the text
        #[arg(long)]
        json: bool,
    },
}
