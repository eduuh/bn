//! `doc_dir` is the other half of the doc sink's trust boundary: a card names the file, the
//! manifest names the directory. Neither alone can put a file anywhere.

use bn_core::agents::parse_agent;
use bn_core::resolver::JobScope;
use std::path::PathBuf;

fn manifest(sinks: &str, doc_dir: &str) -> Result<bn_core::agents::AgentDef, String> {
    let text = format!(
        "---\nname: a\nkind: staff\ngather: \"bn x\"\nsinks: {sinks}\n{doc_dir}\
         description: \"x\"\n---\nbody\n"
    );
    parse_agent(&text, "a", JobScope::Global, PathBuf::from("/tmp/a.md")).map_err(|e| e.to_string())
}

#[test]
fn the_doc_sink_requires_a_directory() {
    let err = manifest(r#"["doc"]"#, "").expect_err("must refuse");
    assert!(err.contains("doc_dir"), "got: {err}");
}

#[test]
fn a_directory_without_the_doc_sink_is_refused() {
    let err = manifest(r#"["attn"]"#, "doc_dir: chronicle\n").expect_err("must refuse");
    assert!(err.contains("doc_dir"), "got: {err}");
}

#[test]
fn a_nested_relative_directory_is_fine() {
    let a = manifest(r#"["doc"]"#, "doc_dir: learn/topics\n").expect("parses");
    assert_eq!(a.doc_dir.as_deref(), Some("learn/topics"));
}

#[test]
fn a_directory_that_climbs_out_is_refused() {
    for dir in ["../secrets", "/etc", "chronicle/../..", "./x", "a//b"] {
        let err = manifest(r#"["doc"]"#, &format!("doc_dir: \"{dir}\"\n"))
            .expect_err(&format!("{dir} must be refused"));
        assert!(err.contains("doc_dir"), "{dir}: got {err}");
    }
}
