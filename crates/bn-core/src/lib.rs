#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::unwrap_used
    )
)]

//! bn-core — the engine. Never prints for humans; returns values.
//!
//! Phase 1 scope: notes core (model, store, resolver, id, render, commands).
//! Modules are filled in over the build order in phase1-plan.md.

pub mod action_center;
pub mod action_rank;
pub mod advise;
pub mod afk;
pub mod agent;
pub mod agents;
pub mod archivist;
pub mod attention;
pub mod autoreview;
pub mod backlog;
pub mod bearings;
pub mod bn_subprocess;
pub mod brag;
pub mod calib;
pub mod capture;
pub mod chief;
pub mod clock;
pub mod commands;
pub mod completed;
pub mod controller;
pub mod cost;
pub mod crons;
pub mod crossrepo;
pub mod curriculum;
pub mod daily;
pub mod doctor;
pub mod dojo;
pub mod events;
pub mod eye;
pub mod feed;
pub mod findings;
pub mod findings_gather;
pub mod fleet_gc;
pub mod focus;
pub mod forge;
pub mod gh_pin;
pub mod glyph;
pub mod goals_align;
pub mod handoff;
pub mod harness;
pub mod hosts;
pub mod id;
pub mod idle;
pub mod inbox;
pub mod insights;
pub mod learnings;
pub mod lifecycle;
pub mod log;
pub mod machine;
pub mod mcp;
pub mod meetings;
pub mod mentor;
pub mod metrics;
pub mod migrate;
pub mod model;
pub mod next;
pub mod night;
pub mod notify;
pub mod orchestrate;
pub mod palette;
pub mod payload;
pub mod pipeline;
pub mod plan_review;
pub mod pool;
pub mod pr_observe;
pub mod quarter;
pub mod queue_from;
pub mod recall;
pub mod references;
pub mod related;
pub mod release;
pub mod remote;
pub mod render;
pub mod repo_resolve;
pub mod reports;
pub mod resolver;
pub mod retro;
pub mod review_gate;
pub mod reviews;
pub mod rollups;
pub mod routine;
pub mod runner;
pub mod scheduler;
pub mod scout;
pub mod search;
pub mod select;
pub mod serve;
pub mod skills;
pub mod specialists;
pub mod staff;
pub mod staff_backpressure;
pub mod staff_retention;
pub mod staff_schedule;
pub mod standup;
pub mod store;
pub mod suggestions;
pub mod sync;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tmux;
pub mod today_agent;
pub mod todo;
pub mod tools;
pub mod track;
pub mod trust;
pub mod util;
pub mod verify_ui;
pub mod wake;
pub mod why;
pub mod worker;
