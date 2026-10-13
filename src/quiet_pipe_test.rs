use super::*;

#[test]
fn matches_the_std_stdout_broken_pipe_panic_payload() {
    // Exact payload of `println!` on Unix/macOS: raw OS error 32 (EPIPE).
    let payload = "failed printing to stdout: Broken pipe (os error 32)".to_string();
    assert!(payload_is_stdout_broken_pipe(&payload));
}

#[test]
fn matches_kind_display_and_windows_broken_pipe_wording() {
    // io::Error::from(ErrorKind::BrokenPipe) renders lowercase.
    let kind = "failed printing to stdout: broken pipe".to_string();
    assert!(payload_is_stdout_broken_pipe(&kind));

    // Windows renders EPIPE as "The pipe has been ended.".
    let windows = "failed printing to stdout: The pipe has been ended. (os error 109)".to_string();
    assert!(payload_is_stdout_broken_pipe(&windows));
}

#[test]
fn rejects_stdout_errors_that_are_not_broken_pipes() {
    let full = "failed printing to stdout: No space left on device (os error 28)".to_string();
    assert!(!payload_is_stdout_broken_pipe(&full));

    let io = "failed printing to stdout: device io error".to_string();
    assert!(!payload_is_stdout_broken_pipe(&io));

    let bare = "failed printing to stdout".to_string();
    assert!(!payload_is_stdout_broken_pipe(&bare));
}

#[test]
fn rejects_stderr_print_panics_and_other_payloads() {
    // stderr writes keep the default loud behavior.
    let stderr = "failed printing to stderr: Broken pipe (os error 32)".to_string();
    assert!(!payload_is_stdout_broken_pipe(&stderr));

    let plain = "boom".to_string();
    assert!(!payload_is_stdout_broken_pipe(&plain));

    // Payloads that are neither String nor &'static str.
    let boxed: Box<dyn Any + Send> = Box::new(7_u32);
    assert!(!payload_is_stdout_broken_pipe(boxed.as_ref()));
}
