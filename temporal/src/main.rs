//! `vivi-temporal`: the per-host worker and the starters.
//!
//! ```text
//! vivi-temporal worker --host <name> --config <file>
//! vivi-temporal lander --config <file> --target <target>
//! vivi-temporal need   --config <file> --target <target> --handle <h> --brief-file <f>
//!                      [--slice <id>:<path,path>]...
//! ```

use std::process::ExitCode;

use vivi_temporal::config::Config;
use vivi_temporal::types::SliceSpec;
use vivi_temporal::worker::{NeedRequest, run_host, run_need, start_lander};

const USAGE: &str = "usage:
  vivi-temporal worker --host <name> --config <file>
  vivi-temporal lander --config <file> --target <target>
  vivi-temporal need --config <file> --target <target> --handle <h> --brief-file <f> [--slice <id>:<paths>]... [--rerun yes]";

struct Args {
    command: String,
    flags: Vec<(String, String)>,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let mut raw = std::env::args().skip(1);
        let command = raw.next().ok_or("no command")?;
        let mut flags = Vec::new();
        while let Some(flag) = raw.next() {
            let name = flag
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected argument {flag:?}"))?;
            let value = raw
                .next()
                .ok_or_else(|| format!("--{name} needs a value"))?;
            flags.push((name.to_string(), value));
        }
        Ok(Args { command, flags })
    }

    fn one(&self, name: &str) -> Result<String, String> {
        self.flags
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| format!("--{name} is required"))
    }

    fn all(&self, name: &str) -> Vec<String> {
        let named = self.flags.iter().filter(|(n, _)| n == name);
        named.map(|(_, v)| v.clone()).collect()
    }
}

/// `--rerun yes` starts a need again after an earlier run ended.
fn rerun_flag(args: &Args) -> Result<bool, String> {
    match args.all("rerun").as_slice() {
        [] => Ok(false),
        [v] if v == "yes" => Ok(true),
        _ => Err("--rerun takes the value yes, once".to_string()),
    }
}

/// `id:path,path` → a slice with that write scope; its brief is the need's.
fn parse_slice(text: &str, brief: &str) -> Result<SliceSpec, String> {
    let (id, paths) = text.split_once(':').unwrap_or((text, ""));
    if id.is_empty() {
        return Err(format!("--slice {text:?} has no id"));
    }
    Ok(SliceSpec {
        id: id.to_string(),
        write_scope: paths
            .split(',')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        brief: brief.to_string(),
    })
}

async fn run(args: Args) -> Result<(), String> {
    let config = Config::load(&args.one("config")?).map_err(|e| e.to_string())?;
    match args.command.as_str() {
        "worker" => run_host(config, &args.one("host")?)
            .await
            .map_err(|e| e.to_string()),
        "lander" => {
            let said = start_lander(&config, &args.one("target")?).await;
            println!("{}", said.map_err(|e| e.to_string())?);
            Ok(())
        }
        "need" => {
            let path = args.one("brief-file")?;
            let brief = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            let slices = args.all("slice");
            let slices = slices.iter().map(|s| parse_slice(s, &brief));
            let request = NeedRequest {
                handle: args.one("handle")?,
                target: args.one("target")?,
                slices: slices.collect::<Result<_, _>>()?,
                brief,
                rerun: rerun_flag(&args)?,
            };
            let outcome = run_need(&config, request)
                .await
                .map_err(|e| e.to_string())?;
            println!("{outcome:?}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
