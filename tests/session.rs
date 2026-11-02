use rustclipsync_core::{
    adapters::{Clipboard, ClipboardError, NoHistory, StateStore},
    client::{Registration, RemoteState},
    direction::SyncDirection,
    session::{Session, SessionConfig, SyncError},
    state::{self, Ack, Message, Outbox, Persisted},
    transport::Transport,
    Error, Filter, FilterMode, Outcome, MAX_TEXT_BYTES,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

#[derive(Clone, Default)]
struct MemoryStore {
    bytes: Rc<RefCell<Option<Vec<u8>>>>,
    fail: Rc<Cell<bool>>,
    saves: Rc<Cell<usize>>,
    fail_on_save: Rc<Cell<Option<usize>>>,
}
impl StateStore for MemoryStore {
    fn load(&self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.bytes.borrow().clone())
    }
    fn save(&self, bytes: &[u8]) -> io::Result<()> {
        self.saves.set(self.saves.get() + 1);
        if self.fail.get() || self.fail_on_save.get() == Some(self.saves.get()) {
            return Err(io::Error::other("fixture disk failure"));
        }
        *self.bytes.borrow_mut() = Some(bytes.to_vec());
        Ok(())
    }
}
#[derive(Clone, Default)]
struct MemoryClipboard {
    value: Rc<RefCell<Option<(String, bool)>>>,
    writes: Rc<Cell<usize>>,
    fail: Rc<Cell<bool>>,
    fail_after_write: Rc<Cell<bool>>,
    snapshots: Rc<Cell<usize>>,
    fail_on_snapshot: Rc<Cell<Option<usize>>>,
}
impl Clipboard for MemoryClipboard {
    fn snapshot(&self, allow: bool) -> Result<Option<(String, bool)>, ClipboardError> {
        self.snapshots.set(self.snapshots.get() + 1);
        if self.fail_on_snapshot.get() == Some(self.snapshots.get()) {
            return Err(ClipboardError::Unavailable);
        }
        if !allow && self.value.borrow().as_ref().is_some_and(|v| v.1) {
            return Err(ClipboardError::Sensitive);
        }
        Ok(self.value.borrow().clone())
    }
    fn write(&self, text: &str, _: &str, sensitive: bool) -> Result<(), ClipboardError> {
        if self.fail.get() {
            return Err(ClipboardError::Unavailable);
        }
        self.writes.set(self.writes.get() + 1);
        *self.value.borrow_mut() = Some((text.into(), sensitive));
        if self.fail_after_write.get() {
            return Err(ClipboardError::Timeout);
        }
        Ok(())
    }
}
struct RemoteData {
    epoch: String,
    revision: u64,
    ack: u64,
    current: Option<(Message, String)>,
    commits: HashMap<String, u64>,
    sends: Vec<Outbox>,
    acknowledgements: Vec<Ack>,
    calls: usize,
}
struct Remote {
    data: Mutex<RemoteData>,
    lose_response: AtomicBool,
    change_during_download: AtomicBool,
    generation: Arc<AtomicU64>,
    control_log: Mutex<Vec<&'static str>>,
    fail_report: AtomicBool,
    fail_revoke: AtomicBool,
    hide_delivery: AtomicBool,
}
impl Remote {
    fn new(generation: Arc<AtomicU64>) -> Self {
        Self {
            data: Mutex::new(RemoteData {
                epoch: state::random_uuid().unwrap(),
                revision: 0,
                ack: 0,
                current: None,
                commits: HashMap::new(),
                sends: vec![],
                acknowledgements: vec![],
                calls: 0,
            }),
            lose_response: AtomicBool::new(false),
            change_during_download: AtomicBool::new(false),
            generation,
            control_log: Mutex::new(Vec::new()),
            fail_report: AtomicBool::new(false),
            fail_revoke: AtomicBool::new(false),
            hide_delivery: AtomicBool::new(false),
        }
    }
    fn publish(&self, text: &str, sensitive: bool) {
        let mut d = self.data.lock().unwrap();
        d.revision += 1;
        d.current = Some((
            Message {
                id: state::random_uuid().unwrap(),
                revision: d.revision,
                source_device_id: state::random_uuid().unwrap(),
                sensitive,
            },
            text.into(),
        ));
    }
}
impl Transport for Remote {
    fn set_sync_direction(&self, s: &Persisted, _: SyncDirection, _: bool) -> Result<(), Error> {
        self.control_log
            .lock()
            .unwrap()
            .push(if s.paired { "update" } else { "configure" });
        if s.paired && self.fail_report.load(Ordering::SeqCst) {
            Err(Error::Status(503))
        } else {
            Ok(())
        }
    }
    fn ensure_sync_direction(&self, _: &Persisted) -> Result<(), Error> {
        self.control_log.lock().unwrap().push("ack");
        if self.fail_report.load(Ordering::SeqCst) {
            Err(Error::Status(503))
        } else {
            Ok(())
        }
    }
    fn revoke_sync_direction(&self, s: &Persisted) -> Result<(), Error> {
        if !s.paired {
            return Ok(());
        }
        self.control_log.lock().unwrap().push("revoke");
        if self.fail_revoke.load(Ordering::SeqCst) {
            Err(Error::Status(503))
        } else {
            Ok(())
        }
    }
    fn register(&self, _: &Persisted) -> Result<Registration, Error> {
        let mut d = self.data.lock().unwrap();
        d.calls += 1;
        d.ack = d.revision;
        Ok(Registration {
            epoch: d.epoch.clone(),
            ack_revision: d.ack,
            current_revision: d.revision,
        })
    }
    fn state(&self, _: &Persisted) -> Result<RemoteState, Error> {
        self.control_log.lock().unwrap().push("state");
        let mut d = self.data.lock().unwrap();
        d.calls += 1;
        Ok(RemoteState {
            epoch: d.epoch.clone(),
            revision: d.revision,
            ack_revision: d.ack,
            delivery_revision: d
                .current
                .as_ref()
                .filter(|_| !self.hide_delivery.load(Ordering::SeqCst))
                .map(|v| v.0.revision),
            message_id: d
                .current
                .as_ref()
                .filter(|_| !self.hide_delivery.load(Ordering::SeqCst))
                .map(|v| v.0.id.clone()),
            source_device_id: d
                .current
                .as_ref()
                .filter(|_| !self.hide_delivery.load(Ordering::SeqCst))
                .map(|v| v.0.source_device_id.clone()),
            size: d
                .current
                .as_ref()
                .filter(|_| !self.hide_delivery.load(Ordering::SeqCst))
                .map(|v| v.1.len() as u64),
            sensitive: d
                .current
                .as_ref()
                .filter(|_| !self.hide_delivery.load(Ordering::SeqCst))
                .map(|v| v.0.sensitive),
        })
    }
    fn changes(&self, _: &Persisted) -> Result<Option<RemoteState>, Error> {
        panic!("not used by session")
    }
    fn content(&self, _: &Persisted, _: &Message) -> Result<String, Error> {
        if self.change_during_download.swap(false, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(self
            .data
            .lock()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .1
            .clone())
    }
    fn send(&self, s: &Persisted, o: &Outbox) -> Result<u64, Error> {
        let mut d = self.data.lock().unwrap();
        d.calls += 1;
        d.sends.push(o.clone());
        if let Some(revision) = d.commits.get(&o.copy.id) {
            return Ok(*revision);
        }
        if o.base_revision != d.revision {
            return Err(Error::Status(409));
        }
        d.revision += 1;
        let revision = d.revision;
        d.commits.insert(o.copy.id.clone(), revision);
        d.current = Some((
            Message {
                id: o.copy.id.clone(),
                revision,
                source_device_id: s.state.device_id.clone(),
                sensitive: o.copy.sensitive,
            },
            o.copy.text.clone(),
        ));
        if self.lose_response.swap(false, Ordering::SeqCst) {
            return Err(Error::Status(503));
        }
        Ok(revision)
    }
    fn cancel(&self, _: &Persisted, o: &Outbox) -> Result<Option<u64>, Error> {
        Ok(self.data.lock().unwrap().commits.get(&o.copy.id).copied())
    }
    fn acknowledge(&self, _: &Persisted, ack: &Ack) -> Result<(), Error> {
        let mut d = self.data.lock().unwrap();
        d.ack = d.ack.max(ack.revision);
        d.acknowledgements.push(ack.clone());
        Ok(())
    }
}
type TestSession = Session<MemoryClipboard, MemoryStore, NoHistory, Remote>;
struct Fixture {
    remote: Arc<Remote>,
    store: MemoryStore,
    clipboard: MemoryClipboard,
    generation: Arc<AtomicU64>,
    key: [u8; 32],
}
impl Fixture {
    fn new() -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        Self {
            remote: Arc::new(Remote::new(generation.clone())),
            store: MemoryStore::default(),
            clipboard: MemoryClipboard::default(),
            generation,
            key: state::random_key().unwrap(),
        }
    }
    fn open(&self) -> TestSession {
        self.open_sensitive(false)
    }
    fn open_sensitive(&self, allow_sensitive: bool) -> TestSession {
        Session::open(
            SessionConfig {
                server: "https://example.test".into(),
                encryption_key: self.key,
                filter: Filter::new(FilterMode::Off, &[], false).unwrap(),
                allow_sensitive,
            },
            self.remote.clone(),
            self.store.clone(),
            self.clipboard.clone(),
            NoHistory,
            self.generation.clone(),
        )
        .map(|mut s| {
            s.set_sync_direction(SyncDirection::Bidirectional, false)
                .unwrap();
            s
        })
        .unwrap()
    }
    fn copy(&self, session: &mut TestSession, text: &str) {
        *self.clipboard.value.borrow_mut() = Some((text.into(), false));
        session
            .record_local(text, None, false, Instant::now())
            .unwrap();
    }
}

#[test]
fn construction_has_no_network_or_clipboard_effect_and_pairing_ignores_old_value() {
    let f = Fixture::new();
    f.remote.publish("old remote", false);
    *f.clipboard.value.borrow_mut() = Some(("old local".into(), false));
    let mut s = f.open();
    assert_eq!(f.remote.data.lock().unwrap().calls, 0);
    assert!(!s.has_pending());
    s.check().unwrap();
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn offline_queue_keeps_only_latest_copy() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    for text in ["first", "second", "latest"] {
        f.copy(&mut s, text);
    }
    assert_eq!(s.flush(0).unwrap(), Outcome::Uploaded);
    let d = f.remote.data.lock().unwrap();
    assert_eq!(d.sends.len(), 1);
    assert_eq!(d.sends[0].copy.text, "latest");
}

#[test]
fn uncertain_upload_retries_same_id_without_second_revision() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "value");
    f.remote.lose_response.store(true, Ordering::SeqCst);
    assert!(s.flush(0).is_err());
    let pending = s.persisted.state.pending.clone().unwrap();
    drop(s);
    let mut s = f.open();
    s.check().unwrap();
    assert_eq!(s.flush(0).unwrap(), Outcome::Uploaded);
    let d = f.remote.data.lock().unwrap();
    assert_eq!(d.revision, 1);
    assert_eq!(d.sends.len(), 2);
    assert_eq!(d.sends[0], d.sends[1]);
    assert_eq!(d.sends[1].copy.id, pending.copy.id);
}

#[test]
fn newer_copy_resolves_uncertain_commit_before_upload() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "uncertain");
    f.remote.lose_response.store(true, Ordering::SeqCst);
    assert!(s.flush(0).is_err());
    f.copy(&mut s, "intermediate");
    f.copy(&mut s, "latest");
    assert_eq!(s.flush(0).unwrap(), Outcome::Current);
    assert_eq!(s.flush(0).unwrap(), Outcome::Uploaded);
    let d = f.remote.data.lock().unwrap();
    assert_eq!(d.sends.len(), 2);
    assert_eq!(d.sends[1].copy.text, "latest");
    assert_eq!(d.sends[1].base_revision, 1);
}

#[test]
fn conflict_does_not_silently_rebase_or_replace_clipboard() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "local");
    f.remote.publish("remote", false);
    assert!(matches!(s.flush(0), Err(SyncError::Conflict)));
    assert_eq!(s.persisted.state.pending.unwrap().base_revision, 0);
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn received_message_is_not_reuploaded_or_replayed_after_clear_and_restart() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("remote", false);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
    let id = s.persisted.state.ack.as_ref().unwrap().id.clone();
    assert_eq!(
        s.record_local("remote", Some(&id), false, Instant::now())
            .unwrap(),
        Outcome::Skipped
    );
    s.flush(0).unwrap();
    *f.clipboard.value.borrow_mut() = None;
    s.record_clear().unwrap();
    drop(s);
    let mut s = f.open();
    s.check().unwrap();
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert_eq!(f.clipboard.writes.get(), 1);
    assert!(f.remote.data.lock().unwrap().sends.is_empty());
}

#[test]
fn crash_during_apply_is_skipped_on_restart() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("remote", false);
    f.clipboard.fail.set(true);
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Clipboard(_))
    ));
    drop(s);
    f.clipboard.fail.set(false);
    let mut s = f.open();
    assert_eq!(s.persisted.state.ack.as_ref().unwrap().result, "skipped");
    s.check().unwrap();
    s.flush(0).unwrap();
    s.pull(0, Instant::now()).unwrap();
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn local_event_during_download_prevents_overwrite() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("remote", false);
    f.remote
        .change_during_download
        .store(true, Ordering::SeqCst);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Skipped);
    assert_eq!(s.persisted.state.cursor, 0);
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn sensitive_content_is_filtered_in_both_directions_by_default() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    assert_eq!(
        s.record_local("sensitive", None, true, Instant::now())
            .unwrap(),
        Outcome::Filtered
    );
    assert!(s.persisted.state.pending.is_none());
    f.remote.publish("sensitive", true);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Filtered);
    assert_eq!(s.persisted.state.ack.as_ref().unwrap().result, "skipped");
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn persistence_failure_prevents_upload_side_effect() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "value");
    f.store.fail.set(true);
    assert!(matches!(s.flush(0), Err(SyncError::Storage(_))));
    assert!(f.remote.data.lock().unwrap().sends.is_empty());
}

#[test]
fn persistence_failure_prevents_clipboard_side_effect() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("remote", false);
    f.store.fail.set(true);
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Storage(_))
    ));
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn cleared_pending_selection_does_not_upload_after_restart() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "old");
    drop(s);
    *f.clipboard.value.borrow_mut() = None;
    let mut s = f.open();
    s.check().unwrap();
    s.flush(0).unwrap();
    assert!(!s.has_pending());
    assert!(f.remote.data.lock().unwrap().sends.is_empty());
}

#[test]
fn epoch_change_requires_repair() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.data.lock().unwrap().epoch = state::random_uuid().unwrap();
    assert!(matches!(s.check(), Err(SyncError::Epoch)));
    assert_eq!(f.clipboard.writes.get(), 0);
}

#[test]
fn corrupted_state_fails_closed_instead_of_registering_new_device() {
    let f = Fixture::new();
    let s = f.open();
    drop(s);
    f.store.bytes.borrow_mut().as_mut().unwrap()[20] ^= 1;
    let result = Session::open(
        SessionConfig {
            server: "https://example.test".into(),
            encryption_key: f.key,
            filter: Filter::new(FilterMode::Off, &[], false).unwrap(),
            allow_sensitive: false,
        },
        f.remote.clone(),
        f.store,
        f.clipboard,
        NoHistory,
        f.generation,
    );
    assert!(matches!(result, Err(SyncError::Storage(_))));
    assert_eq!(f.remote.data.lock().unwrap().calls, 0);
}

#[test]
fn oversize_text_never_enters_outbox() {
    let f = Fixture::new();
    let mut s = f.open();
    assert!(matches!(
        s.record_local(&"x".repeat(MAX_TEXT_BYTES + 1), None, false, Instant::now()),
        Err(SyncError::Remote(Error::TooLarge))
    ));
    assert!(s.persisted.state.pending.is_none());
}

#[test]
fn private_sensitive_receive_is_durable_skipped_and_never_writes_clipboard() {
    let f = Fixture::new();
    let mut s = f.open_sensitive(true);
    s.check().unwrap();
    s.use_sensitive_inbox(true);
    f.remote.publish("sensitive fixture", true);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Stored);
    assert_eq!(f.clipboard.writes.get(), 0);
    assert_eq!(s.persisted.state.ack.as_ref().unwrap().result, "skipped");
    let inbox = s.persisted.inbox.clone().unwrap();
    assert_eq!(inbox.text, "sensitive fixture");
    assert!(s.persisted.notification_identity().inbox.is_none());
    let raw = f.store.bytes.borrow().clone().unwrap();
    assert!(!raw.windows(17).any(|x| x == b"sensitive fixture"));
    drop(s);
    let mut reopened = f.open_sensitive(true);
    reopened.use_sensitive_inbox(true);
    assert_eq!(reopened.persisted.inbox.as_ref(), Some(&inbox));
    reopened.flush(0).unwrap();
    assert_eq!(reopened.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert_eq!(f.clipboard.writes.get(), 0);
    assert!(!reopened.clear_inbox("stale-id").unwrap());
    assert!(reopened.clear_inbox(&inbox.id).unwrap());
    assert!(f.open_sensitive(true).persisted.inbox.is_none());
}
#[test]
fn failed_private_inbox_commit_does_not_ack_or_apply_and_can_recover() {
    let f = Fixture::new();
    let mut s = f.open_sensitive(true);
    s.check().unwrap();
    s.use_sensitive_inbox(true);
    f.remote.publish("sensitive fixture", true);
    f.store.fail.set(true);
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Storage(_))
    ));
    assert_eq!(f.clipboard.writes.get(), 0);
    assert!(f.remote.data.lock().unwrap().acknowledgements.is_empty());
    f.store.fail.set(false);
    drop(s);
    let mut s = f.open_sensitive(true);
    s.use_sensitive_inbox(true);
    assert!(s.persisted.inbox.is_none());
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Stored);
}

#[test]
fn regression_new_filter_applies_to_pending_upload() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "private-token");
    s.update_policy(
        Filter::new(FilterMode::Blacklist, &["private".into()], true).unwrap(),
        false,
    );
    s.flush(0).unwrap();
    assert!(
        f.remote.data.lock().unwrap().sends.is_empty(),
        "new blacklist must prevent previously queued matching text from being uploaded"
    );
}

#[test]
fn regression_clipboard_failure_does_not_wedge_future_pulls() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("first", false);
    f.clipboard.fail.set(true);
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Clipboard(_))
    ));
    f.clipboard.fail.set(false);
    f.remote.publish("second", false);
    let outcome = s.pull(0, Instant::now()).unwrap();
    assert_eq!(
        outcome,
        Outcome::Downloaded,
        "a later remote message must be receivable after the clipboard recovers"
    );
}

#[test]
fn regression_slow_pull_does_not_expire_echo_before_write() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("remote", false);
    let request_started = Instant::now() - std::time::Duration::from_secs(6);
    assert_eq!(s.pull(0, request_started).unwrap(), Outcome::Downloaded);
    let id = s.persisted.state.ack.as_ref().unwrap().id.clone();
    let outcome = s
        .record_local("remote", Some(&id), false, Instant::now())
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::Skipped,
        "echo window must start when clipboard is written, rather than when network request began"
    );
}

#[test]
fn regression_oversize_new_copy_does_not_send_old_copy() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "old");
    let oversize = "x".repeat(MAX_TEXT_BYTES + 1);
    *f.clipboard.value.borrow_mut() = Some((oversize.clone(), false));
    assert!(matches!(
        s.record_local(&oversize, None, false, Instant::now()),
        Err(SyncError::Remote(Error::TooLarge))
    ));
    s.flush(0).unwrap();
    assert!(
        f.remote.data.lock().unwrap().sends.is_empty(),
        "the superseded clipboard value must not be uploaded"
    );
}

#[test]
fn regression_reopened_paired_session_checks_pending_before_flush() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "old");
    drop(s);
    *f.clipboard.value.borrow_mut() = None;
    let mut reopened = f.open();
    reopened.flush(0).unwrap();
    assert!(
        f.remote.data.lock().unwrap().sends.is_empty(),
        "flush must check restored pending against current clipboard even when already paired"
    );
}

#[test]
fn policy_change_resolves_uncertain_upload_without_resending_filtered_text() {
    for committed in [false, true] {
        for latest_allowed in [false, true] {
            let f = Fixture::new();
            let mut s = f.open();
            s.check().unwrap();
            f.copy(&mut s, "private-old");
            if committed {
                f.remote.lose_response.store(true, Ordering::SeqCst);
                assert!(s.flush(0).is_err());
            } else {
                // Persist an attempted request without sending it, as if it
                // never reached the server before cancellation became necessary.
                assert_eq!(s.flush(1).unwrap(), Outcome::Skipped);
            }
            let sends_before = f.remote.data.lock().unwrap().sends.len();
            let latest = if latest_allowed {
                "public-new"
            } else {
                "private-new"
            };
            f.copy(&mut s, latest);
            s.update_policy(
                Filter::new(FilterMode::Blacklist, &["private".into()], false).unwrap(),
                false,
            );
            assert_eq!(s.flush(0).unwrap(), Outcome::Current);
            assert_eq!(f.remote.data.lock().unwrap().sends.len(), sends_before);
            assert_eq!(
                s.flush(0).unwrap(),
                if latest_allowed {
                    Outcome::Uploaded
                } else {
                    Outcome::Current
                }
            );
            let d = f.remote.data.lock().unwrap();
            assert_eq!(
                d.sends.len(),
                sends_before + usize::from(latest_allowed),
                "committed={committed}, latest_allowed={latest_allowed}"
            );
            if latest_allowed {
                assert_eq!(d.sends.last().unwrap().copy.text, "public-new");
            }
            assert!(s.persisted.state.pending.is_none());
        }
    }
}

#[test]
fn oversize_copy_cancels_uncertain_upload_and_survives_restart() {
    for committed in [false, true] {
        let f = Fixture::new();
        let mut s = f.open();
        s.check().unwrap();
        f.copy(&mut s, "old");
        if committed {
            f.remote.lose_response.store(true, Ordering::SeqCst);
            assert!(s.flush(0).is_err());
        } else {
            assert_eq!(s.flush(1).unwrap(), Outcome::Skipped);
        }
        f.copy(&mut s, "queued");
        let sends_before = f.remote.data.lock().unwrap().sends.len();
        assert!(matches!(
            s.record_local(&"x".repeat(MAX_TEXT_BYTES + 1), None, false, Instant::now()),
            Err(SyncError::Remote(Error::TooLarge))
        ));
        drop(s);
        let mut s = f.open();
        s.flush(0).unwrap();
        s.flush(0).unwrap();
        assert!(!s.has_pending(), "committed={committed}");
        assert_eq!(f.remote.data.lock().unwrap().sends.len(), sends_before);
    }
}

#[test]
fn oversize_copy_reports_storage_failure_if_discard_cannot_be_persisted() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "old");
    f.store.fail.set(true);
    assert!(matches!(
        s.record_local(&"x".repeat(MAX_TEXT_BYTES + 1), None, false, Instant::now()),
        Err(SyncError::Storage(_))
    ));
    assert!(f.remote.data.lock().unwrap().sends.is_empty());
}

#[test]
fn ambiguous_write_is_skipped_durably_without_replay_in_same_session() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("ambiguous", false);
    f.clipboard.fail_after_write.set(true);
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Clipboard(ClipboardError::Timeout))
    ));
    f.clipboard.fail_after_write.set(false);
    s.flush(0).unwrap();
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert_eq!(f.clipboard.writes.get(), 1);
    assert_eq!(
        f.remote.data.lock().unwrap().acknowledgements[0].result,
        "skipped"
    );
    drop(s);
    let mut s = f.open();
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert_eq!(f.clipboard.writes.get(), 1);
}

#[test]
fn failed_write_recovery_save_reports_storage_and_preserves_crash_journal() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("ambiguous", false);
    f.clipboard.fail_after_write.set(true);
    f.store.fail_on_save.set(Some(f.store.saves.get() + 2));
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Storage(_))
    ));
    assert!(f.remote.data.lock().unwrap().acknowledgements.is_empty());
    drop(s);
    f.clipboard.fail_after_write.set(false);
    f.store.fail_on_save.set(None);
    let mut s = f.open();
    s.flush(0).unwrap();
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert_eq!(f.clipboard.writes.get(), 1);
    assert_eq!(
        f.remote.data.lock().unwrap().acknowledgements[0].result,
        "skipped"
    );
}

#[test]
fn final_snapshot_failure_releases_journal_without_skipping_unwritten_message() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("retry me", false);
    f.clipboard
        .fail_on_snapshot
        .set(Some(f.clipboard.snapshots.get() + 3));
    assert!(matches!(
        s.pull(0, Instant::now()),
        Err(SyncError::Clipboard(_))
    ));
    assert_eq!(f.clipboard.writes.get(), 0);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
    assert_eq!(f.clipboard.writes.get(), 1);
}

#[test]
fn restored_matching_pending_is_sent_without_explicit_check() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "still current");
    let id = s.persisted.state.pending.as_ref().unwrap().copy.id.clone();
    drop(s);
    let mut s = f.open();
    assert_eq!(s.flush(0).unwrap(), Outcome::Uploaded);
    assert_eq!(f.remote.data.lock().unwrap().sends[0].copy.id, id);
}

#[test]
fn restored_stale_pending_does_not_block_direct_pull() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "old local");
    drop(s);
    *f.clipboard.value.borrow_mut() = None;
    f.remote.publish("new remote", false);
    let mut s = f.open();
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
    assert!(f.remote.data.lock().unwrap().sends.is_empty());
}

#[test]
fn background_inbox_never_uses_clipboard_and_preserves_ordinary_receive() {
    let f = Fixture::new();
    let mut s = f.open_sensitive(true);
    s.use_sensitive_inbox(true);
    s.check().unwrap();
    let reads = f.clipboard.snapshots.get();
    f.clipboard.fail_on_snapshot.set(Some(reads + 1));
    f.clipboard.fail.set(true);
    f.remote.publish("synthetic background secret", true);
    assert_eq!(s.pull_inbox(0).unwrap(), Outcome::Stored);
    assert_eq!(f.clipboard.snapshots.get(), reads);
    assert_eq!(f.clipboard.writes.get(), 0);
    assert_eq!(
        s.persisted.inbox.as_ref().unwrap().text,
        "synthetic background secret"
    );
    s.flush(0).unwrap();
    assert_eq!(
        f.remote.data.lock().unwrap().acknowledgements[0].result,
        "skipped"
    );
    assert!(!f
        .store
        .bytes
        .borrow()
        .as_ref()
        .unwrap()
        .windows(b"synthetic background secret".len())
        .any(|bytes| bytes == b"synthetic background secret"));
    let cursor = s.persisted.state.cursor;
    f.remote.publish("ordinary foreground value", false);
    assert_eq!(s.pull_inbox(0).unwrap(), Outcome::Skipped);
    assert_eq!(s.persisted.state.cursor, cursor);
    assert_eq!(f.clipboard.snapshots.get(), reads);
    f.clipboard.fail_on_snapshot.set(None);
    f.clipboard.fail.set(false);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
}

#[test]
fn background_inbox_requires_opt_in_and_foreground_restore_check() {
    let f = Fixture::new();
    let mut s = f.open_sensitive(true);
    s.use_sensitive_inbox(true);
    assert_eq!(s.pull_inbox(0).unwrap(), Outcome::Skipped);
    assert_eq!(f.remote.data.lock().unwrap().calls, 0);
    s.check().unwrap();
    f.remote.publish("private fixture", true);
    s.use_sensitive_inbox(false);
    assert_eq!(s.pull_inbox(0).unwrap(), Outcome::Skipped);
    assert_eq!(s.persisted.state.cursor, 0);
    s.use_sensitive_inbox(true);
    f.copy(&mut s, "pending fixture");
    drop(s);
    let reads = f.clipboard.snapshots.get();
    let mut restored = f.open_sensitive(true);
    restored.use_sensitive_inbox(true);
    assert_eq!(restored.pull_inbox(0).unwrap(), Outcome::Skipped);
    assert_eq!(f.clipboard.snapshots.get(), reads);
    assert!(restored.persisted.state.pending.is_some());
}

#[test]
fn background_inbox_aborts_when_lifecycle_changes_during_download() {
    let f = Fixture::new();
    let mut s = f.open_sensitive(true);
    s.use_sensitive_inbox(true);
    s.check().unwrap();
    f.remote.publish("synthetic revoked preview", true);
    f.remote
        .change_during_download
        .store(true, Ordering::SeqCst);
    assert_eq!(s.pull_inbox(0).unwrap(), Outcome::Skipped);
    assert!(s.persisted.inbox.is_none());
    assert_eq!(s.persisted.state.cursor, 0);
}

#[test]
fn background_write_opt_in_never_reads_and_preserves_sensitive_policy() {
    for (allow_sensitive, inbox, expected, writes) in [
        (false, false, Outcome::Filtered, 0),
        (true, true, Outcome::Stored, 0),
        (true, false, Outcome::Downloaded, 1),
    ] {
        let f = Fixture::new();
        let mut s = f.open_sensitive(allow_sensitive);
        s.use_sensitive_inbox(inbox);
        s.check().unwrap();
        let reads = f.clipboard.snapshots.get();
        f.clipboard.fail_on_snapshot.set(Some(reads + 1));
        f.remote.publish("synthetic marked background value", true);
        assert_eq!(s.pull_background(0, true).unwrap(), expected);
        assert_eq!(f.clipboard.writes.get(), writes);
        f.remote
            .publish("synthetic ordinary background value", false);
        assert_eq!(s.pull_background(0, true).unwrap(), Outcome::Downloaded);
        assert_eq!(f.clipboard.writes.get(), writes + 1);
        assert_eq!(f.clipboard.snapshots.get(), reads);
    }
}

#[test]
fn direction_must_be_acknowledged_before_metadata_and_unknown_fails_closed() {
    let f = Fixture::new();
    let mut s = f.open();
    f.remote.fail_report.store(true, Ordering::SeqCst);
    assert!(matches!(
        s.check(),
        Err(SyncError::Remote(Error::Status(503)))
    ));
    assert!(
        !f.remote.control_log.lock().unwrap().contains(&"state"),
        "no metadata before ACK"
    );
    f.remote.fail_report.store(false, Ordering::SeqCst);
    s.check().unwrap();
    let log = f.remote.control_log.lock().unwrap().clone();
    assert!(log.iter().position(|v| *v == "ack") < log.iter().position(|v| *v == "state"));
    s.revoke_sync_direction().unwrap();
    assert!(matches!(
        s.check(),
        Err(SyncError::Remote(Error::DirectionRequired))
    ));
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Paused);
    assert_eq!(
        s.record_local("never-send", None, false, Instant::now())
            .unwrap(),
        Outcome::Paused
    );
}

#[test]
fn paused_direction_reports_without_reading_metadata_or_clipboard() {
    let f = Fixture::new();
    let mut s = f.open();
    s.set_sync_direction(SyncDirection::Bidirectional, true)
        .unwrap();
    s.check().unwrap();
    assert!(s.persisted.paired);
    assert!(!f.remote.control_log.lock().unwrap().contains(&"state"));
    assert_eq!(f.clipboard.snapshots.get(), 0);
    assert_eq!(s.push_now(0).unwrap(), Outcome::Paused);
    assert_eq!(s.pull_background(0, true).unwrap(), Outcome::Paused);
}

#[test]
fn failed_direction_update_keeps_local_receive_and_upload_guards() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "queued-before-pause");
    f.remote.fail_report.store(true, Ordering::SeqCst);
    assert!(s
        .set_sync_direction(SyncDirection::Bidirectional, true)
        .is_err());
    assert!(s.persisted.state.pending.is_none());
    assert_eq!(s.push_now(0).unwrap(), Outcome::Paused);
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Paused);
    assert!(f.remote.data.lock().unwrap().sends.is_empty());
}

#[test]
fn receive_only_and_pause_resolve_uncertain_upload_by_cancel_only() {
    for (direction, paused) in [
        (SyncDirection::ReceiveOnly, false),
        (SyncDirection::Bidirectional, true),
    ] {
        let f = Fixture::new();
        let mut s = f.open();
        s.check().unwrap();
        f.copy(&mut s, "uncertain");
        f.remote.lose_response.store(true, Ordering::SeqCst);
        assert!(s.flush(0).is_err());
        s.set_sync_direction(direction, paused).unwrap();
        assert!(s.persisted.state.discard_pending);
        s.flush(0).unwrap();
        assert!(s.persisted.state.pending.is_none());
        assert_eq!(
            f.remote.data.lock().unwrap().sends.len(),
            1,
            "direction changes must never retry the body"
        );
    }
}

#[test]
fn absent_delivery_observes_global_revision_without_rebasing_existing_copy() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.copy(&mut s, "older-local");
    f.remote.publish("filtered-remote", false);
    f.remote.hide_delivery.store(true, Ordering::SeqCst);
    s.check().unwrap();
    assert_eq!(s.persisted.state.cursor, 1);
    assert_eq!(
        s.persisted.state.pending.as_ref().unwrap().base_revision,
        0,
        "existing copies retain their conflict base"
    );
    f.copy(&mut s, "real-new-copy-after-observation");
    assert_eq!(
        s.persisted.state.pending.as_ref().unwrap().base_revision,
        1,
        "a new copy may use the already observed revision"
    );
    assert_eq!(s.flush(0).unwrap(), Outcome::Uploaded);
}

#[test]
fn older_authorized_delivery_is_processed_before_advancing_to_global_revision() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    f.remote.publish("eligible-backfill", false);
    f.remote.data.lock().unwrap().revision = 9;
    assert_eq!(s.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
    assert_eq!(
        s.persisted.state.cursor, 1,
        "global metadata must not skip a valid older candidate"
    );
    assert_eq!(
        f.clipboard.value.borrow().as_ref().unwrap().0,
        "eligible-backfill"
    );
}

#[test]
fn repair_does_not_destroy_identity_when_old_direction_revocation_is_unconfirmed() {
    let f = Fixture::new();
    let mut s = f.open();
    s.check().unwrap();
    let old = s.persisted.clone();
    f.remote.fail_revoke.store(true, Ordering::SeqCst);
    assert!(s
        .re_pair_with_direction(SyncDirection::ReceiveOnly, false)
        .is_err());
    assert_eq!(s.persisted, old);
    f.remote.fail_revoke.store(false, Ordering::SeqCst);
    s.re_pair_with_direction(SyncDirection::ReceiveOnly, true)
        .unwrap();
    assert_ne!(s.persisted.state.device_id, old.state.device_id);
    assert_eq!(
        s.record_local("not-uploaded", None, false, Instant::now())
            .unwrap(),
        Outcome::Paused
    );
}
