//! Tests for `vivi pump log`: tail, filters, follow and truncation.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::Duration;

use super::logview::{Follower, LogFile, LogFilter, ShowOptions, show, tail_lines, view};

const SAMPLE: &str = "\
13:40:00 board: planning new need n1
13:40:01 lane default/2: check 1/4 start: ./scripta/test --stage 1
13:40:02 lane default/10: check 1/4 start: ./scripta/test --stage 1
13:40:03 lane fast/2: merge u1 landed
13:40:04 dispatch u12 to pool a
13:40:05 dispatch u1 to pool b
13:40:06 lane default/2 retry u1
";

fn filter(unit: Option<&str>, grep: Option<&str>) -> LogFilter {
    LogFilter::new(unit, grep)
}

#[test]
fn tail_takes_last_n_unfiltered() {
    let lines = tail_lines(SAMPLE, &LogFilter::default(), 2);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("13:40:05"));
    assert!(lines[1].starts_with("13:40:06"));
    assert!(tail_lines(SAMPLE, &LogFilter::default(), 0).is_empty());
    assert_eq!(tail_lines(SAMPLE, &LogFilter::default(), 50).len(), 7);
}

#[test]
fn unit_matches_a_whole_token_only() {
    let lines = tail_lines(SAMPLE, &filter(Some("u1"), None), 50);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("u12")));
    let text = "00:00:00 pumpfix-u1 x\n00:00:01 unit u1: y\n00:00:02 got u1.\n";
    let lines = tail_lines(text, &filter(Some("u1"), None), 9);
    assert_eq!(lines.len(), 2, "{lines:?}");
}

#[test]
fn grep_is_case_sensitive_substring() {
    assert_eq!(tail_lines(SAMPLE, &filter(None, Some("pool")), 9).len(), 2);
    assert!(tail_lines(SAMPLE, &filter(None, Some("POOL")), 9).is_empty());
}

#[test]
fn filters_combine_with_and_and_count_applies_after() {
    let combined = filter(Some("u1"), Some("retry"));
    let lines = tail_lines(SAMPLE, &combined, 50);
    assert_eq!(lines, vec!["13:40:06 lane default/2 retry u1"]);
    // -n 1 over the two u1 lines keeps the last, not the last overall line.
    let handle = filter(Some("u1"), None);
    assert_eq!(
        tail_lines(SAMPLE, &handle, 1),
        vec!["13:40:06 lane default/2 retry u1"]
    );
    let one = filter(None, Some("lane default/10"));
    assert_eq!(tail_lines(SAMPLE, &one, 1).len(), 1);
}

fn append(path: &std::path::Path, text: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

#[test]
fn follower_reads_appended_complete_lines_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    fs::write(&path, "a\nb\n").unwrap();
    let mut follower = Follower::new(&path, 4);
    assert!(follower.poll().unwrap().is_empty());
    append(&path, "c\npart");
    assert_eq!(follower.poll().unwrap(), vec!["c"]);
    append(&path, "ial\nd\n");
    assert_eq!(follower.poll().unwrap(), vec!["partial", "d"]);
    assert!(follower.poll().unwrap().is_empty());
}

#[test]
fn follower_rereads_after_truncation_and_survives_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let mut follower = Follower::new(&path, 14);
    fs::write(&path, "new\n").unwrap();
    assert_eq!(follower.poll().unwrap(), vec!["new"]);
    fs::remove_file(&path).unwrap();
    assert!(follower.poll().unwrap().is_empty());
    fs::write(&path, "again\n").unwrap();
    assert_eq!(follower.poll().unwrap(), vec!["again"]);
}

#[test]
fn show_prints_filtered_tail_then_appended_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    fs::write(&path, SAMPLE).unwrap();
    let options = ShowOptions {
        count: 1,
        follow: true,
        filter: filter(None, Some("lane default/2")),
    };
    let appender = {
        let path = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            append(
                &path,
                "13:41:00 lane default/3: no\n13:41:01 lane default/2: new\n",
            );
        })
    };
    let mut out = Vec::new();
    show(
        &path,
        &options,
        &mut out,
        Duration::from_millis(20),
        Some(10),
    )
    .unwrap();
    appender.join().unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(
        text,
        "13:40:06 lane default/2 retry u1\n13:41:01 lane default/2: new\n"
    );
}

#[test]
fn show_n_zero_with_follow_prints_only_new_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    fs::write(&path, SAMPLE).unwrap();
    let options = ShowOptions {
        count: 0,
        follow: true,
        filter: LogFilter::default(),
    };
    let appender = {
        let path = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            append(&path, "13:42:00 fresh\n");
        })
    };
    let mut out = Vec::new();
    show(
        &path,
        &options,
        &mut out,
        Duration::from_millis(20),
        Some(10),
    )
    .unwrap();
    appender.join().unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "13:42:00 fresh\n");
}

#[test]
fn show_missing_file_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.txt");
    let options = ShowOptions {
        count: 5,
        follow: false,
        filter: LogFilter::default(),
    };
    let error = show(&path, &options, &mut Vec::new(), Duration::ZERO, Some(0)).unwrap_err();
    assert!(error.to_string().contains("no pump log at"), "{error}");
}

/// A mailspace whose `[pump]` home is `home`, so `view` resolves against it.
fn pump_mailspace(home: &std::path::Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mailspace = dir.path().join(".vivi");
    fs::create_dir_all(&mailspace).unwrap();
    fs::write(
        mailspace.join("mailspace.toml"),
        format!(
            "name = \"logview-test\"\n\n[pump]\nhome = \"{}\"\n",
            home.display()
        ),
    )
    .unwrap();
    dir
}

#[test]
fn view_returns_the_filtered_bounded_tail() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("pump-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("log.txt"), SAMPLE).unwrap();
    let mailspace = pump_mailspace(&home);
    let project = mailspace.path();
    let tail = view(project, LogFile::Log, 2, &LogFilter::default()).unwrap();
    assert_eq!(
        tail,
        vec![
            "13:40:05 dispatch u1 to pool b",
            "13:40:06 lane default/2 retry u1",
        ]
    );
    let filtered = view(project, LogFile::Log, 1, &filter(Some("u1"), None)).unwrap();
    assert_eq!(filtered, vec!["13:40:06 lane default/2 retry u1"]);
    assert!(
        view(project, LogFile::Log, 0, &LogFilter::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn view_reads_the_exceptions_file_when_selected() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("pump-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("log.txt"), SAMPLE).unwrap();
    fs::write(home.join("exceptions.txt"), "13:41:00 stuck u1\n").unwrap();
    let mailspace = pump_mailspace(&home);
    let lines = view(
        mailspace.path(),
        LogFile::Exceptions,
        9,
        &LogFilter::default(),
    )
    .unwrap();
    assert_eq!(lines, vec!["13:41:00 stuck u1"]);
}

#[test]
fn view_missing_file_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("pump-home");
    fs::create_dir_all(&home).unwrap();
    let mailspace = pump_mailspace(&home);
    let error = view(mailspace.path(), LogFile::Log, 5, &LogFilter::default()).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("no pump log at"), "{text}");
    assert!(text.contains("log.txt"), "{text}");
}
