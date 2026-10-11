use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vivi::VivariumError;
use vivi::cli::PoolDoctorCommand;
use vivi::mailspace::Mailspace;
use vivi::pool::doctor::{self, Check, Inputs, Report};
use vivi::pool::doctor_pool;
use vivi::pool::doctor_repo;
use vivi::pool::selftest::{self, TestOptions};
use vivi::pool::selftest_world::Timing;
use vivi::pool::work::{epoch_secs, open_dispatches};
use vivi::pool::{PoolConfig, PoolProvider, open_provider};

/// `vivi pool doctor [--test [--long]]`: print the report; fail when any
/// check failed. The config is read, not validated up front, because
/// validating it is one of the checks.
pub(crate) fn doctor(command: &PoolDoctorCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let Some(config) = space.config.pool.clone() else {
        return Err(VivariumError::Config(
            "no [pool] table in .vivi/mailspace.toml".into(),
        ));
    };
    let provider = open_provider(&config, &space.config.name);
    let mut report = doctor::run(&Inputs {
        provider: provider.as_ref(),
        config: &config,
        mailspace_name: &space.config.name,
        open: &open_dispatches(&space)?,
        now: epoch_secs(),
        github: &doctor_repo::gather,
    });
    if command.test {
        let pool = config.pool_name(&space.config.name);
        let checks = live_test(command.long, &report, &config, &pool, provider.as_ref());
        report.checks.extend(checks);
    }
    if command.json {
        println!("{}", report.to_json()?);
    } else {
        print!("{}", report.render());
    }
    match report.counts().fail {
        0 => Ok(()),
        n => Err(VivariumError::Message(format!(
            "doctor: {n} check(s) failed"
        ))),
    }
}

/// The live test, or the reason it did not run: it needs working provider
/// checks and a valid `[pool]`. The repository checks do not block it, because
/// the test never touches the repository.
fn live_test(
    long: bool,
    report: &Report,
    config: &PoolConfig,
    pool: &str,
    provider: &dyn PoolProvider,
) -> Vec<Check> {
    let config_failed = report.checks.iter().any(|c| {
        c.group == doctor_pool::GROUP && c.name == "config" && c.status == doctor::Status::Fail
    });
    if report.provider_failed() || config_failed {
        return vec![Check::skipped(
            selftest::PROVIDER_GROUP,
            "live test",
            "not run: the provider or [pool] checks above failed; fix those first",
        )];
    }
    eprintln!("running the live test: {}", provider.live_test_notice());
    let interrupted = install_interrupt();
    let cancelled = || interrupted.load(Ordering::SeqCst);
    selftest::run_test(
        provider,
        config,
        pool,
        &TestOptions {
            long,
            timing: Timing::default(),
            cancelled: &cancelled,
            progress: &print_progress,
        },
    )
}

fn print_progress(check: &Check) {
    #[allow(clippy::cast_precision_loss)]
    let took = check
        .millis
        .map_or(String::new(), |ms| format!(" ({:.1}s)", ms as f64 / 1000.0));
    eprintln!("  {:<7}  {}{took}", check.status.word(), check.name);
}

/// Catch Ctrl-C so the test can stop and clean up. A second Ctrl-C quits at
/// once and says what may be left.
fn install_interrupt() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let seen = Arc::clone(&flag);
        runtime.spawn(async move {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            seen.store(true, Ordering::SeqCst);
            eprintln!("interrupted: stopping and cleaning up (Ctrl-C again quits now)");
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!(
                    "quit before cleanup finished: machines of the \"-doctor\" pool may remain; run `vivi pool doctor --test` again to remove them"
                );
                std::process::exit(130);
            }
        });
    }
    flag
}
