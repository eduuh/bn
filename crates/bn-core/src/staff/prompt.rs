//! What an agent is told: the dispatch prompt, and the equip preamble that seeds it.
//!
//! The third side of the contract, beside [`super::card`] (what the agent may say back) and
//! [`super::route`] (what saying it does). Keeping them apart matters because the output contract
//! advertised here and the validation enforced there must agree, and a disagreement is much easier
//! to spot between two short files than inside one long one.
//!
//! The packet is wrapped in an explicit untrusted-data frame exactly once, here — it is public
//! content a harness must read as data, never as instructions.
use super::*;

// --- prompt --------------------------------------------------------------------------------------

/// The full dispatch prompt: pinned skill, then the packet inside an explicit **untrusted-data
/// frame** (done once here — see [`PACKET_FRAME_HEADER`]), then the output contract. The frame is
/// the prompt-injection guard for the gather path: the packet is public, untrusted content and
/// must be read as data, never as instructions (#272). The contract advertises only the kinds this
/// agent's sink allowlist admits ([`kinds_for_sinks`]), so a scoped agent (e.g. a `[attn, daily]`
/// chief) is never invited to write a kind [`validate_card`] would then reject.
pub fn build_prompt(
    skill_body: &str,
    agent: &str,
    allowed_sinks: &[String],
    packet: &str,
    today: &str,
) -> String {
    build_prompt_with(skill_body, agent, allowed_sinks, packet, today, false, None)
}

/// [`build_prompt`], plus the `replace` half of the contract for an agent whose manifest declares
/// it. Kept a separate entry point so the common card — one that reports events as they happen —
/// is never told about a mechanism that would make it ack its own board.
#[allow(clippy::too_many_arguments)]
pub fn build_prompt_with(
    skill_body: &str,
    agent: &str,
    allowed_sinks: &[String],
    packet: &str,
    today: &str,
    replace: bool,
    max_severity: Option<&str>,
) -> String {
    let advertised = kinds_for_sinks(allowed_sinks);
    let kinds = advertised.join("|");
    // The skeleton shows `repo`/`branch` as empty strings, which reads as "optional" — and a model
    // that leaves them empty on a routed kind loses the *whole* card to [`validate_card`], headline
    // and body included. Spell the requirement out, but only when a routed kind is actually on
    // offer, so a `[attn, daily]` chief isn't handed a rule about fields it can never need.
    let routed: Vec<&str> = advertised
        .iter()
        .copied()
        .filter(|k| kind_needs_route(k))
        .collect();
    let route_rule = if routed.is_empty() {
        String::new()
    } else {
        format!(
            " Items of kind [{}] MUST also carry a non-empty `repo` and `branch` naming the branch \
             they land on — one that omits either is rejected and the entire card is discarded, so \
             emit such an item only when the packet tells you both.",
            routed.join("|")
        )
    };
    // A `key` only earns its place when it can outlive the run: it must name the *subject* the
    // packet identifies, not this run's wording of it, or refresh-in-place and `replace` both
    // degrade into minting a fresh item every time.
    let key_rule = if replace {
        "Every item MUST carry a stable `key` naming its subject as the packet identifies it \
         (never this run's wording), and the card MUST set \"replace\": true — it states your whole \
         current set, so anything you keyed before and now omit is treated as fixed and is \
         resolved. Omitting an item you can still see therefore closes it. "
    } else {
        "An item MAY carry a stable `key` naming its subject, which refreshes the same item in \
         place across runs instead of raising a second copy. "
    };
    let replace_field = if replace { " \"replace\": true," } else { "" };
    // The ceiling has to be *in* the contract, not just in the agent's prose. A model handed an
    // alarming packet reaches for the highest severity on offer, and offering `urgent` to an agent
    // whose whole discipline is "I summarize, I never escalate" is inviting it to break that rule.
    let severities = match max_severity {
        Some(max) => crate::attention::SEVERITIES
            .iter()
            .take_while(|s| **s != max)
            .chain(std::iter::once(&max))
            .copied()
            .collect::<Vec<_>>()
            .join("|"),
        None => crate::attention::SEVERITIES.join("|"),
    };
    let severity_rule = match max_severity {
        Some(max) => format!(
            " Nothing you raise may exceed severity `{max}` — you summarize, and urgency belongs to \
             whatever found the problem. Anything higher is clamped back down to `{max}`."
        ),
        None => String::new(),
    };
    format!(
        "{skill_body}\n\n{PACKET_FRAME_HEADER}\n{packet}\n{PACKET_FRAME_FOOTER}\n\n\
         Output EXACTLY one JSON briefing card and nothing else. Every item's `kind` MUST be one of \
         [{kinds}] — any other kind is rejected and the entire card is discarded.{route_rule}\
         {severity_rule} \
         {key_rule}\
         An `attn` item MAY set \"mode\": \"auto\" to offer itself to an unattended worker; that \
         requires a `prompt` saying what the worker should do, plus `repo` and `branch`. \
         `detail` carries longer context that does not belong in the one-line title. \
         A `dojo-card` MAY set `area` to the mastery area it drills. \
         A `doc` item is a whole file: its `key` is the filename (a plain name, no directories) \
         and its `body` the Markdown written verbatim; the directory is fixed by the agent. \
         A `note-append` may also use `body` for continuation lines, indented under its `title` — \
         `title` itself is one line, so anything multi-line put there arrives as one run-on line. \
         On a thin or degraded packet, still emit a valid card whose headline names the gap \
         (never invent findings to fill it).\n\
         {{ \"agent\": \"{agent}\", \"date\": \"{today}\",{replace_field} \"headline\": \"...\", \
         \"body_md\": \"...\", \"items\": [ {{ \"kind\": \"{kinds}\", \
         \"severity\": \"{severities}\", \"title\": \"...\", \"key\": \"...\", \
         \"detail\": \"\", \"link\": \"\", \"repo\": \"\", \
         \"branch\": \"\" }} ] }}\n"
    )
}

// --- built-in gatherers ---------------------------------------------------------------------------

/// The equip preamble (#568): a deterministic block prepended to the dispatch prompt that seeds the
/// launched agent with the reusable **skills** and **MCP servers** its definition declares. Staff
/// runs already dispatch with the harness's full tool surface (`--allow-all-tools`), so equipping is
/// done in-prompt — the agent is told which skills are available (loaded natively in its harness and
/// referred to by name) and which MCP servers/tools it should use. Returns `None` when the agent
/// declares neither (prompt unchanged for the common case). A skill is named by its file/skill name:
/// the agent refers to it by that name, never as a `/bn:<name>` or `/mcp.bn.<name>` slash-command
/// prompt.
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

/// The `bn staff gather demo` packet — the echo-grade demo agent's deterministic input.
pub fn demo_packet(cfg: &Config, today: &str) -> String {
    serde_json::json!({
        "agent": "demo",
        "date": today,
        "machine": cfg.machine(),
        "open_attention": crate::attention::open_items(today).len(),
    })
    .to_string()
}
