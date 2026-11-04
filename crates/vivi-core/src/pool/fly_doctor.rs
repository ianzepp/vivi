//! The machine-host half of `vivi pool doctor` for the `fly` host.

use super::doctor::Check;
use super::fly::{FlyctlProvider, parse_app_names};
use super::fly_api::fetch_token;

pub const GROUP: &str = "Provider (Fly)";

/// What the probes saw. `None` means the probe was not attempted because one
/// it depends on failed.
#[derive(Debug, Clone)]
pub struct FlyFacts {
    pub app: String,
    pub harness_env: Vec<String>,
    pub cli: Result<String, String>,
    pub login: Option<Result<(), String>>,
    pub token: Option<Result<(), String>>,
    pub app_found: Option<Result<bool, String>>,
    pub secrets: Option<Result<Vec<String>, String>>,
}

impl FlyctlProvider {
    /// Probe `flyctl` and the Fly account. Reads only; no token is kept.
    pub(super) fn gather(&self, harness_env: &[String]) -> FlyFacts {
        let cli = self
            .run(&["version".to_string()], &[])
            .map(|(out, _)| version_line(&out))
            .map_err(|e| e.to_string());
        let login = cli.is_ok().then(|| {
            self.run(&["auth".to_string(), "whoami".to_string()], &[])
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        let signed_in = matches!(login, Some(Ok(())));
        let token = signed_in.then(|| fetch_token().map(|_| ()).map_err(|e| e.to_string()));
        let app_found = signed_in.then(|| self.app_exists());
        let secrets = (matches!(app_found, Some(Ok(true))) && !harness_env.is_empty())
            .then(|| self.secret_names().map_err(|e| e.to_string()));
        FlyFacts {
            app: self.app.clone(),
            harness_env: harness_env.to_vec(),
            cli,
            login,
            token,
            app_found,
            secrets,
        }
    }

    fn app_exists(&self) -> Result<bool, String> {
        let args = ["apps", "list", "--json"].map(str::to_string);
        let (out, _) = self.run(&args, &[]).map_err(|e| e.to_string())?;
        let names = parse_app_names(&out).map_err(|e| e.to_string())?;
        Ok(names.contains(&self.app))
    }
}

/// `fly v0.4.115` from the longer `fly version` line.
fn version_line(output: &str) -> String {
    let first = output.lines().next().unwrap_or_default();
    first
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Turn what was seen into checks.
#[must_use]
pub fn checks(f: &FlyFacts) -> Vec<Check> {
    vec![
        cli_check(f),
        login_check(f),
        token_check(f),
        app_check(f),
        secrets_check(f),
    ]
}

fn cli_check(f: &FlyFacts) -> Check {
    match &f.cli {
        Ok(version) => Check::ok(GROUP, "flyctl", version.clone()),
        Err(e) => Check::fail(
            GROUP,
            "flyctl",
            e.clone(),
            "install flyctl (https://fly.io/docs/flyctl/install/) and put `fly` on PATH",
        ),
    }
}

fn login_check(f: &FlyFacts) -> Check {
    match &f.login {
        Some(Ok(())) => Check::ok(GROUP, "login", "logged in"),
        Some(Err(e)) => Check::fail(GROUP, "login", e.clone(), "fly auth login"),
        None => Check::skipped(GROUP, "login", "needs flyctl"),
    }
}

fn token_check(f: &FlyFacts) -> Check {
    match &f.token {
        Some(Ok(())) => Check::ok(GROUP, "api token", "available (not shown)"),
        Some(Err(e)) => Check::fail(
            GROUP,
            "api token",
            e.clone(),
            "fly auth login (or set FLY_API_TOKEN)",
        ),
        None => Check::skipped(GROUP, "api token", "needs a login"),
    }
}

fn app_check(f: &FlyFacts) -> Check {
    let app = &f.app;
    match &f.app_found {
        Some(Ok(true)) => Check::ok(GROUP, "app", format!("{app} exists")),
        Some(Ok(false)) => Check::fail(
            GROUP,
            "app",
            format!("app {app} does not exist"),
            "vivi pool create (makes the app, then the slots)",
        ),
        Some(Err(e)) => Check::fail(GROUP, "app", e.clone(), "fly apps list"),
        None => Check::skipped(GROUP, "app", "needs a login"),
    }
}

fn secrets_check(f: &FlyFacts) -> Check {
    let name = "secrets";
    if f.harness_env.is_empty() {
        return Check::ok(GROUP, name, "no harness env names configured");
    }
    let have = match (&f.app_found, &f.secrets) {
        (_, Some(Ok(have))) => have,
        (_, Some(Err(e))) => {
            return Check::fail(
                GROUP,
                name,
                e.clone(),
                format!("fly secrets list -a {}", f.app),
            );
        }
        _ => return Check::skipped(GROUP, name, "needs the app"),
    };
    let missing: Vec<&String> = f.harness_env.iter().filter(|n| !have.contains(n)).collect();
    if missing.is_empty() {
        return Check::ok(
            GROUP,
            name,
            format!(
                "all {} harness env names are app secrets",
                f.harness_env.len()
            ),
        );
    }
    let names = missing
        .iter()
        .map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let assigns = missing
        .iter()
        .map(|n| format!("{n}=<value>"))
        .collect::<Vec<_>>()
        .join(" ");
    Check::warn(
        GROUP,
        name,
        format!("no app secret named {names} (ignore a name the image itself provides)"),
        format!("fly secrets set -a {} {assigns}", f.app),
    )
}

impl FlyctlProvider {
    /// The provider's checks, as the pool doctor lists them.
    #[must_use]
    pub fn doctor_checks(&self, harness_env: &[String]) -> Vec<Check> {
        checks(&self.gather(harness_env))
    }
}

#[cfg(test)]
#[path = "fly_doctor_test.rs"]
mod tests;
