use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_bn"))
        .args(args)
        .output()
        .expect("run bn");
    assert!(
        output.status.success(),
        "bn {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "version output must use stdout");
    String::from_utf8(output.stdout).expect("version output is UTF-8")
}

#[test]
fn flags_and_subcommand_print_identical_build_version() {
    let long_flag = run(&["--version"]);
    assert_eq!(run(&["-V"]), long_flag);
    assert_eq!(run(&["version"]), long_flag);

    let line = long_flag
        .strip_suffix('\n')
        .expect("version output ends with a newline");
    let prefix = format!("bn {} (git ", env!("CARGO_PKG_VERSION"));
    let metadata = line
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(')'))
        .expect("version output contains crate version and build metadata");
    let (git_sha, rest) = metadata
        .split_once(", built ")
        .expect("version output contains git metadata");
    let (build_date, target) = rest
        .split_once(", ")
        .expect("version output contains build date and target");

    assert!(!git_sha.is_empty());
    assert_eq!(build_date.len(), 10);
    assert_eq!(build_date.as_bytes()[4], b'-');
    assert_eq!(build_date.as_bytes()[7], b'-');
    assert!(!target.is_empty());
}
