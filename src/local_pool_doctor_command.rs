use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::collections::BTreeMap;
use vivi::VivariumError;
use vivi::cli::PoolDoctorCommand;
use vivi::mailspace::Mailspace;

use vivi::pool::collect;
use vivi::pool::doctor::{self, Check, Inputs, Report};
use vivi::pool::doctor_pool;
use vivi::pool::doctor_repo;
use vivi::pool::pools::{self, Need, PoolHandle};
use vivi::pool::porcelain;
use vivi::pool::progress::Progress;
use vivi::pool::record::OpenDispatch;
use vivi::pool::selftest::{self, TestOptions};
use vivi::pool::selftest_world::Timing;
use vivi::pool::slot::PoolId;
use vivi::pool::work::{epoch_secs, open_dispatches};
use vivi::pool::{PoolConfig, PoolProvider};

/// `vivi pool doctor [--pool <name>] [--test [--long]]`: print the report;
/// fail when any check failed. Without `--pool` every pool is checked (and
/// `--test` needs `--pool` when there are several). The config is read, not
/// validated up front, because validating it is one of the checks.
pub(crate) fn doctor(command: &PoolDoctorCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let need = if command.test { Need::One } else { Need::Any };
    let handles = pools::open_unvalidated(&space, command.pool.as_deref(), need)?;
    if command.test && !command.allow_spend {
        refuse_unpaid_test(&handles)?;
    }
    let open = open_dispatches(&space)?;
    Progress::stderr(command.porcelain, "")
        .say("checking the providers, the repositories and the pools");
    let mut report = Report { checks: Vec::new() };
    for handle in &handles {
        report.checks.extend(check_pool(command, handle, &open));
    }
    if command.pool.is_none() {
        let configured = pools::configured(&space)?;
        report.checks.extend(removed_pools_check(&open, configured));
    }
    if command.json {
        println!("{}", report.to_json()?);
    } else if command.porcelain {
        report
            .checks
            .iter()
            .for_each(|c| println!("{}", porcelain::check(c)));
        for handle in &handles {
            let counts = report.of_pool(&handle.id.pool).counts();
            println!("{}", porcelain::summary(&handle.id.pool, &counts));
        }
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

/// Refuse `--test` on a pool whose provider bills for it, before anything is
/// checked or rented.
fn refuse_unpaid_test(handles: &[PoolHandle]) -> Result<(), VivariumError> {
    match handles
        .iter()
        .find_map(|h| h.provider.live_test_spend_gate())
    {
        Some(why) => Err(VivariumError::Message(why)),
        None => Ok(()),
    }
}

/// The checks of one pool, with the live test after them when asked.
fn check_pool(
    command: &PoolDoctorCommand,
    handle: &PoolHandle,
    open: &[OpenDispatch],
) -> Vec<Check> {
    let mine: Vec<OpenDispatch> = open
        .iter()
        .filter(|d| d.record.pool == handle.id.pool)
        .cloned()
        .collect();
    let provider = handle.provider.as_ref();
    let report = doctor::run(&Inputs {
        provider,
        config: &handle.config,
        id: &handle.id,
        open: &mine,
        now: epoch_secs(),
        github: &doctor_repo::gather,
    });
    let mut checks = report.checks.clone();
    if command.test {
        let live = live_test(
            command.long,
            command.porcelain,
            (&report, &handle.config, &handle.id),
            provider,
        );
        checks.extend(live.into_iter().map(|c| c.in_pool(&handle.id.pool)));
    }
    checks
}

/// Open dispatches whose pool is no longer in the config: a failure that
/// `reconcile` fixes by closing each as lost.
fn removed_pools_check(
    open: &[OpenDispatch],
    configured: &BTreeMap<String, PoolConfig>,
) -> Option<Check> {
    let gone: Vec<String> = open
        .iter()
        .filter(|d| !configured.contains_key(&d.record.pool))
        .map(|d| {
            format!(
                "{} ({})",
                d.task,
                collect::missing_pool_text(&d.record.pool)
            )
        })
        .collect();
    (!gone.is_empty()).then(|| {
        Check::fail(
            doctor_pool::GROUP,
            "dispatches of removed pools",
            format!(
                "open dispatch with no pool in the config: {}",
                gone.join("; ")
            ),
            "vivi pool reconcile (records each as lost and closes it)",
        )
    })
}

/// The live test, or the reason it did not run: it needs working provider
/// checks and a valid pool table. The repository checks do not block it, because
/// the test never touches the repository.
fn live_test(
    long: bool,
    quiet: bool,
    (report, config, id): (&Report, &PoolConfig, &PoolId),
    provider: &dyn PoolProvider,
) -> Vec<Check> {
    let config_failed = report.checks.iter().any(|c| {
        c.group == doctor_pool::GROUP && c.name == "config" && c.status == doctor::Status::Fail
    });
    if report.provider_failed() || config_failed {
        return vec![Check::skipped(
            selftest::PROVIDER_GROUP,
            "live test",
            "not run: the provider or pool config checks above failed; fix those first",
        )];
    }
    let say = Progress::stderr(quiet, "");
    say.say(&format!(
        "running the live test: {}; expect about {}",
        provider.live_test_notice(),
        if long {
            "10 minutes (the --long step alone is almost 7)"
        } else {
            "3 minutes (about 10 with --long)"
        }
    ));
    let interrupted = install_interrupt();
    let cancelled = || interrupted.load(Ordering::SeqCst);
    let detail = Progress::stderr(quiet, "    ");
    selftest::run_test(
        provider,
        config,
        id,
        &TestOptions {
            long,
            timing: Timing::default(),
            cancelled: &cancelled,
            progress: &|check| {
                if !quiet {
                    print_progress(check);
                }
            },
            starting: &|name| say.say(&format!("  running  {name}")),
            detail: &detail,
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
                    "quit before cleanup finished: machines of the \"<pool>-doctor\" pool may remain; run `vivi pool doctor --test` again to remove them"
                );
                std::process::exit(130);
            }
        });
    }
    flag
}
