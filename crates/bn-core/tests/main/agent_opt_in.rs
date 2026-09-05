//! The opt-in gate, once, for every agent that declares one.
//!
//! It used to be seven copies of `if !config.<agent>_enabled { print; return 0 }`, each inside a
//! native job named after its agent — which is precisely why an agent could not be defined by its
//! manifest alone. The switch is now a manifest field resolved against one list.

use bn_core::agents::{parse_agent, AgentDef};
use bn_core::resolver::JobScope;
use std::path::PathBuf;

fn agent(name: &str, extra: &str) -> AgentDef {
    let text = format!("---\nname: {name}\nkind: command\n{extra}description: \"x\"\n---\nbody\n");
    parse_agent(&text, name, JobScope::Global, PathBuf::from("/tmp/x.md")).expect("parses")
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn an_agent_that_is_not_opt_in_always_runs() {
    let a = agent("standup", "");
    assert!(!a.opt_in);
    assert!(a.switched_on(&[]));
}

#[test]
fn an_opt_in_agent_stays_off_until_it_is_named() {
    let a = agent("staff-mentor", "opt_in: true\n");
    assert!(a.opt_in);
    assert!(!a.switched_on(&[]));
    assert!(!a.switched_on(&names(&["sentinel"])));
    assert!(a.switched_on(&names(&["sentinel", "staff-mentor"])));
}

/// The list is hand-written config, and `Staff-Mentor ` plainly means the same agent.
#[test]
fn the_enabled_list_is_forgiving_about_case_and_padding() {
    let a = agent("staff-mentor", "opt_in: true\n");
    assert!(a.switched_on(&names(&["  Staff-Mentor  "])));
}

/// Naming an agent that is not opt-in is harmless, not an error — a person listing everything they
/// want running should not have to know which ones needed listing.
#[test]
fn naming_a_non_opt_in_agent_changes_nothing() {
    let a = agent("standup", "");
    assert!(a.switched_on(&names(&["standup"])));
}
