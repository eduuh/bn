use super::self_exe;
use std::path::PathBuf;

#[test]
fn a_reinstalled_binary_resolves_to_its_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let bn = dir.path().join("bn");
    std::fs::write(&bn, b"").unwrap();
    let deleted = PathBuf::from(format!("{} (deleted)", bn.display()));
    assert_eq!(self_exe(Some(deleted)), bn);
    assert_eq!(self_exe(Some(bn.clone())), bn);
}

#[test]
fn no_usable_path_falls_back_to_bn_on_path() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("bn");
    let deleted = PathBuf::from(format!("{} (deleted)", gone.display()));
    assert_eq!(self_exe(Some(deleted)), PathBuf::from("bn"));
    assert_eq!(self_exe(None), PathBuf::from("bn"));
}
