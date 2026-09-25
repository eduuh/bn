//! `bn schema` — publish the `--json` contract (see [`bn_core::schema`]).
use crate::*;
use bn_core::schema::{self, CommandSchema};

pub(crate) fn emit_schema(name: Option<String>, json: bool) -> i32 {
    match name {
        Some(name) => one(&name, json),
        None => all(json),
    }
}

fn entry_json(c: &CommandSchema) -> serde_json::Value {
    serde_json::json!({
        "command": c.tag,
        "invocation": c.invocation,
        "summary": c.summary,
        "fields": c.fields,
    })
}

fn all(json: bool) -> i32 {
    if json {
        json_ok(
            "schema",
            serde_json::json!({
                "version": bn_core::render::ENVELOPE_VERSION,
                "envelope": schema::ENVELOPE_FIELDS,
                "commands": schema::COMMANDS.iter().map(entry_json).collect::<Vec<_>>(),
            }),
        );
        return 0;
    }
    println!(
        "bn --json envelope, contract version {}\n",
        bn_core::render::ENVELOPE_VERSION
    );
    for field in schema::ENVELOPE_FIELDS {
        println!("  {field}");
    }
    println!("\nDocumented commands ({}):\n", schema::COMMANDS.len());
    for c in schema::COMMANDS {
        println!("  {:<16} {}", c.tag, c.summary);
    }
    println!("\n`bn schema <command>` prints one command's data shape.");
    0
}

fn one(name: &str, json: bool) -> i32 {
    // Accept the envelope tag or the bare subcommand that produces it, so `bn schema wt list`'s
    // tag (`wt-list`) and a user's guess (`wt-list` vs `wt`) both land.
    let Some(c) = schema::find(name).or_else(|| {
        schema::COMMANDS
            .iter()
            .find(|c| c.tag.split('-').next() == Some(name))
    }) else {
        fail_json(
            json,
            "schema",
            "unknown_command",
            &format!("'{name}' is not documented. `bn schema` lists what is."),
        );
    };
    if json {
        json_ok("schema", entry_json(c));
        return 0;
    }
    println!("{}  —  {}", c.tag, c.summary);
    println!("{}\n", c.invocation);
    for field in c.fields {
        println!("  data.{field}");
    }
    0
}
