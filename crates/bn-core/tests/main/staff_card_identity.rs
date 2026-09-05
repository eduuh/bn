//! The card fields that let a `.md` agent express what a native one used to: a stable `key`, an
//! `auto` hand-off to an unattended worker, and the `replace` contract.
//!
//! These are the capabilities an agent's *prompt* needs before its Rust twin can be deleted — a
//! patrol that re-derives its findings every run has to be able to say "this is my whole current
//! set" and "this one is safe for the night shift to pull", or migrating it silently loses both.

use bn_core::staff::{build_prompt_with, validate_card};

const AGENT: &str = "sentinel";
const TODAY: &str = "2026-09-03";

fn sinks() -> Vec<String> {
    vec!["attn".to_string()]
}

fn doc_sink() -> Vec<String> {
    vec!["doc".to_string()]
}

fn card_json(items: &str, replace: &str) -> String {
    format!(
        r#"{{"agent":"{AGENT}","date":"{TODAY}",{replace}"headline":"Estate patrol",
            "body_md":"the useful part","items":[{items}]}}"#
    )
}

#[test]
fn a_key_survives_validation_so_the_item_can_refresh_in_place() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"TODO in parse.rs is 90 days old",
            "key":"todo-age:bn:src/parse.rs:42","detail":"added 2026-06-05"}"#,
        "",
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, None).expect("valid card");

    assert_eq!(card.items[0].key, "todo-age:bn:src/parse.rs:42");
    assert_eq!(card.items[0].detail, "added 2026-06-05");
}

/// The trust boundary: `replace` decides whether items the agent stopped reporting get acked, so a
/// run's own output must not be able to claim it. Only the manifest grants it.
#[test]
fn a_card_cannot_grant_itself_the_replace_power() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"a finding","key":"k1"}"#,
        r#""replace":true,"#,
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, None).expect("valid card");

    assert!(
        !card.replace,
        "a card that asserted replace against a manifest that does not declare it must not keep it"
    );
}

#[test]
fn the_manifest_grants_replace_even_when_the_card_omits_it() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"a finding","key":"k1"}"#,
        "",
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY, true, None).expect("valid card");

    assert!(card.replace);
}

/// An `auto` item is pulled by a worker that is handed `prompt` and nothing else, so the pairing is
/// required rather than defaulted — an elevated item with no brief would park a worker on nothing.
#[test]
fn an_auto_item_without_a_prompt_is_dropped() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"fix the flake","mode":"auto",
            "repo":"bn","branch":"fix/flake"},
           {"kind":"attn","severity":"warn","title":"fix the other flake","mode":"auto",
            "prompt":"Make verify green on bn.","repo":"bn","branch":"fix/other"}"#,
        "",
    );

    let card =
        validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, None).expect("card must survive");

    assert_eq!(
        card.items.len(),
        1,
        "only the prompt-less auto item is dropped"
    );
    assert_eq!(card.items[0].title, "fix the other flake");
    assert!(card.body_md.contains("mode 'auto' requires a prompt"));
}

#[test]
fn an_unknown_mode_is_dropped() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"a finding","mode":"yolo"}"#,
        "",
    );

    let card =
        validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, None).expect("card must survive");

    assert!(card.items.is_empty());
    assert!(card.body_md.contains("invalid mode 'yolo'"));
}

/// The contract the agent is handed has to change with `replace`, or a patrol is never told that
/// omitting a finding closes it.
#[test]
fn the_replace_contract_is_advertised_only_when_granted() {
    let on = build_prompt_with("body", AGENT, &sinks(), "{}", TODAY, true, None);
    let off = build_prompt_with("body", AGENT, &sinks(), "{}", TODAY, false, None);

    assert!(on.contains("MUST carry a stable `key`"));
    assert!(on.contains(r#""replace": true"#));
    assert!(off.contains("MAY carry a stable `key`"));
    assert!(
        !off.contains("is treated as fixed"),
        "an event-reporting agent must not be told that silence resolves items"
    );
}

/// A `doc` item is a whole file, so the two halves that make it one are required rather than
/// defaulted — an unnamed or empty doc lands as something that reads like a bug in the store.
#[test]
fn a_doc_needs_both_a_name_and_a_body() {
    let raw = card_json(
        r##"{"kind":"doc","title":"the chronicle","body":"# Chronicle\n\nthe week."},
           {"kind":"doc","title":"the chronicle","key":"2026-W36"},
           {"kind":"doc","title":"the chronicle","key":"2026-W36","body":"# Chronicle\n\nthe week."}"##,
        "",
    );

    let card =
        validate_card(&raw, AGENT, &doc_sink(), &[], TODAY, false, None).expect("card survives");

    assert_eq!(card.items.len(), 1, "only the complete doc is kept");
    assert_eq!(card.items[0].key, "2026-W36");
    assert!(card.body_md.contains("requires a key naming the file"));
    assert!(card.body_md.contains("requires a body"));
}

/// The trust boundary that makes the doc sink safe at all: the manifest owns the directory, a card
/// owns only the filename. Card content is untrusted output derived from untrusted input, so a key
/// that could climb out of that directory is the difference between an agent writing its own
/// archive and an agent writing anywhere in the notes repo.
#[test]
fn a_doc_key_cannot_escape_the_agents_directory() {
    for key in [
        "../../../etc/passwd",
        "/etc/passwd",
        "sub/dir",
        "..",
        "week..x/",
    ] {
        let raw = card_json(
            &format!(r#"{{"kind":"doc","title":"t","key":"{key}","body":"x"}}"#),
            "",
        );
        let card = validate_card(&raw, AGENT, &doc_sink(), &[], TODAY, false, None)
            .expect("card survives");
        assert!(
            card.items.is_empty(),
            "key {key:?} must be refused, got {:?}",
            card.items
        );
    }
}

#[test]
fn a_body_on_a_non_doc_item_is_refused() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"a finding","body":"a whole document"}"#,
        "",
    );

    let card =
        validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, None).expect("card survives");

    assert!(card.items.is_empty());
    assert!(card
        .body_md
        .contains("'body' applies to kinds 'doc' and 'note-append'"));
}

/// A summarizing agent's whole discipline is that it does not escalate — and stating that in prose
/// does not hold, because the output contract offers `urgent` to everyone and a model handed an
/// alarming packet reaches for it. The ceiling is declared and enforced.
#[test]
fn a_severity_ceiling_clamps_rather_than_drops() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"urgent","title":"the sky is falling"}"#,
        "",
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, Some("warn"))
        .expect("card survives");

    assert_eq!(card.items.len(), 1, "the item is kept, not dropped");
    assert_eq!(card.items[0].severity, "warn");
    assert!(
        !card.body_md.contains("Dropped"),
        "over-reaching on severity must not cost the reader the item"
    );
}

#[test]
fn a_ceiling_leaves_a_lower_severity_alone() {
    let raw = card_json(r#"{"kind":"attn","severity":"info","title":"a note"}"#, "");
    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY, false, Some("warn")).expect("ok");
    assert_eq!(card.items[0].severity, "info");
}

/// Requiring a branch made the note sink unreachable for every agent whose packet is repo-scoped:
/// they were told to file notes they could never file, and the item was dropped citing a field
/// their input does not have. A repo-scoped note belongs on the repo's own note.
#[test]
fn a_repo_scoped_note_item_lands_on_the_repos_own_note() {
    let raw = card_json(
        r#"{"kind":"note-append","severity":"info","title":"today's focus","repo":"bn"}"#,
        "",
    );

    let card = validate_card(&raw, AGENT, &["note".to_string()], &[], TODAY, false, None)
        .expect("card survives");

    assert_eq!(card.items.len(), 1, "must not be dropped: {}", card.body_md);
    assert_eq!(card.items[0].branch, "main");
}

#[test]
fn a_routed_item_with_no_repo_at_all_is_still_dropped() {
    let raw = card_json(
        r#"{"kind":"note-append","severity":"info","title":"nowhere to put this"}"#,
        "",
    );

    let card = validate_card(&raw, AGENT, &["note".to_string()], &[], TODAY, false, None)
        .expect("card survives");

    assert!(card.items.is_empty());
    assert!(card.body_md.contains("requires a repo"));
}
