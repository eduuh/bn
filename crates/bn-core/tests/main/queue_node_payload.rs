use bn_core::hosts::{NodeRecord, NodeStatus};
use bn_core::payload::queue::node_data;
use bn_core::worker::capacity::Capacity;

#[test]
fn node_payload_exposes_capacity_and_scheduler_headroom() {
    let node = NodeRecord {
        node_id: "worker-a".to_string(),
        labels: vec!["skill:review".to_string(), "os:linux".to_string()],
        owned_repos: vec!["bn".to_string()],
        capacity: Some(Capacity {
            mem_total_mb: Some(32_000),
            mem_available_mb: Some(20_000),
            swap_total_mb: Some(8_000),
            swap_free_mb: Some(7_000),
            loadavg1: Some(1.5),
            nproc: 8,
            disk_avail_mb: Some(120_000),
        }),
        active_attempts: 1,
        last_heartbeat: 990,
        status: NodeStatus::Ready,
        slots: 3,
        ..NodeRecord::default()
    };

    let value = node_data(&node, 1_000);

    assert_eq!(value["status"], "ready");
    assert_eq!(value["heartbeat_age_secs"], 10);
    assert_eq!(value["free"], 2);
    assert_eq!(value["capacity"]["mem_available_mb"], 20_000);
    assert_eq!(value["owned_repos"][0], "bn");
    assert_eq!(value["skills"][0], "review");
}

#[test]
fn a_lapsed_heartbeat_is_offline_without_losing_the_reported_state() {
    let node = NodeRecord {
        node_id: "worker-a".to_string(),
        last_heartbeat: 1,
        status: NodeStatus::Ready,
        ..NodeRecord::default()
    };

    let value = node_data(&node, 1_000);

    assert_eq!(value["status"], "offline");
    assert_eq!(value["reported_status"], "ready");
    assert_eq!(value["stale"], true);
}
