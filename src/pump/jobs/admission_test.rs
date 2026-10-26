//! Companion tests for dispatch admission: catalog construction and pure
//! candidate resolution, compared field-for-field with `role_dispatch::
//! resolve`, the resolver `vivi pool dispatch` runs.

use super::*;
use crate::letter::fixture::task_body_with;
use crate::letter::work::{JobFront, TaskFront};
use crate::mailspace::MailspaceConfig;

/// Two pools joined by a fallback, whose P2 bands name different models, and
/// the roles the resolution cases address: `hand` carries a band, `scout` a
/// slug with a thinking effort.
const CONFIG: &str = r#"
name = "admission-test"

[[identities]]
name = "hand"
kind = "hand"
model = "P2"

[[identities]]
name = "scout"
kind = "hand"
model = "zai/glm-4.7"
thinking = "high"

[pools.alpha]
provider = "local"
slots = 2
fallback = "beta"

[pools.alpha.harness]
name = "codex"
run = "codex exec --model {model} --brief {brief} --rules {rules}"
models = ["openai/gpt-5.2", "zai/glm-4.7", "zai/glm-4.7-air", "claude-opus"]

[[pools.alpha.bands.P2]]
model = "openai/gpt-5.2"
reasoning = "high"

[pools.beta]
provider = "fly"
slots = 1

[pools.beta.harness]
name = "codex"
run = "codex exec --model {model} --brief {brief} --rules {rules}"
models = ["zai/glm-4.7", "openai/gpt-5.2"]

[[pools.beta.bands.P2]]
model = "zai/glm-4.7"
reasoning = "medium"
"#;

fn config() -> MailspaceConfig {
    toml::from_str(CONFIG).expect("mailspace toml")
}

fn catalog() -> DispatchCatalog {
    DispatchCatalog::from_mailspace(&config())
}

/// The dispatch command's role facts for `name`, built straight from the
/// config the way `role_facts` builds them from a role view.
fn facts_of(config: &MailspaceConfig, name: &str) -> Option<RoleFacts> {
    config
        .identities
        .iter()
        .find(|role| role.name == name)
        .map(|role| RoleFacts {
            name: role.name.clone(),
            kind: role.kind.clone(),
            band: role.model.clone(),
            thinking: role.thinking.clone(),
            charter: String::new(),
            read_only: role.read_only,
        })
}

fn job<'a>(role: Option<&'a str>, model: Option<&'a str>, letter: &'a str) -> CandidateJob<'a> {
    CandidateJob {
        role,
        model,
        read_only: false,
        command: false,
        letter,
    }
}

/// A task letter with one front-matter key set.
fn letter_with(set: impl FnOnce(&mut TaskFront)) -> String {
    let mut front = TaskFront::default();
    set(&mut front);
    task_body_with(&front, "Ship it.")
}

/// Resolve `job` on `pool` and require every resolved field to match
/// `role_dispatch::resolve` given the same inputs; returns the resolution.
fn assert_matches_resolver(
    catalog: &DispatchCatalog,
    job: &CandidateJob,
    role: Option<RoleFacts>,
    pool: &str,
) -> ResolvedCandidate {
    let resolved = resolve_candidate(catalog, job, pool).expect("candidate resolves");
    let pool_config = catalog.pool(pool).expect("catalog pool");
    let expected = role_dispatch::resolve(
        pool_config,
        pool,
        &role_dispatch::Input {
            model_flag: job.model,
            read_only_flag: job.read_only,
            role: role.as_ref(),
            body: job.letter,
        },
    )
    .expect("resolver agrees");
    assert_eq!(resolved.pool, pool);
    assert_eq!(resolved.model, Some(expected.model), "model on {pool}");
    assert_eq!(
        resolved.reasoning, expected.reasoning,
        "reasoning on {pool}"
    );
    assert_eq!(resolved.band, expected.band, "band on {pool}");
    assert_eq!(
        resolved.read_only, expected.read_only,
        "read-only on {pool}"
    );
    resolved
}

#[test]
fn candidate_resolution_matches_pool_dispatch() {
    let mailspace = config();
    let catalog = catalog();
    let plain = letter_with(|_| {});

    // Override: the job's model slug wins over the role's band.
    let override_job = job(Some("hand"), Some("zai/glm-4.7"), &plain);
    let resolved = assert_matches_resolver(
        &catalog,
        &override_job,
        facts_of(&mailspace, "hand"),
        "alpha",
    );
    assert_eq!(resolved.model.as_deref(), Some("zai/glm-4.7"));

    // Task band: the letter's `band` key beats the role's slug.
    let banded = letter_with(|front| front.band = Some(String::from("P2")));
    let banded_job = job(Some("scout"), None, &banded);
    let resolved = assert_matches_resolver(
        &catalog,
        &banded_job,
        facts_of(&mailspace, "scout"),
        "alpha",
    );
    assert_eq!(resolved.model.as_deref(), Some("openai/gpt-5.2"));

    // Role slug: a role model the harness offers is a slug, not a band.
    let slug_job = job(Some("scout"), None, &plain);
    let resolved =
        assert_matches_resolver(&catalog, &slug_job, facts_of(&mailspace, "scout"), "alpha");
    assert_eq!(resolved.model.as_deref(), Some("zai/glm-4.7"));
    assert_eq!(resolved.reasoning.as_deref(), Some("high"));

    // Role band: a role band resolves through the pool's own bands table.
    let band_job = job(Some("hand"), None, &plain);
    let resolved =
        assert_matches_resolver(&catalog, &band_job, facts_of(&mailspace, "hand"), "alpha");
    assert_eq!(resolved.model.as_deref(), Some("openai/gpt-5.2"));
    assert_eq!(resolved.band, Some(2));

    // Differing fallback models: alpha falls back to beta, yet the same job
    // resolves independently on each pool, to each pool's own model.
    let on_alpha = resolve_candidate(&catalog, &band_job, "alpha").expect("alpha resolves");
    let on_beta = resolve_candidate(&catalog, &band_job, "beta").expect("beta resolves");
    assert_eq!(on_alpha.model.as_deref(), Some("openai/gpt-5.2"));
    assert_eq!(on_beta.model.as_deref(), Some("zai/glm-4.7"));
    assert_ne!(on_alpha.model, on_beta.model);
    assert_matches_resolver(&catalog, &band_job, facts_of(&mailspace, "hand"), "beta");

    // Built-in auditor: an internal pump job naming the auditor runs as the
    // shipped Auditor role, read-only, even with no local auditor identity.
    let auditor = letter_with(|front| {
        front.band = Some(String::from("P2"));
        front.job = Some(JobFront {
            id: String::from("job-1"),
            kind: String::from("audit"),
            builtin_role: Some(String::from("auditor")),
        });
    });
    let auditor_job = job(None, None, &auditor);
    let resolved = assert_matches_resolver(
        &catalog,
        &auditor_job,
        Some(RoleFacts::builtin_auditor()),
        "alpha",
    );
    assert!(resolved.read_only);

    // A name the catalog does not know is no role at all: a model-less job
    // then has no model anywhere, exactly as pool dispatch refuses it.
    let ghost = job(Some("ghost"), None, &plain);
    let error = resolve_candidate(&catalog, &ghost, "alpha").expect_err("no model");
    assert!(error.to_string().contains("no model"), "got: {error}");
}

#[test]
fn provider_namespace_groups_models_not_hosts() {
    let catalog = catalog();
    let plain = letter_with(|_| {});

    // The key is the namespace before the first `/`.
    let zai = job(None, Some("zai/glm-4.7"), &plain);
    let resolved =
        resolve_candidate(&catalog, &zai, "alpha").expect("zai/glm-4.7 offered by alpha");
    assert_eq!(resolved.provider.as_deref(), Some("zai"));
    let air = job(None, Some("zai/glm-4.7-air"), &plain);
    let resolved =
        resolve_candidate(&catalog, &air, "alpha").expect("zai/glm-4.7-air offered by alpha");
    assert_eq!(resolved.provider.as_deref(), Some("zai"));
    let openai = job(None, Some("openai/gpt-5.2"), &plain);
    let resolved = resolve_candidate(&catalog, &openai, "alpha").expect("offered by alpha");
    assert_eq!(resolved.provider.as_deref(), Some("openai"));

    // An unqualified slug is its own key, within its pool.
    let bare = job(None, Some("claude-opus"), &plain);
    let resolved = resolve_candidate(&catalog, &bare, "alpha").expect("offered by alpha");
    assert_eq!(resolved.provider.as_deref(), Some("claude-opus"));

    // Alpha is a `local` pool: the machine host is never the key. Even the
    // band choice on a local pool keys by the model's namespace.
    assert_eq!(catalog.pool("alpha").expect("alpha").provider, "local");
    let band_job = job(Some("hand"), None, &plain);
    let resolved = resolve_candidate(&catalog, &band_job, "alpha").expect("band choice on alpha");
    assert_eq!(resolved.provider.as_deref(), Some("openai"));
}

#[test]
fn command_jobs_have_no_provider_bucket() {
    let catalog = catalog();
    let plain = letter_with(|_| {});
    let command = CandidateJob {
        role: Some("hand"),
        model: Some("zai/glm-4.7"),
        read_only: true,
        command: true,
        letter: &plain,
    };
    let resolved = resolve_candidate(&catalog, &command, "alpha").expect("command resolves");
    assert_eq!(resolved.model, None);
    assert_eq!(resolved.provider, None);
    assert_eq!(resolved.reasoning, None);
    assert_eq!(resolved.band, None);
    assert!(resolved.read_only);
}
