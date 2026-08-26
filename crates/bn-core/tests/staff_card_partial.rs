//! One malformed item must not destroy an otherwise good staff brief.
//!
//! Regression for the repeated `staff-chief-am` / `staff-chief-pm` failures: a single item whose
//! kind fell outside the agent's sink allowlist rejected the entire card, losing the headline, the
//! body, and every valid item alongside it.

use bn_core::staff::validate_card;

const AGENT: &str = "staff-chief-am";
const TODAY: &str = "2026-08-26";

fn sinks() -> Vec<String> {
    vec!["attn".to_string(), "daily".to_string()]
}

fn card_json(items: &str) -> String {
    format!(
        r#"{{"agent":"{AGENT}","date":"{TODAY}","headline":"Morning brief",
            "body_md":"the useful part","items":[{items}]}}"#
    )
}

/// The exact shape that was failing: a `todo` item (sink `note`) beside valid `attn` items.
#[test]
fn an_item_outside_the_sink_allowlist_is_dropped_not_fatal() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"warn","title":"real risk","link":"","repo":"","branch":""},
           {"kind":"todo","severity":"warn","title":"not allowed here","link":"","repo":"","branch":""}"#,
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY).expect("card must survive");

    assert_eq!(card.headline, "Morning brief", "headline must be kept");
    assert_eq!(card.items.len(), 1, "only the invalid item is dropped");
    assert_eq!(card.items[0].title, "real risk");
    assert!(
        card.body_md.contains("Dropped 1 invalid item"),
        "the loss must be visible in the brief, got: {}",
        card.body_md
    );
}

/// An invalid severity is likewise an item-level problem.
#[test]
fn an_invalid_severity_drops_only_that_item() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"nonsense","title":"bad","link":"","repo":"","branch":""},
           {"kind":"attn","severity":"urgent","title":"good","link":"","repo":"","branch":""}"#,
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY).expect("card must survive");
    assert_eq!(card.items.len(), 1);
    assert_eq!(card.items[0].title, "good");
}

/// A fully valid card must be untouched — no spurious "dropped" note.
#[test]
fn a_valid_card_is_unchanged() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"info","title":"fine","link":"","repo":"","branch":""}"#,
    );

    let card = validate_card(&raw, AGENT, &sinks(), &[], TODAY).expect("valid card");
    assert_eq!(card.items.len(), 1);
    assert_eq!(card.body_md, "the useful part");
}

/// Card-level failures must still reject outright: a wrong agent means the whole document is wrong,
/// which is a different class of problem from one bad item.
#[test]
fn a_card_level_failure_still_rejects() {
    let raw = card_json(
        r#"{"kind":"attn","severity":"info","title":"fine","link":"","repo":"","branch":""}"#,
    )
    .replace(AGENT, "someone-else");

    assert!(validate_card(&raw, AGENT, &sinks(), &[], TODAY).is_err());
}
