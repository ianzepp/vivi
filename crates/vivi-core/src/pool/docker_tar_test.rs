use super::*;

fn files() -> Vec<(String, Vec<u8>)> {
    vec![
        ("/work/boot.sh".to_string(), b"echo hi\n".to_vec()),
        ("/work/source/.gitignore".to_string(), b"build/\n".to_vec()),
        (
            "/work/vivi-marks/vivi_pool_state".to_string(),
            vec![b'x'; 700],
        ),
    ]
}

#[test]
fn files_round_trip_and_directories_are_not_returned_as_files() {
    let bytes = build(&files()).unwrap();
    assert_eq!(bytes.len() % 512, 0);
    let back = read(&bytes).unwrap();
    let paths: Vec<&str> = back.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "work/boot.sh",
            "work/source/.gitignore",
            "work/vivi-marks/vivi_pool_state"
        ]
    );
    assert_eq!(back[0].content, b"echo hi\n");
    assert_eq!(back[2].content.len(), 700, "a file past one block");
}

#[test]
fn every_parent_directory_gets_one_entry() {
    let bytes = build(&files()).unwrap();
    let dirs: Vec<String> = bytes
        .chunks(512)
        .filter(|b| b[156] == b'5')
        .map(|b| cstr(&b[..100]))
        .collect();
    assert_eq!(dirs, ["work/", "work/source/", "work/vivi-marks/"]);
}

#[test]
fn the_stream_is_a_tar_the_system_tool_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("a.tar");
    std::fs::write(&archive, build(&files()).unwrap()).unwrap();
    let Ok(out) = std::process::Command::new("tar")
        .arg("-tf")
        .arg(&archive)
        .output()
    else {
        return;
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(listing.contains("work/boot.sh"), "{listing}");
    assert!(listing.contains("work/source/.gitignore"));
}

#[test]
fn a_stream_written_by_the_system_tool_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("marks");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("state"), "ready\n").unwrap();
    let Ok(out) = std::process::Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .arg("-cf")
        .arg("-")
        .arg("-C")
        .arg(dir.path())
        .arg("marks")
        .output()
    else {
        return;
    };
    assert!(out.status.success());
    let back = read(&out.stdout).unwrap();
    let state = back.iter().find(|f| f.path == "marks/state").unwrap();
    assert_eq!(state.content, b"ready\n");
}

#[test]
fn a_long_path_uses_the_prefix_field_and_comes_back_whole() {
    let long = format!("/{}/{}", "d".repeat(90), "f".repeat(90));
    let bytes = build(&[(long.clone(), b"x".to_vec())]).unwrap();
    let back = read(&bytes).unwrap();
    assert_eq!(back[0].path, long.trim_start_matches('/'));
    let too_long = format!("/{}", "z".repeat(300));
    assert!(build(&[(too_long, Vec::new())]).is_err());
}

#[test]
fn a_cut_stream_is_an_error_and_an_empty_one_has_no_files() {
    let bytes = build(&files()).unwrap();
    assert!(read(&bytes[..700]).is_err());
    assert!(read(&[]).unwrap().is_empty());
    assert!(read(&[0u8; 1024]).unwrap().is_empty());
}
