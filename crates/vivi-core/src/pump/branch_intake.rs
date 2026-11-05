//! Branch intake: typed records for observed source branches, retained
//! generations, proofs, and durable notification/publication intents.
//!
//! This module owns the shared vocabulary. The store owns persistence in
//! additive `pump.db` tables; the other children are inactive seams for the
//! observation, construction, proof, and publication steps.

pub mod capture;
pub mod evidence;
pub mod git;
pub mod model;
pub mod observe;
pub mod prepare;
pub mod proof;
pub mod publish;
pub mod report;
pub mod views;

pub use model::{
    BranchObservation, BranchState, GenerationRecord, GenerationStage, IntakeIntent,
    IntakeIntentKind, NotificationState, ProofPurpose, ProofRecord, PublicationState, TipRecord,
};
