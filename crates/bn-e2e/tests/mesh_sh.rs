//! The plain-ssh mesh helpers (config/jobs/lib/mesh.sh) edit ~/.ssh/config and authorized_keys in
//! place on every join, leave and keeper pass, so a re-run must never duplicate or lose a block.
use std::fs;
use std::process::Command;

const JOBS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/jobs");

/// Run `script` with lib/mesh.sh sourced and `$HOME` at `home`; its stdout.
fn mesh_sh(home: &std::path::Path, script: &str) -> String {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(". \"$(dirname \"$0\")/lib/mesh.sh\"\n{script}"))
        // `$0` decides where lib/mesh.sh finds lib/tunnel.sh, as it does for a real job.
        .arg(format!("{JOBS}/mesh.sh"))
        .env("HOME", home)
        .env("BN_MESH_CONF", home.join("mesh.conf"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn joining_twice_leaves_one_block_and_keeps_everything_else() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".ssh")).unwrap();
    let cfg = home.path().join(".ssh/config");
    fs::write(&cfg, "Host github.com\n  User git\n").unwrap();
    mesh_sh(
        home.path(),
        r#"bn_mesh_set_block ~/.ssh/config vps "$(bn_mesh_block vps 1.2.3.4 root 22)"
           bn_mesh_set_block ~/.ssh/config vps "$(bn_mesh_block vps 5.6.7.8 root 2222)""#,
    );
    let body = fs::read_to_string(&cfg).unwrap();
    assert_eq!(body.matches("# >>> bn mesh vps >>>").count(), 1, "{body}");
    assert!(
        body.contains("HostName 5.6.7.8") && body.contains("Port 2222"),
        "{body}"
    );
    assert!(!body.contains("1.2.3.4"), "{body}");
    assert!(body.starts_with("Host github.com\n  User git\n"), "{body}");

    mesh_sh(home.path(), "bn_mesh_set_block ~/.ssh/config vps ''");
    assert_eq!(
        fs::read_to_string(&cfg).unwrap(),
        "Host github.com\n  User git\n"
    );
}

#[test]
fn a_node_key_is_one_managed_block_that_leave_removes() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".ssh")).unwrap();
    let ak = home.path().join(".ssh/authorized_keys");
    fs::write(&ak, "ssh-ed25519 AAAAmine me@laptop\n").unwrap();
    mesh_sh(
        home.path(),
        "bn_mesh_set_key vps 'ssh-ed25519 AAAAold root@vps'; bn_mesh_set_key vps 'ssh-ed25519 AAAAnew root@vps'",
    );
    let body = fs::read_to_string(&ak).unwrap();
    assert_eq!(body.matches("AAAA").count(), 2, "{body}");
    assert!(
        body.contains("AAAAnew") && body.contains("AAAAmine"),
        "{body}"
    );
    mesh_sh(home.path(), "bn_mesh_set_key vps ''");
    assert_eq!(
        fs::read_to_string(&ak).unwrap(),
        "ssh-ed25519 AAAAmine me@laptop\n"
    );
}

#[test]
fn targets_and_names_are_parsed_strictly() {
    let home = tempfile::tempdir().unwrap();
    let out = mesh_sh(
        home.path(),
        r#"bn_mesh_split root@1.2.3.4:2222 && echo "$_user|$_host|$_port"
           bn_mesh_split box.lan && echo "$_user|$_host|$_port"
           bn_mesh_split root@h:x || echo bad-port
           bn_mesh_valid_name contabo && echo ok-name
           bn_mesh_valid_name '-oProxyCommand=x' || echo bad-name"#,
    );
    assert_eq!(
        out,
        "root|1.2.3.4|2222\n|box.lan|22\nbad-port\nok-name\nbad-name\n"
    );
}

#[test]
fn saved_nodes_are_listed_by_name() {
    let home = tempfile::tempdir().unwrap();
    let out = mesh_sh(
        home.path(),
        "bn_mesh_put contabo target root@1.2.3.4; bn_mesh_put contabo back reverse; \
         bn_mesh_put lan-box target me@10.0.0.2; bn_mesh_nodes; bn_mesh_get contabo back",
    );
    assert_eq!(out, "contabo\nlan-box\nreverse\n");
}
