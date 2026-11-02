//! Disposable-server integration probe; never accesses an OS clipboard or keyring.
use rustclipsync_core::{
    adapters::{Clipboard, ClipboardError, NoHistory, StateStore},
    client::Client,
    direction::SyncDirection,
    session::{Session, SessionConfig, SyncError},
    state, Filter, FilterMode, Outcome,
};
use std::{
    cell::RefCell,
    io,
    rc::Rc,
    sync::{atomic::AtomicU64, Arc},
    time::Instant,
};

#[derive(Clone, Default)]
struct Memory(Rc<RefCell<Option<Vec<u8>>>>);
impl StateStore for Memory {
    fn load(&self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.borrow().clone())
    }
    fn save(&self, bytes: &[u8]) -> io::Result<()> {
        *self.0.borrow_mut() = Some(bytes.to_vec());
        Ok(())
    }
}
#[derive(Clone, Default)]
struct Selection(Rc<RefCell<Option<(String, bool)>>>);
impl Clipboard for Selection {
    fn snapshot(&self, _: bool) -> Result<Option<(String, bool)>, ClipboardError> {
        Ok(self.0.borrow().clone())
    }
    fn write(&self, text: &str, _: &str, sensitive: bool) -> Result<(), ClipboardError> {
        *self.0.borrow_mut() = Some((text.into(), sensitive));
        Ok(())
    }
}
type TestSession = Session<Selection, Memory, NoHistory>;
fn open(base: &str, store: Memory, selection: Selection, key: [u8; 32]) -> TestSession {
    let remote = Arc::new(
        Client::new(
            base,
            "fixture".into(),
            "integration-password-only".into(),
            true,
        )
        .unwrap(),
    );
    Session::open(
        SessionConfig {
            server: base.into(),
            encryption_key: key,
            filter: Filter::new(FilterMode::Off, &[], false).unwrap(),
            allow_sensitive: false,
        },
        remote,
        store,
        selection,
        NoHistory,
        Arc::new(AtomicU64::new(0)),
    )
    .map(|mut s| {
        s.set_sync_direction(SyncDirection::Bidirectional, false)
            .unwrap();
        s
    })
    .unwrap()
}
fn copy(s: &mut TestSession, text: &str) {
    *s.clipboard.0.borrow_mut() = Some((text.into(), false));
    s.record_local(text, None, false, Instant::now()).unwrap();
}
fn main() {
    let base = std::env::args().nth(1).expect("loopback HTTPS fixture URL");
    assert_eq!(
        std::env::var("RCS_CORE_DISPOSABLE_TEST").as_deref(),
        Ok("1")
    );
    assert!(base.starts_with("https://127.0.0.1:"));
    let key = state::random_key().unwrap();
    let store = Memory::default();
    let selection = Selection::default();
    let mut a = open(
        &base,
        Memory::default(),
        Selection::default(),
        state::random_key().unwrap(),
    );
    let mut b = open(&base, store.clone(), selection.clone(), key);
    a.check().unwrap();
    b.check().unwrap();
    for text in ["offline-first", "offline-last 中文 😀"] {
        copy(&mut a, text);
    }
    assert_eq!(a.flush(0).unwrap(), Outcome::Uploaded);
    assert_eq!(a.persisted.state.cursor, 1);
    assert_eq!(b.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
    assert_eq!(
        selection.0.borrow().as_ref().unwrap().0,
        "offline-last 中文 😀"
    );
    let id = b.persisted.state.ack.as_ref().unwrap().id.clone();
    assert_eq!(
        b.record_local("offline-last 中文 😀", Some(&id), false, Instant::now())
            .unwrap(),
        Outcome::Skipped
    );
    b.flush(0).unwrap();
    a.flush(0).unwrap();
    for text in ["remote-intermediate", "remote-latest"] {
        copy(&mut a, text);
        a.flush(0).unwrap();
        a.flush(0).unwrap();
    }
    assert_eq!(b.pull(0, Instant::now()).unwrap(), Outcome::Downloaded);
    assert_eq!(selection.0.borrow().as_ref().unwrap().0, "remote-latest");
    b.flush(0).unwrap();
    b.record_clear().unwrap();
    *selection.0.borrow_mut() = None;
    drop(b);
    let mut b = open(&base, store, selection.clone(), key);
    b.check().unwrap();
    assert_eq!(b.pull(0, Instant::now()).unwrap(), Outcome::Current);
    assert!(selection.0.borrow().is_none());

    copy(&mut a, "same request retried");
    let request = a.persisted.state.pending.clone().unwrap();
    let revision = a.remote.send(&a.persisted, &request).unwrap();
    assert_eq!(a.remote.send(&a.persisted, &request).unwrap(), revision);
    assert_eq!(a.flush(0).unwrap(), Outcome::Uploaded);
    assert_eq!(a.persisted.state.cursor, revision);
    b.pull(0, Instant::now()).unwrap();
    b.flush(0).unwrap();
    a.flush(0).unwrap();

    // A real server marks sensitive payloads; the default receiver skips them.
    copy(&mut a, "fixture sensitive");
    let mut marked = a.persisted.state.pending.clone().unwrap();
    marked.copy.sensitive = true;
    let sensitive_revision = a.remote.send(&a.persisted, &marked).unwrap();
    a.persisted
        .state
        .committed(&marked.copy.id, sensitive_revision)
        .unwrap();
    assert_eq!(b.pull(0, Instant::now()).unwrap(), Outcome::Filtered);
    b.flush(0).unwrap();
    a.flush(0).unwrap();
    assert_eq!(
        selection.0.borrow().as_ref().unwrap().0,
        "same request retried"
    );

    copy(&mut b, "competing local");
    copy(&mut a, "another device");
    a.flush(0).unwrap();
    assert!(matches!(b.flush(0), Err(SyncError::Conflict)));
    assert_eq!(selection.0.borrow().as_ref().unwrap().0, "competing local");
    println!("PASS: real HTTPS registration, latest-only synchronization, idempotency, clear/restart, echo, sensitive policy and conflict");
}
