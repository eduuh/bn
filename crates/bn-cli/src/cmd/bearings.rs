//! `bn bearings` — the bounded shared current-state snapshot.

use crate::*;
use bn_core::bearings::BearingsSnapshot;

pub(crate) fn emit_bearings(cfg: &Config, json: bool) {
    let snapshot = BearingsSnapshot::gather(
        cfg,
        bn_core::util::now_epoch_secs(),
        &bn_core::util::today(),
    );
    println!("{}", render_bearings(&snapshot, json));
}

fn render_bearings(snapshot: &BearingsSnapshot, json: bool) -> String {
    if json {
        Envelope::success("bearings", payload::bearings_data(snapshot)).to_json()
    } else {
        snapshot.render_markdown()
    }
}


