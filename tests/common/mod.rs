use std::{
    fs::{File, OpenOptions},
    io,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use fs2::{FileExt, lock_contended_error};

use astra_lite::core::engine::Engine;

/// Max time to wait for the hardware lock: long enough for the whole
/// hardware test suite to run sequentially (worst case ~30 minutes; the
/// goto tests alone can take ~10 minutes each), short enough to notice a
/// test process stuck while holding the lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// Cross-process lock that serializes all tests controlling the shared
/// hardware server (INDI on Linux, ASCOM Alpaca on Windows).
///
/// Integration tests run in separate processes. When they run in parallel
/// (e.g. `cargo nextest` or several `cargo test --test <name>` invocations),
/// each of them connects to the same local server and drives the same
/// simulated camera and telescope. The INDI simulators keep a single device
/// state, so two concurrent exposure requests collide and the tests fail
/// with "no events" watchdog panics. `#[serial_test::serial]` only works
/// within one process, which is why this lock is needed on top of it.
///
/// The lock is an exclusive `flock(2)` on a lock file. The kernel releases
/// it when the owning process exits, so a test that panics (the program
/// aborts on panic) cannot leave a stale lock behind.
///
/// Note that file locks are per open file description: a process must not
/// open the lock file a second time while still holding the lock from the
/// first open, otherwise it would block itself.
pub struct HardwareLock {
    _file: File,
}

impl HardwareLock {
    /// Waits until the lock is acquired.
    ///
    /// Returns an error instead of waiting forever if another test process
    /// holds the lock longer than `LOCK_TIMEOUT`.
    pub fn acquire() -> io::Result<Self> {
        let path = std::env::temp_dir().join("astra_lite_hardware_test.lock");
        // The lock file is never written to; `truncate(false)` makes that explicit.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;

        // `WouldBlock` covers Unix (EWOULDBLOCK). On Windows contention
        // comes back as ERROR_LOCK_VIOLATION, which std does not map to a
        // dedicated `ErrorKind`, so the raw code is compared against fs2's
        // canonical one.
        let contended = lock_contended_error().raw_os_error();
        let start = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == contended => {}
                Err(e) => return Err(e),
            }
            if start.elapsed() > LOCK_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "another test process held the hardware lock for more than {} minutes; \
                         it may be stuck",
                        LOCK_TIMEOUT.as_secs() / 60
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Disconnects the engine from the hardware server when dropped.
///
/// Dropping an `Engine` without `stop()` leaks the INDI connection: the
/// engine is self-referential (its own event handlers and the timer thread
/// hold `Arc<Engine>` clones), so it is never actually released, and the
/// socket and the reader thread stay alive until the process exits. The INDI
/// server keeps broadcasting to the leaked connection, and BLOBs (delivered
/// via file descriptors) can be consumed by a stale reader instead of the
/// current test's reader, silently dropping frames and stalling the mode.
/// Create the guard right after the engine so the connection is closed while
/// the test's `HardwareLock` is still held. Note that on Windows `stop()`
/// does not tear down the ASCOM Alpaca session (pre-existing gap in the
/// production code); the stale session is inert because `stop()` detaches
/// all HAL subscribers.
pub struct EngineTeardown {
    engine: Arc<Engine>,
}

impl EngineTeardown {
    pub fn new(engine: &Arc<Engine>) -> Self {
        Self {
            engine: Arc::clone(engine),
        }
    }
}

impl Drop for EngineTeardown {
    fn drop(&mut self) {
        self.engine.stop();
    }
}
