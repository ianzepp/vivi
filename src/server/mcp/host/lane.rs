//! `lane_create`: `vivi project lane add` as a host tool.
//!
//! The server owns the host's projects and lanes roots, so the tool makes
//! the whole lane: the bare repository `<HOST_LANES_ROOT>/<name>/<target>.git`,
//! the checkout `<project>/<target>` and the `[pump.targets.<target>]` entry,
//! then answers the project's pump readiness.

use crate::project_create::lane_add::{self, LaneAdd};
use crate::project_create::readiness_of;
use serde_json::{Value, json};

use crate::server::AppState;

use super::super::tools::{ToolError, opt_str, req_str};
use super::{blocking, hosted_entry, require};

/// Add one lane. The `/v1/mcp` host tool.
pub(crate) async fn lane_create(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let state = state.clone();
    let arguments = arguments.clone();
    blocking(move || {
        // One host-project mutation at a time, the same mutex `create_project`
        // and `delete_project` hold, so the archived-name check and the write
        // it guards cannot race another host tool.
        let _one = super::CREATING.lock().unwrap_or_else(|e| e.into_inner());
        add(&state, &arguments)
    })
    .await
}

fn add(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let name = req_str(arguments, "name")?;
    crate::project_create::check_name(name).map_err(|err| ToolError::new(err.to_string()))?;
    if hosted_entry(state, name).is_some_and(|meta| meta.is_archived()) {
        return Err(ToolError::new(format!(
            "{name} is archived: its name cannot take a new lane"
        )));
    }
    let (projects_root, lanes_root, default_prefix) = require(&state.host, "lane_create")?;
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
