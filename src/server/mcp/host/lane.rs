//! `lane_create`: `vivi project lane add` as a host tool.
//!
//! The server owns the host's projects and lanes roots, so the tool makes
//! the whole lane: the bare repository `<HOST_LANES_ROOT>/<name>/<target>.git`,
//! the checkout `<project>/<target>` and the `[pump.targets.<target>]` entry,
//! then answers the project's pump readiness.

use serde_json::{Value, json};
use vivi::project_create::lane_add::{self, LaneAdd};
use vivi::project_create::readiness_of;

use crate::AppState;

use super::super::tools::{ToolError, opt_str, req_str};
use super::{HostConfig, blocking, require};

/// Add one lane. The `/v1/mcp` host tool.
pub(crate) async fn lane_create(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let host = state.host.clone();
    let arguments = arguments.clone();
    blocking(move || add(&host, &arguments)).await
}

fn add(host: &HostConfig, arguments: &Value) -> Result<Value, ToolError> {
    let name = req_str(arguments, "name")?;
    vivi::project_create::check_name(name).map_err(|err| ToolError::new(err.to_string()))?;
    let (projects_root, lanes_root, default_prefix) = require(host, "lane_create")?;
    let project = projects_root.join(name);
    let mut request = LaneAdd::new(&project, req_str(arguments, "target")?, lanes_root);
    request.git_url_prefix =
        Some(opt_str(arguments, "git_url_prefix")?.map_or(default_prefix, str::to_owned));
    request.seed = super::opt_bool(arguments, "seed", false)?;
    if let Some(source) = opt_str(arguments, "source")? {
        super::source::check(source)?;
        request.source = Some(source.to_owned());
    }
    let added = lane_add::add(&request).map_err(|err| ToolError::new(err.to_string()))?;
    let readiness = readiness_of(&project).map_err(|err| ToolError::new(err.to_string()))?;
    let mut out = serde_json::to_value(&added)
        .map_err(|err| ToolError::new(format!("failed to encode lane_create result: {err}")))?;
    if let Some(object) = out.as_object_mut() {
        object.insert("name".into(), json!(name));
        object.insert("readiness".into(), json!(readiness));
    }
    Ok(out)
}
