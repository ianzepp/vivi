use vivi::VivariumError;
use vivi::cli::PoolDoctorCommand;
use vivi::mailspace::Mailspace;
use vivi::pool::doctor::{self, Inputs};
use vivi::pool::doctor_repo;
use vivi::pool::fly::FlyctlProvider;

use crate::local_pool_work_command::{epoch_secs, open_dispatches};

/// `vivi pool doctor`: print the report; fail when any check failed. The
/// config is read, not validated up front, because validating it is one of
/// the checks.
pub(crate) fn doctor(command: &PoolDoctorCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let Some(config) = space.config.pool.clone() else {
        return Err(VivariumError::Config(
            "no [pool] table in .vivi/mailspace.toml".into(),
        ));
    };
    let provider = FlyctlProvider::new(&config.app_name(&space.config.name));
    let report = doctor::run(&Inputs {
        provider: &provider,
        config: &config,
        mailspace_name: &space.config.name,
        open: &open_dispatches(&space)?,
        now: epoch_secs(),
        github: &doctor_repo::gather,
    });
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
