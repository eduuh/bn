use super::target_slots;

#[test]
fn exact_slot_selection_is_validated() {
    assert_eq!(target_slots(4, None).unwrap(), vec![1, 2, 3, 4]);
    assert_eq!(target_slots(4, Some(2)).unwrap(), vec![2]);
    assert!(target_slots(4, Some(0)).is_err());
    assert!(target_slots(4, Some(5)).is_err());
}
