use super::*;

fn host() -> HostConfig {
    HostConfig {
        projects_root: None,
        lanes_root: Some(PathBuf::from("/mnt/vivi/lanes")),
        git_url_prefix: Some("git@hub:".into()),
        default_pool: None,
    }
}

#[test]
fn prefixed_urls_map_under_the_lanes_root() {
    assert_eq!(
        lane_path("git@hub:faberlang/radix.git", &host()),
        Some(PathBuf::from("/mnt/vivi/lanes/faberlang/radix.git"))
    );
    assert_eq!(lane_path("git@hub:../etc.git", &host()), None);
    assert_eq!(lane_path("git@elsewhere:x/y.git", &host()), None);
}

#[test]
fn file_urls_and_absolute_paths_are_read_as_they_are() {
    assert_eq!(
        lane_path("file:///srv/lanes/a/b.git", &host()),
        Some(PathBuf::from("/srv/lanes/a/b.git"))
    );
    assert_eq!(
        lane_path("/srv/lanes/a/b.git", &HostConfig::default()),
        Some(PathBuf::from("/srv/lanes/a/b.git"))
    );
}
