use super::super::config::PoolConfig;
use super::super::slot::PoolId;

const BASE: &str = r#"
provider = "runpod"
slots = 2
image = "runpod/pytorch:2.4.0-py3.11-cuda12.4.1-devel-ubuntu22.04"

[runpod]
api_key_file = "/keys/runpod.key"
gpu_types = ["NVIDIA A100 80GB PCIe", "NVIDIA H100 PCIe"]
"#;

fn parse(extra: &str) -> PoolConfig {
    toml::from_str(&format!("{BASE}{extra}")).unwrap()
}

fn id() -> PoolId {
    PoolId::new("space", "gpu")
}

fn refused(text: &str) -> String {
    toml::from_str::<PoolConfig>(text)
        .unwrap()
        .validate(&id())
        .unwrap_err()
        .to_string()
}

fn refused_with(extra: &str) -> String {
    refused(&format!("{BASE}{extra}"))
}

#[test]
fn a_minimal_pool_validates_and_takes_the_defaults() {
    let config = parse("");
    config.validate(&id()).unwrap();
    let r = &config.runpod;
    assert_eq!(r.gpu_count, 1);
    assert_eq!(r.cloud_type(), "SECURE");
    assert_eq!(r.container_disk_gb, 40);
    assert!(r.min_cuda_version.is_none() && r.data_centers.is_empty());
}

#[test]
fn every_option_is_read() {
    let config = parse(
        "gpu_count = 2\ncloud = \"community\"\ncontainer_disk_gb = 120\nmin_cuda_version = \"12.8\"\ndata_centers = [\"EU-RO-1\", \"US-CA-2\"]\n",
    );
    config.validate(&id()).unwrap();
    let r = &config.runpod;
    assert_eq!(r.gpu_types, ["NVIDIA A100 80GB PCIe", "NVIDIA H100 PCIe"]);
    assert_eq!(r.gpu_count, 2);
    assert_eq!(r.cloud_type(), "COMMUNITY");
    assert_eq!(r.container_disk_gb, 120);
    assert_eq!(r.min_cuda_version.as_deref(), Some("12.8"));
    assert_eq!(r.data_centers, ["EU-RO-1", "US-CA-2"]);
}

#[test]
fn the_key_file_is_required_and_must_be_a_path() {
    let none = BASE.replace("api_key_file = \"/keys/runpod.key\"\n", "");
    assert!(refused(&none).contains("api_key_file is required"));
    for bad in ["relative.key", "", "/", "~/", "/a\\u0007b"] {
        let text = BASE.replace("/keys/runpod.key", bad);
        assert!(refused(&text).contains("api_key_file"), "{bad:?}");
    }
    let home = BASE.replace("/keys/runpod.key", "~/.config/secrets/runpod.key");
    toml::from_str::<PoolConfig>(&home)
        .unwrap()
        .validate(&id())
        .unwrap();
}

#[test]
fn a_tilde_path_is_expanded_and_an_absolute_one_is_not() {
    let config = parse("");
    assert_eq!(
        config.runpod.key_path(),
        std::path::PathBuf::from("/keys/runpod.key")
    );
    let home = parse("").runpod;
    let tilde = super::PoolRunpod {
        api_key_file: Some("~/k".into()),
        ..home
    };
    let path = tilde.key_path();
    assert!(
        path.is_absolute() && path.ends_with("k") && !path.starts_with("~"),
        "{path:?}"
    );
}

#[test]
fn the_gpu_list_must_be_present_clean_and_without_duplicates() {
    assert!(
        refused(&BASE.replace(
            "gpu_types = [\"NVIDIA A100 80GB PCIe\", \"NVIDIA H100 PCIe\"]",
            "gpu_types = []"
        ))
        .contains("gpu_types")
    );
    assert!(
        refused(&BASE.replace("NVIDIA H100 PCIe", "NVIDIA A100 80GB PCIe")).contains("duplicate")
    );
    for bad in ["A;B", " lead", "trail ", "a\\nb", "$(x)", "a,b"] {
        let text = BASE.replace("NVIDIA H100 PCIe", bad);
        assert!(refused(&text).contains("gpu_types"), "{bad:?}");
    }
}

#[test]
fn counts_sizes_and_names_are_bounded() {
    assert!(refused_with("gpu_count = 0\n").contains("gpu_count"));
    assert!(refused_with("gpu_count = 9\n").contains("gpu_count"));
    assert!(refused_with("container_disk_gb = 0\n").contains("container_disk_gb"));
    assert!(refused_with("container_disk_gb = 5000\n").contains("container_disk_gb"));
    assert!(refused_with("cloud = \"spot\"\n").contains("cloud"));
    for bad in ["12", "12.", "12.8.1", "x.y", "12.8; id", ""] {
        assert!(
            refused_with(&format!("min_cuda_version = \"{bad}\"\n")).contains("min_cuda_version"),
            "{bad:?}"
        );
    }
    for bad in ["EU RO", "EU-RO-1,US", "a;b", ""] {
        assert!(
            refused_with(&format!("data_centers = [\"{bad}\"]\n")).contains("data_centers"),
            "{bad:?}"
        );
    }
}

#[test]
fn an_unknown_key_is_named_not_ignored() {
    let said = refused_with("lease_minutes = 5\n");
    assert!(
        said.contains("lease_minutes") && said.contains("runpod"),
        "{said}"
    );
}

#[test]
fn a_pool_name_that_would_not_fit_in_a_pod_name_is_refused() {
    let long = "m".repeat(80);
    let config = parse("");
    let said = config
        .validate(&PoolId::new(&long, &"p".repeat(120)))
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("pod name") || said.contains("too long"),
        "{said}"
    );
}
