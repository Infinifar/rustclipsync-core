use rustclipsync_core::{
    client::{Registration, RemoteState},
    state::{Ack, Message, Outbox, Persisted, State},
    transport::Transport,
    Error,
};
use std::sync::{Arc, Mutex};

#[test]
fn worker_preserves_thread_and_unwind_auto_traits() {
    fn assert_traits<T: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
    assert_traits::<rustclipsync_core::worker::Worker>();
    assert_traits::<&rustclipsync_core::worker::Worker>();
}

fn identity() -> Persisted {
    Persisted {
        server: "https://example.test".into(),
        token: "fixture-token".into(),
        paired: true,
        state: State::new("epoch".into(), "device".into(), 0),
        inbox: None,
    }
}

struct ControlledPoll {
    responses: Mutex<std::sync::mpsc::Receiver<Result<Option<RemoteState>, Error>>>,
    entered: std::sync::mpsc::Sender<()>,
}
impl Transport for ControlledPoll {
    fn changes(&self, _: &Persisted) -> Result<Option<RemoteState>, Error> {
        self.entered.send(()).unwrap();
        self.responses.lock().unwrap().recv().unwrap()
    }
    fn register(&self, _: &Persisted) -> Result<Registration, Error> {
        unreachable!()
    }
    fn state(&self, _: &Persisted) -> Result<RemoteState, Error> {
        unreachable!()
    }
    fn content(&self, _: &Persisted, _: &Message) -> Result<String, Error> {
        unreachable!()
    }
    fn send(&self, _: &Persisted, _: &Outbox) -> Result<u64, Error> {
        unreachable!()
    }
    fn cancel(&self, _: &Persisted, _: &Outbox) -> Result<Option<u64>, Error> {
        unreachable!()
    }
    fn acknowledge(&self, _: &Persisted, _: &Ack) -> Result<(), Error> {
        unreachable!()
    }
}

#[test]
fn regression_worker_retries_disconnected_when_channel_was_full() {
    use rustclipsync_core::worker::{Event, Worker};
    use std::{sync::mpsc, time::Duration};
    let (responses_tx, responses_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::sync_channel(1);
    let remote = Arc::new(ControlledPoll {
        responses: Mutex::new(responses_rx),
        entered: entered_tx,
    });
    let worker = Worker::start(remote, identity(), 7, events_tx.clone());
    let deadline = Duration::from_secs(6);
    entered_rx.recv_timeout(deadline).unwrap();
    responses_tx.send(Ok(None)).unwrap();
    entered_rx.recv_timeout(deadline).unwrap();
    assert_eq!(
        events_rx.recv_timeout(deadline).unwrap(),
        (7, Event::Connected)
    );
    events_tx.send((7, Event::Changed)).unwrap(); // Deliberately fill the bounded event queue.
    responses_tx.send(Err(Error::Status(503))).unwrap();
    entered_rx.recv_timeout(deadline).unwrap(); // First failure processed; retry is now waiting.
    assert_eq!(
        events_rx.recv_timeout(deadline).unwrap(),
        (7, Event::Changed)
    );
    responses_tx.send(Err(Error::Status(503))).unwrap();
    entered_rx.recv_timeout(deadline).unwrap(); // Second failure processed with room in the queue.
    let notice = events_rx.try_recv();
    drop(worker);
    responses_tx.send(Ok(None)).unwrap(); // Release the stopped polling thread.
    assert_eq!(
        notice,
        Ok((7, Event::Disconnected)),
        "the connection transition was lost when the queue filled and never retried"
    );
}

#[test]
fn worker_retries_connected_when_initial_queue_was_full() {
    use rustclipsync_core::worker::{Event, Worker};
    use std::{sync::mpsc, time::Duration};
    let (responses_tx, responses_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::sync_channel(1);
    events_tx.send((7, Event::Changed)).unwrap();
    let remote = Arc::new(ControlledPoll {
        responses: Mutex::new(responses_rx),
        entered: entered_tx,
    });
    let worker = Worker::start(remote, identity(), 7, events_tx);
    let deadline = Duration::from_secs(6);
    entered_rx.recv_timeout(deadline).unwrap();
    responses_tx.send(Ok(None)).unwrap();
    entered_rx.recv_timeout(deadline).unwrap(); // First Connected encountered a full queue.
    assert_eq!(
        events_rx.recv_timeout(deadline).unwrap(),
        (7, Event::Changed)
    );
    responses_tx.send(Ok(None)).unwrap();
    entered_rx.recv_timeout(deadline).unwrap();
    let notice = events_rx.try_recv();
    drop(worker);
    responses_tx.send(Ok(None)).unwrap();
    assert_eq!(notice, Ok((7, Event::Connected)));
}

#[test]
fn terminal_error_delivers_disconnect_and_wakeup_before_exit() {
    use rustclipsync_core::worker::{Event, Worker};
    use std::{sync::mpsc, time::Duration};
    let (responses_tx, responses_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::sync_channel(1);
    let remote = Arc::new(ControlledPoll {
        responses: Mutex::new(responses_rx),
        entered: entered_tx,
    });
    let _worker = Worker::start(remote, identity(), 7, events_tx);
    let deadline = Duration::from_secs(6);
    entered_rx.recv_timeout(deadline).unwrap();
    responses_tx.send(Err(Error::Status(401))).unwrap();
    assert_eq!(
        events_rx.recv_timeout(deadline).unwrap(),
        (7, Event::Disconnected)
    );
    assert_eq!(
        events_rx.recv_timeout(deadline).unwrap(),
        (7, Event::Changed)
    );
    assert_eq!(
        events_rx.recv_timeout(deadline),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}

#[test]
fn dropping_worker_stops_terminal_notification_retry_with_full_queue() {
    use rustclipsync_core::worker::{Event, Worker};
    use std::{sync::mpsc, time::Duration};
    let (responses_tx, responses_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::sync_channel(1);
    events_tx.send((7, Event::Changed)).unwrap();
    let remote = Arc::new(ControlledPoll {
        responses: Mutex::new(responses_rx),
        entered: entered_tx,
    });
    let worker = Worker::start(remote, identity(), 7, events_tx);
    let deadline = Duration::from_secs(6);
    entered_rx.recv_timeout(deadline).unwrap();
    responses_tx.send(Err(Error::Status(401))).unwrap();
    drop(worker);
    // Keep the event queue full until the transport has been released.
    assert_eq!(
        entered_rx.recv_timeout(deadline),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    assert_eq!(
        events_rx.recv_timeout(deadline).unwrap(),
        (7, Event::Changed)
    );
    assert_eq!(
        events_rx.recv_timeout(deadline),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}

#[test]
fn shutdown_and_join_wait_for_inflight_transport_to_return() {
    use rustclipsync_core::worker::{Event, Worker};
    use std::{sync::mpsc, time::Duration};
    let (responses_tx, responses_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::sync_channel(1);
    events_tx.send((7, Event::Changed)).unwrap();
    let remote = Arc::new(ControlledPoll {
        responses: Mutex::new(responses_rx),
        entered: entered_tx,
    });
    let worker = Worker::start(remote, identity(), 7, events_tx);
    let deadline = Duration::from_secs(6);
    entered_rx.recv_timeout(deadline).unwrap();
    worker.shutdown();
    responses_tx.send(Err(Error::Status(401))).unwrap();
    worker
        .join()
        .expect("worker must stop even with a full event queue");
    assert_eq!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    assert_eq!(events_rx.try_recv(), Ok((7, Event::Changed)));
    assert_eq!(events_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
}

#[test]
fn join_reports_transport_panic_to_the_host() {
    use rustclipsync_core::worker::Worker;
    use std::{sync::mpsc, time::Duration};
    let (responses_tx, responses_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (events_tx, _events_rx) = mpsc::sync_channel(1);
    let remote = Arc::new(ControlledPoll {
        responses: Mutex::new(responses_rx),
        entered: entered_tx,
    });
    let worker = Worker::start(remote, identity(), 7, events_tx);
    entered_rx.recv_timeout(Duration::from_secs(6)).unwrap();
    // The controlled transport panics on an unexpected response-channel close.
    drop(responses_tx);
    assert!(
        worker.join().is_err(),
        "transport panics must be observable by the host"
    );
}
