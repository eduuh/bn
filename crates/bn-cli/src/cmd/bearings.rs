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

#[cfg(test)]
mod tests {
    use super::*;
    use bn_core::agent::Fleet;

    fn snapshot() -> BearingsSnapshot {
        BearingsSnapshot::from_parts(100, &Fleet::default(), &[], &[])
    }

    #[test]
    fn markdown_output_uses_the_shared_renderer() {
        let snapshot = snapshot();
        assert_eq!(
            render_bearings(&snapshot, false),
            snapshot.render_markdown()
        );
    }

    #[test]
    fn json_output_wraps_the_same_snapshot_in_the_standard_envelope() {
        let snapshot = snapshot();
        let output: serde_json::Value =
            serde_json::from_str(&render_bearings(&snapshot, true)).unwrap();

        assert_eq!(output["command"], "bearings");
        assert_eq!(output["data"], payload::bearings_data(&snapshot));
    }
}
