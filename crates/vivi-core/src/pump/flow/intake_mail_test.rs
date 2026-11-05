//! Companion coverage for build admission through the intake phase.
use super::tests::rig;
use crate::pump::flow::mail::MailWriter;
use vivi_error::VivariumError;

type R = Result<(), VivariumError>;

#[test]
fn prerequisite_waits_then_starts() -> R {
    let mut rig = rig(None)?;
    rig.open_kind("dep", "build")?;
    rig.mail.set_prerequisites("build", &["dep"]);
    rig.open_kind("build", "build")?;
    rig.tick()?;
    rig.tick()?;
    assert!(rig.runs("build")?.is_empty());
    assert!(
        rig.store
            .flow_waits()?
            .iter()
            .any(|row| row.need == "build")
    );
    rig.mail.close_need("dep", "landed")?;
    rig.mail.close_record("dep", "landed")?;
    rig.tick()?;
    assert_eq!(rig.runs("build")?.len(), 1);
    assert!(
        !rig.store
            .flow_waits()?
            .iter()
            .any(|row| row.need == "build")
    );
    Ok(())
}

#[test]
fn every_prerequisite_must_land() -> R {
    let mut rig = rig(None)?;
    rig.open_kind("d1", "build")?;
    rig.open_kind("d2", "build")?;
    rig.mail.set_prerequisites("build", &["d1", "d2"]);
    rig.open_kind("build", "build")?;
    rig.mail.close_need("d1", "landed")?;
    rig.mail.close_record("d1", "landed")?;
    rig.tick()?;
    assert!(rig.runs("build")?.is_empty());
    rig.mail.close_need("d2", "landed")?;
    rig.mail.close_record("d2", "landed")?;
    rig.tick()?;
    assert_eq!(rig.runs("build")?.len(), 1);
    Ok(())
}
