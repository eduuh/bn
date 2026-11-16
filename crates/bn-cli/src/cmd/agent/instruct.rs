//! Task-owned execution instructions, recorded before delivery to the live agent.
use crate::*;

fn directive_id(text: &str) -> String {
    format!("d{}", &bn_core::task_doc::version_of(text)[..8])
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_instruct(
    cfg: &Config,
    target: &str,
    text: &str,
    no_send: bool,
    list: bool,
    drop: Option<&str>,
    as_json: bool,
) {
    let (repo, branch) = target
        .split_once(':')
        .unwrap_or_else(|| fail("bn agent instruct: expected repo:branch"));
    let task = bn_core::pipeline::gate::governing_task(cfg, repo, branch)
        .unwrap_or_else(|| fail("bn agent instruct: capture or claim a task first"));
    bn_core::task_doc::ensure(cfg, &task)
        .unwrap_or_else(|error| fail(&format!("bn agent instruct: {error}")));
    let doc = bn_core::task_doc::read(cfg, &task)
        .unwrap_or_else(|| fail("bn agent instruct: cannot read task document"));
    let mut items: Vec<String> = doc
        .section("Directives")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("- ").map(str::to_string))
        .collect();
    if list {
        let rows: Vec<_> = items
            .iter()
            .map(|text| serde_json::json!({"id": directive_id(text), "text": text}))
            .collect();
        if as_json {
            println!(
                "{}",
                Envelope::success(
                    "agent_instruct",
                    serde_json::json!({"target": target, "directives": rows, "count": rows.len()})
                )
                .to_json()
            );
        } else {
            for text in &items {
                println!("[{}] {text}", directive_id(text));
            }
        }
        return;
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (id, changed) = if let Some(id) = drop {
        let before = items.len();
        items.retain(|text| directive_id(text) != id);
        if before == items.len() {
            fail("bn agent instruct: unknown directive");
        }
        (id.to_string(), true)
    } else {
        if text.is_empty() {
            fail("bn agent instruct: an instruction is required");
        }
        let changed = !items.contains(&text);
        if changed {
            items.push(text.clone());
        }
        (directive_id(&text), changed)
    };
    if changed {
        let body = items
            .iter()
            .map(|text| format!("- {text}"))
            .collect::<Vec<_>>()
            .join("\n");
        let updated = bn_core::render::set_section(&doc.text, "## Directives", &body);
        bn_core::task_doc::write(cfg, &task, updated, Some(&doc.version()))
            .unwrap_or_else(|error| fail(&format!("bn agent instruct: {error}")));
    }
    let delivered = if no_send || drop.is_some() {
        None
    } else {
        Some(super::deliver_instruction(
            cfg,
            target,
            &format!("Task instruction (recorded in the task document under Directives): {text}"),
        ))
    };
    if as_json {
        println!("{}", Envelope::success("agent_instruct", serde_json::json!({
            "target": target, "id": id, "text": text, "recorded": changed, "delivered": delivered,
        })).to_json());
    } else {
        println!(
            "{} [{id}]",
            if drop.is_some() {
                "Dropped"
            } else {
                "Recorded"
            }
        );
        if delivered == Some(false) {
            eprintln!("bn agent instruct: recorded, but no live session accepted delivery");
        }
    }
}
