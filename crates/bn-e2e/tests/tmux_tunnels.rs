use std::fs::{self, File, FileTimes};
use std::process::Command;
use std::time::{Duration, SystemTime};

fn render(status: Option<&str>, age: Option<i64>) -> String {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".cache/bn")).unwrap();
    fs::create_dir_all(home.path().join(".ssh")).unwrap();
    fs::write(
        home.path().join(".ssh/config"),
        "# >>> bn tunnel cloud-pc-0R6MD\nHost cloud-pc-0R6MD laptop\n# <<< bn tunnel cloud-pc-0R6MD\n",
    )
    .unwrap();
    if let Some(status) = status {
        let path = home.path().join(".cache/bn/tunnels.status");
        fs::write(&path, status).unwrap();
        if let Some(age) = age {
            let duration = Duration::from_secs(age.unsigned_abs());
            let modified = if age >= 0 {
                SystemTime::now() - duration
            } else {
                SystemTime::now() + duration
            };
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
        }
    }
    let output = Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/workflow/bin/tmux-tunnels.sh"
        ))
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn only_connected_nodes_are_shown_with_nicknames_or_short_hostnames() {
    assert_eq!(
        render(
            Some("cloud-pc-0R6MD ok\ncloud-pc-ANBKW down\ncloud-pc-OTHER ok\n"),
            None
        ),
        "#[fg=#7aa2f7]⇄ #[fg=#9ece6a]laptop✓ #[fg=#9ece6a]OTHER✓ "
    );
}

#[test]
fn disconnected_or_unknown_states_leave_no_segment() {
    assert_eq!(
        render(
            Some("cloud-pc-ANBKW down\ncloud-pc-UNKNOWN pending\ncloud-pc-MISSING\n"),
            None
        ),
        ""
    );
}

#[test]
fn missing_or_empty_cache_leaves_no_segment() {
    assert_eq!(render(None, None), "");
    assert_eq!(render(Some(""), None), "");
}

#[test]
fn stale_or_future_cache_keeps_the_watchdog_warning() {
    for age in [600, -600] {
        assert_eq!(
            render(Some("cloud-pc-0R6MD ok\n"), Some(age)),
            "#[fg=#565f89]⇄ ? "
        );
    }
}
