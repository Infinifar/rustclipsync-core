use rustclipsync_core::state;

#[test]
fn reads_state_written_by_unmodified_omaclipsync_020() {
    let bytes = include_bytes!("fixtures/omaclipsync-0.2.0.state");
    let saved = state::open(&[0x42; 32], bytes.to_vec()).unwrap();
    assert_eq!(saved.server, "https://fixture.example.test");
    assert_eq!(saved.token, "11".repeat(32));
    assert!(saved.paired);
    assert_eq!(saved.state.cursor, 4);
    assert_eq!(
        saved.state.device_id,
        "22222222-2222-4222-8222-222222222222"
    );
    let pending = saved.state.pending.as_ref().unwrap();
    assert!(pending.attempted);
    assert_eq!(pending.base_revision, 4);
    assert_eq!(pending.copy.text, "fixture only: 中文 😀");
    assert!(pending.copy.sensitive);
    assert_eq!(
        saved.state.queued.as_ref().unwrap().text,
        "last offline copy"
    );
    // State continues through the unchanged retry/cancellation protocol.
    let mut next = saved.clone();
    next.state.committed(&pending.copy.id, 5).unwrap();
    assert_eq!(next.state.pending.as_ref().unwrap().base_revision, 5);
    assert_eq!(
        next.state.pending.as_ref().unwrap().copy.text,
        "last offline copy"
    );
    assert_eq!(
        state::open(&[0x42; 32], state::seal(&[0x42; 32], &next).unwrap()).unwrap(),
        next
    );
    assert!(state::open(&[0x43; 32], bytes.to_vec()).is_err());
}
