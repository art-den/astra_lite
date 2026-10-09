//! One dedicated STA COM thread per device, driven by a command channel.
//!
//! `IDispatch` must never be handed between threads, so the interface pointer lives
//! on a private thread and public handles are `Send + Clone` senders of closures.
//! That is what makes a handle safe to move between threads without an
//! `unsafe impl Send` over a raw `IDispatch`.
//!
//! Rules enforced here:
//! * the message queue is pumped while the thread is idle, so a modal driver dialog
//!   stays responsive;
//! * a task that panics answers with an error instead of killing the thread;
//! * only the last handle releases the COM object, and never touches `Connected`;
//! * COM is uninitialised on the owning thread, never on the caller's.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::com::ComGuard;
use crate::com::apartment;
use crate::com::dispatch::Dispatch;
use crate::device::CapabilitySnapshot;
use crate::error::{AscomError, AscomErrorKind, Result};

/// How long the worker waits for work before pumping the message queue.
const IDLE_TICK: Duration = Duration::from_millis(50);
/// How long the last handle waits for the worker to release the driver.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

type Reply = Box<dyn Any + Send>;
type Task = dyn FnOnce(&mut Device) -> Reply + Send;

enum Message {
    Task { task: Box<Task>, reply: Sender<Result<Reply>> },
    Shutdown,
}

/// The device side of the actor. Exists only on the COM thread.
pub struct Device {
    dispatch: Dispatch,
    /// Filled on first use, dropped whenever the connection state changes.
    capabilities: Option<CapabilitySnapshot>,
}

impl Device {
    /// The late-bound driver. Only reachable from inside a task, i.e. on the
    /// COM thread.
    pub fn dispatch(&self) -> &Dispatch {
        &self.dispatch
    }

    pub fn capabilities(&self) -> Option<&CapabilitySnapshot> {
        self.capabilities.as_ref()
    }

    pub fn set_capabilities(&mut self, snapshot: CapabilitySnapshot) {
        self.capabilities = Some(snapshot);
    }

    /// Connection state changed, so every `CanXxx` we cached is suspect.
    pub fn invalidate_capabilities(&mut self) {
        self.capabilities = None;
    }
}

/// The state one COM thread shares with every handle to it.
///
/// `Drop` lives here rather than on [`Actor`] because it must run exactly once, when
/// the last handle goes away: a clone that leaves early must not shut down the device
/// the surviving handles are still using.
struct Inner {
    tx: Sender<Message>,
    /// Win32 id of the worker thread, for [`Actor::cancel_call`]; zero once it exits,
    /// because Windows recycles thread ids. Shared with the worker, hence the `Arc`.
    worker_thread: Arc<AtomicU32>,
    /// Set when this thread stops answering calls — by the worker as its loop ends,
    /// or by `Drop` when it detaches a stuck worker. Shared, hence the `Arc`.
    dead: Arc<AtomicBool>,
    done: Mutex<Option<Receiver<()>>>,
}

/// A handle to a device's COM thread. `Send + Clone`.
///
/// Cloning is cheap and safe; the COM thread and the driver outlive every clone but
/// the last one.
#[derive(Clone)]
pub struct Actor {
    inner: Arc<Inner>,
}

impl Actor {
    /// Starts a COM thread and instantiates `prog_id` on it.
    ///
    /// Returns only after the driver object exists, so activation errors surface
    /// here rather than on the first call.
    pub fn spawn(prog_id: &str) -> Result<Self> {
        let prog_id = prog_id.to_string();
        Self::start(move || {
            let dispatch = Dispatch::from_prog_id(&prog_id)?;
            Ok(Device { dispatch, capabilities: None })
        })
    }

    /// Starts a COM thread and builds the device on it with `make`.
    ///
    /// The device is created on the worker thread because an `IDispatch` must never
    /// cross a thread boundary. The tests use the same hook with an in-process mock
    /// `IDispatch`, so the actor is covered without a driver installed.
    fn start(make: impl FnOnce() -> Result<Device> + Send + 'static) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Message>();
        let (init_tx, init_rx) = mpsc::channel::<Result<()>>();
        let worker_thread = Arc::new(AtomicU32::new(0));
        let thread_flag = Arc::clone(&worker_thread);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let dead = Arc::new(AtomicBool::new(false));
        let dead_flag = Arc::clone(&dead);

        let handle: JoinHandle<()> = std::thread::spawn(move || {
            Self::work(make, rx, init_tx, thread_flag, done_tx, dead_flag);
        });

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(Inner {
                    tx,
                    worker_thread,
                    dead,
                    done: Mutex::new(Some(done_rx)),
                }),
            }),
            // The worker bailed out; reaping it keeps the thread from lingering.
            Ok(Err(err)) => {
                let _ = handle.join();
                Err(err)
            }
            Err(_) => Err(AscomError::local(
                AscomErrorKind::Disconnected,
                "Actor::spawn",
                "the COM thread died while creating the driver",
            )),
        }
    }

    fn work(
        make: impl FnOnce() -> Result<Device>,
        rx: Receiver<Message>,
        init_tx: Sender<Result<()>>,
        thread_flag: Arc<AtomicU32>,
        done_tx: Sender<()>,
        dead_flag: Arc<AtomicBool>,
    ) {
        let guard = match ComGuard::new() {
            Ok(guard) => guard,
            Err(err) => {
                let _ = init_tx.send(Err(err));
                return;
            }
        };
        thread_flag.store(ComGuard::thread_id(), Ordering::SeqCst);
        let device = match make() {
            Ok(device) => device,
            Err(err) => {
                let _ = init_tx.send(Err(err));
                return;
            }
        };
        if init_tx.send(Ok(())).is_err() {
            return;
        }
        // `Worker` owns the apartment guard, so it outlives the interface pointer.
        Worker { device: Some(device), _guard: guard }.run(rx);
        // The loop only ends for good, so this thread answers nothing further and is
        // no longer a legitimate `CoCancelCall` target. Telling the handles both
        // facts is the worker's job, and the only place that knows them for certain.
        dead_flag.store(true, Ordering::SeqCst);
        thread_flag.store(0, Ordering::SeqCst);
        let _ = done_tx.send(());
    }

    /// Runs one unit of work on the COM thread and waits for its result.
    ///
    /// The closure returns `Result`, and so does this call: the channel's own failure
    /// and the driver's error are the same kind of failure to a caller, so the two
    /// `Result`s are collapsed into one.
    pub fn call<R: Any + Send>(
        &self,
        f: impl FnOnce(&mut Device) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        if self.inner.dead.load(Ordering::SeqCst) {
            return Err(self.dead_error());
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let task: Box<Task> = Box::new(move |device| Box::new(f(device)) as Reply);
        self.inner
            .tx
            .send(Message::Task { task, reply: reply_tx })
            .map_err(|_| self.dead_error())?;

        match reply_rx.recv() {
            Ok(Ok(boxed)) => *boxed.downcast::<Result<R>>().map_err(|_| {
                AscomError::local(
                    AscomErrorKind::Com,
                    "Actor::call",
                    "the COM thread answered with an unexpected type",
                )
            })?,
            // A failure of the thread's own bookkeeping. Its kind says nothing about
            // whether the thread is alive, so only the worker may set `dead`.
            Ok(Err(err)) => Err(err),
            // The worker dropped our reply slot without answering: it is gone.
            Err(_) => {
                self.inner.dead.store(true, Ordering::SeqCst);
                Err(self.dead_error())
            }
        }
    }

    fn dead_error(&self) -> AscomError {
        AscomError::local(
            AscomErrorKind::Disconnected,
            "Actor",
            "the COM thread for this device is no longer running",
        )
    }

    /// Aborts a blocking driver call in progress on the worker thread.
    ///
    /// The escape hatch for a driver that "hangs" inside e.g. `ImageArray`; a
    /// watchdog thread calls this while the handle stays usable afterwards.
    /// Refused when the worker is gone (its id may already have been recycled) and
    /// when called from the worker itself, which would cancel the caller's own call.
    pub fn cancel_call(&self) -> Result<()> {
        if self.inner.dead.load(Ordering::SeqCst) {
            return Err(self.dead_error());
        }
        let thread_id = self.inner.worker_thread.load(Ordering::SeqCst);
        if thread_id == 0 {
            return Err(AscomError::local(
                AscomErrorKind::Com,
                "CoCancelCall",
                "the COM thread has not registered its thread id yet",
            ));
        }
        if thread_id == ComGuard::thread_id() {
            return Err(AscomError::local(
                AscomErrorKind::Com,
                "CoCancelCall",
                "refusing to cancel the calling thread's own COM call",
            ));
        }
        apartment::cancel_call(thread_id)
    }

    pub fn is_dead(&self) -> bool {
        self.inner.dead.load(Ordering::SeqCst)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Runs once, for the last handle. Ask the worker to release the driver and
        // wait for it: COM must be uninitialised by the thread that initialised it.
        // `Connected` is left exactly as it is, by design.
        let _ = self.tx.send(Message::Shutdown);
        if let Some(receiver) = self.done.lock().ok().and_then(|mut slot| slot.take()) {
            match receiver.recv_timeout(SHUTDOWN_TIMEOUT) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // The worker is stuck inside a driver call. Forcing it would mean
                    // killing a thread that owns COM, so we detach instead. The id
                    // stays stale behind us, which is exactly why `cancel_call` also
                    // refuses once `dead` is set.
                    self.dead.store(true, Ordering::SeqCst);
                }
            }
        }
    }
}

/// The worker loop, on the COM thread.
struct Worker {
    device: Option<Device>,
    /// Dropped after the loop, so the apartment outlives the interface pointer.
    _guard: ComGuard,
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        loop {
            match rx.recv_timeout(IDLE_TICK) {
                Ok(Message::Task { task, reply }) => {
                    let outcome = self.execute(task);
                    // A dropped reply slot only means the caller stopped caring.
                    let _ = reply.send(outcome);
                }
                Ok(Message::Shutdown) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => apartment::pump_messages(),
                // All handles are gone.
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // Release the driver while the apartment is still alive.
        self.device = None;
        apartment::pump_messages();
    }

    fn execute(&mut self, task: Box<Task>) -> Result<Reply> {
        let Some(device) = self.device.as_mut() else {
            return Err(AscomError::local(
                AscomErrorKind::Disconnected,
                "Actor",
                "the driver object has already been released",
            ));
        };
        // A panicking task must not take the thread (and the apartment) with it.
        match catch_unwind(AssertUnwindSafe(|| task(device))) {
            Ok(reply) => {
                apartment::pump_messages();
                Ok(reply)
            }
            Err(payload) => {
                // A panic is this crate's fault, not evidence that the device is gone:
                // the thread survives and must keep answering calls, so the kind must
                // not be one a caller could read as "the thread is dead".
                let text = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a payload that is not a string".to_string());
                Err(AscomError::local(
                    AscomErrorKind::Com,
                    "Actor",
                    format!("a task panicked on the COM thread: {text}"),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::mock::{Element, ItemBase, MockCollection};

    /// An actor whose device is an in-process `IDispatch` (see `com::mock`), so the
    /// thread, the handshake and the flags are covered without a driver installed.
    /// Its only member is `Count`, answering the number of mock elements.
    fn mock_actor() -> Actor {
        Actor::start(|| {
            let mock =
                MockCollection::indexed(vec![Element::Empty, Element::Int(42)], ItemBase::Zero);
            let variant = mock.into_variant();
            Ok(Device { dispatch: Dispatch::from_variant(&variant)?, capabilities: None })
        })
        .expect("the mock device is created on the COM thread")
    }

    fn count(actor: &Actor) -> Result<i32> {
        actor.call(|device| device.dispatch().get_i32("Count"))
    }

    /// Long enough for the worker to notice anything a buggy `Drop` queued for it.
    const SETTLE: Duration = Duration::from_millis(200);

    #[test]
    fn a_dropped_clone_leaves_the_other_handles_usable() {
        // A handle is `Clone` and every device wrapper hands out clones, so only the
        // last handle may shut the device down.
        let actor = mock_actor();
        drop(actor.clone());
        std::thread::sleep(SETTLE);
        assert!(!actor.is_dead(), "dropping a clone shut the device down");
        assert_eq!(count(&actor).expect("the surviving handle must still work"), 2);
    }

    #[test]
    fn a_panicking_task_is_answered_without_poisoning_the_actor() {
        let actor = mock_actor();
        let err = actor
            .call(|_device| -> Result<i32> { panic!("boom from a task") })
            .expect_err("a panic must come back as an error");
        assert!(!actor.is_dead(), "a panicking task killed a healthy thread: {err}");
        assert!(err.message.contains("boom from a task"), "the panic text is lost: {err}");
        assert_eq!(count(&actor).expect("the next call must still be answered"), 2);
    }

    #[test]
    fn a_disconnected_answer_from_a_live_driver_does_not_kill_the_actor() {
        // `Disconnected` is also a legitimate driver answer (`RPC_E_DISCONNECTED`), so
        // it must reach the caller without being read as "the thread is gone".
        let actor = mock_actor();
        let err = actor
            .call(|_device| -> Result<i32> {
                Err(AscomError::local(
                    AscomErrorKind::Disconnected,
                    "Link",
                    "the driver objects are gone",
                ))
            })
            .expect_err("the driver error must reach the caller");
        assert_eq!(err.kind, AscomErrorKind::Disconnected);
        assert!(!actor.is_dead(), "a driver answer killed a healthy thread");
        assert_eq!(count(&actor).expect("the next call must still be answered"), 2);
    }

    #[test]
    fn a_task_cannot_cancel_the_thread_it_runs_on() {
        // `CoCancelCall` on one's own thread targets the *caller's* pending call, so a
        // handle dropped inside a task must be refused instead.
        let actor = mock_actor();
        let clone = actor.clone();
        let outcome = actor.call(move |_device| clone.cancel_call().map(|()| 0i32));
        let err = outcome.expect_err("cancelling one's own thread must be refused");
        assert_eq!(err.kind, AscomErrorKind::Com);
        assert!(
            err.message.contains("own COM call"),
            "not refused by the self-cancel guard: {err}"
        );
        assert_eq!(count(&actor).expect("the thread must stay usable"), 2);
    }

    #[test]
    fn another_thread_is_still_allowed_to_cancel() {
        // The guard must not swallow the escape hatch it protects: from a watchdog
        // thread the call reaches `CoCancelCall`, whatever an idle thread answers
        // there.
        let actor = mock_actor();
        if let Err(err) = actor.cancel_call() {
            assert!(
                !err.message.contains("own COM call"),
                "a foreign thread was refused by the self-cancel guard: {err}"
            );
        }
        assert_eq!(count(&actor).expect("the thread must stay usable"), 2);
    }

    #[test]
    fn the_worker_clears_its_thread_id_when_it_exits() {
        // Win32 thread ids are recycled, so an id left behind lets `cancel_call`
        // abort an unrelated thread and report success.
        let actor = mock_actor();
        let flag = Arc::clone(&actor.inner.worker_thread);
        assert_ne!(flag.load(Ordering::SeqCst), 0, "the id must be registered");
        drop(actor);
        assert_eq!(flag.load(Ordering::SeqCst), 0, "a stale id survived the worker");
    }
}
