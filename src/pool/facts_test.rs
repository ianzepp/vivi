use super::*;

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn unescape_undoes_the_runner_escaping_and_keeps_stray_backslashes() {
    assert_eq!(unescape(r"a\nb\\nc\\d\rz"), "a\nb\\nc\\d\rz");
    assert_eq!(unescape(r"C:\path\x"), "C:\\path\\x");
    assert_eq!(unescape("end\\"), "end\\");
    assert_eq!(unescape(""), "");
}

#[test]
fn facts_are_read_in_order_and_counting_stops_at_the_first_gap() {
    let f = from_fields(&fields(&[
        ("fact_1_cmd", "gpu-query --csv"),
        ("fact_1_exit", "0"),
        ("fact_1_out", r"Some GPU, 1.2\n8 GiB"),
        ("fact_2_cmd", "tool --version"),
        ("fact_2_exit", "127"),
        ("fact_2_out", "tool: not found"),
        ("fact_4_cmd", "after-a-gap"),
        ("job", "j1"),
    ]));
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].command, "gpu-query --csv");
    assert_eq!(f[0].output, "Some GPU, 1.2\n8 GiB");
    assert_eq!(f[1].exit, 127);
    assert_eq!(f[1].status(), "exit 127");
}

#[test]
fn a_missing_exit_reads_as_unknown_and_a_missing_output_as_empty() {
    let f = from_fields(&fields(&[("fact_1_cmd", "x")]));
    assert_eq!((f[0].exit, f[0].output.as_str()), (-1, ""));
    assert!(from_fields(&BTreeMap::new()).is_empty());
}

#[test]
fn exit_124_reads_as_a_timeout() {
    let f = from_fields(&fields(&[("fact_1_cmd", "slow"), ("fact_1_exit", "124")]));
    assert_eq!(f[0].status(), "timed out");
}

#[test]
fn text_shows_each_command_with_its_status_and_indented_output() {
    let f = from_fields(&fields(&[
        ("fact_1_cmd", "uname -srm"),
        ("fact_1_exit", "0"),
        ("fact_1_out", "Linux 6.8 x86_64"),
        ("fact_2_cmd", "bad"),
        ("fact_2_exit", "2"),
        ("fact_2_out", r"e1\ne2"),
    ]));
    assert_eq!(
        text(&f),
        "$ uname -srm  (exit 0)\n  Linux 6.8 x86_64\n$ bad  (exit 2)\n  e1\n  e2\n"
    );
    assert_eq!(text(&[]), "");
}
