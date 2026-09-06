//! The "you have these tools" preamble prepended to a dispatched agent's prompt.
//!
//! An agent launched with skills or MCP servers has to be *told* they exist — the harness does not
//! advertise them — so this composes the one-paragraph preamble that names them. `None` when the
//! agent was given neither, so a plain prompt stays plain.
//!
//! Lived under the staff roster's prompt assembly; `bn agent run` still needs it.

pub fn equip_preamble(skills: &[String], mcps: &[String]) -> Option<String> {
    if skills.is_empty() && mcps.is_empty() {
        return None;
    }
    let mut out = String::from("## Equipped for this run\n\n");
    if !skills.is_empty() {
        out.push_str(
            "Skills available to you — each is loaded natively in your harness under the name below. Refer to a skill by its name when it applies; do not invoke it as a `/bn:<name>` or `/mcp.bn.<name>` slash-command prompt:\n",
        );
        for s in skills {
            out.push_str(&format!("- {s}\n"));
        }
        out.push('\n');
    }
    if !mcps.is_empty() {
        out.push_str("MCP servers/tools available to you:\n");
        for m in mcps {
            out.push_str(&format!("- {m}\n"));
        }
        out.push('\n');
    }
    Some(out)
}
