//! Waiting for an ASCOM completion property.
//!
//! ASCOM "async" methods only *initiate*; the outcome arrives through a property
//! (`Slewing`, `IsMoving`, `ImageReady`, `Connecting`), which may itself raise.
//! One helper serves all of them so the polling discipline is defined once.

use std::time::{Duration, Instant};

use crate::actor::{Actor, Device};
use crate::error::{AscomError, Result};

/// The shortest interval the spec tolerates between two reads of a completion
/// property. Polling faster is explicitly discouraged ("needless polling").
pub const MIN_POLL: Duration = Duration::from_millis(100);

/// How long to wait for a completion property, and how often to look.
///
/// The fields are public so a caller can write a spec literally; the two guarantees
/// below are therefore enforced where the spec is read, not only in the constructors.
#[derive(Debug, Clone, Copy)]
pub struct WaitSpec {
    /// How long to keep asking before giving up.
    ///
    /// A duration too large to turn into an [`Instant`] deadline — [`Duration::MAX`],
    /// `Duration::from_secs(u64::MAX)` — is read as the intent behind it, "no
    /// timeout", and never as an overflow.
    pub timeout: Duration,
    /// Shortest interval between two reads.
    ///
    /// Anything below [`MIN_POLL`] is raised to it where it is used: a shorter
    /// interval would turn the wait into a busy RPC loop against the driver.
    pub poll: Duration,
}

impl WaitSpec {
    /// A sane default poll interval of [`MIN_POLL`].
    pub fn new(timeout: Duration) -> Self {
        Self { timeout, poll: MIN_POLL }
    }

    /// Explicit poll interval. Clamped to [`MIN_POLL`]: a shorter interval would
    /// turn into a busy RPC loop against the driver.
    pub fn with_poll(timeout: Duration, poll: Duration) -> Self {
        Self { timeout, poll: Duration::max(poll, MIN_POLL) }
    }

    /// Two minutes: enough for a meridian-flip slew or a long exposure read.
    pub fn default_timeout() -> Self {
        Self::new(Duration::from_secs(120))
    }
}

impl Default for WaitSpec {
    fn default() -> Self {
        Self::default_timeout()
    }
}

/// One polling loop, shared by every completion property.
///
/// The helpers below differ only in how they read the member and what counts as
/// finished, so the discipline is written once here: never poll faster than
/// [`MIN_POLL`], retry transient COM failures, give up immediately when the device is
/// gone, let any other driver error through as the answer, and time out naming the
/// member and what it last reported.
fn poll<T, Reader, Settled, Pending>(
    actor: &Actor,
    member: &str,
    spec: WaitSpec,
    reader: Reader,
    settled: Settled,
    pending: Pending,
) -> Result<T>
where
    T: Send + 'static,
    Reader: Fn(&mut Device) -> Result<T> + Send + Sync + 'static,
    Settled: Fn(&T) -> bool,
    Pending: Fn(&T) -> String,
{
    let reader = std::sync::Arc::new(reader);
    // Reading the member is an RPC on the device thread; the discipline around it is
    // thread-independent, so it lives in `wait_loop`, which tests can drive directly.
    wait_loop(
        move || {
            let this = std::sync::Arc::clone(&reader);
            actor.call(move |device| this(device))
        },
        member,
        spec,
        settled,
        pending,
    )
}

/// The polling discipline, over an arbitrary read of the completion member.
fn wait_loop<T, Read, Settled, Pending>(
    mut read: Read,
    member: &str,
    spec: WaitSpec,
    settled: Settled,
    pending: Pending,
) -> Result<T>
where
    Read: FnMut() -> Result<T>,
    Settled: Fn(&T) -> bool,
    Pending: Fn(&T) -> String,
{
    // `checked_add`, because `Instant + Duration` *panics* on overflow, and a caller
    // who means "no timeout" writes exactly `Duration::MAX`. A deadline that does not
    // fit is that intent: wait for the answer, whenever it comes.
    let deadline = Instant::now().checked_add(spec.timeout);
    // Clamped here and not only in `WaitSpec::with_poll`: `poll` is a public field, so
    // a struct literal can carry an interval the floor forbids.
    let poll = Duration::max(spec.poll, MIN_POLL);
    // Kept as text rather than as the last value, so a member that only ever raises
    // still produces a sensible timeout message.
    let mut last = String::from("never answered");
    loop {
        match read() {
            Ok(value) if settled(&value) => return Ok(value),
            Ok(value) => last = pending(&value),
            Err(err) if err.is_disconnected() => return Err(err),
            Err(err) if err.is_transient() => {}
            // Anything else is the device reporting an actual failure of the
            // operation we were waiting for.
            Err(err) => return Err(err),
        }
        // Sleep no longer than what is left: an unbounded sleep with `poll` larger
        // than `timeout` would overshoot the deadline by a whole interval and then
        // report a wait of `spec.timeout`.
        let sleep = match deadline {
            None => poll,
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(AscomError::timeout(
                        member.to_string(),
                        format!("{member} {last} after {:?}", spec.timeout),
                    ));
                }
                Duration::min(poll, deadline.saturating_duration_since(now))
            }
        };
        std::thread::sleep(sleep);
    }
}

/// Waits until `member` reads `false`.
///
/// An error from the property is not the end of the wait: a driver may briefly fail
/// a read while a slew is running, so transient codes are retried. A *disconnected*
/// error ends the wait immediately, because the device cannot come back.
pub fn wait_flag_false(actor: &Actor, member: &str, spec: WaitSpec) -> Result<()> {
    let name = member.to_string();
    poll(
        actor,
        member,
        spec,
        move |device| device.dispatch().get_bool(&name),
        |value| !*value,
        |_| "was still true".to_string(),
    )
    .map(|_| ())
}

/// Waits until `member` reads `true` (used by the camera's `ImageReady`).
pub fn wait_flag_true(actor: &Actor, member: &str, spec: WaitSpec) -> Result<()> {
    let name = member.to_string();
    poll(
        actor,
        member,
        spec,
        move |device| device.dispatch().get_bool(&name),
        |value| *value,
        |_| "was still false".to_string(),
    )
    .map(|_| ())
}

/// Waits until an integer member answers something `settled` accepts, and returns it.
///
/// For a filter wheel the completion property is `Position` itself: the spec makes it
/// answer `-1` while the wheel turns, so there is no flag to poll — the "still moving"
/// answer is a value rather than a `true`.
pub fn wait_i32(
    actor: &Actor,
    member: &str,
    spec: WaitSpec,
    settled: impl Fn(&i32) -> bool,
) -> Result<i32> {
    let name = member.to_string();
    poll(
        actor,
        member,
        spec,
        move |device| device.dispatch().get_i32(&name),
        settled,
        |value| format!("still reported {value}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AscomErrorKind;

    #[test]
    fn poll_interval_is_never_below_the_spec_floor() {
        assert_eq!(WaitSpec::new(Duration::from_secs(1)).poll, MIN_POLL);
        assert_eq!(WaitSpec::with_poll(Duration::from_secs(1), Duration::from_micros(1)).poll, MIN_POLL);
        let slower = WaitSpec::with_poll(Duration::from_secs(1), Duration::from_millis(250));
        assert_eq!(slower.poll, Duration::from_millis(250));
        assert_eq!(slower.timeout, Duration::from_secs(1));
    }

    #[test]
    fn default_timeout_is_two_minutes() {
        assert_eq!(WaitSpec::default().timeout, Duration::from_secs(120));
    }

    #[test]
    fn a_timeout_too_large_for_a_deadline_waits_instead_of_panicking() {
        // `Instant + Duration::MAX` panics; the wait must read it as "no timeout".
        for timeout in [Duration::MAX, Duration::from_secs(u64::MAX)] {
            let mut reads = 0;
            let value = wait_loop(
                || {
                    reads += 1;
                    Ok::<i32, AscomError>(if reads < 2 { -1 } else { 3 })
                },
                "Position",
                WaitSpec { timeout, poll: MIN_POLL },
                |value| *value >= 0,
                |value| format!("still reported {value}"),
            )
            .expect("an unrepresentable timeout must mean 'no deadline', not a panic");
            assert_eq!(value, 3);
            assert_eq!(reads, 2);
        }
    }

    #[test]
    fn a_wait_without_a_deadline_still_ends_on_a_driver_error() {
        // "No timeout" must not become an endless loop once the device answers no.
        let err = wait_loop(
            || Err(AscomError::local(AscomErrorKind::InvalidOperation, "IsMoving", "moving now")),
            "IsMoving",
            WaitSpec { timeout: Duration::MAX, poll: MIN_POLL },
            |value: &i32| *value == 0,
            |_| "still moving".to_string(),
        )
        .expect_err("a refusal must end a wait that has no deadline");
        assert_eq!(err.kind, AscomErrorKind::InvalidOperation);
    }

    #[test]
    fn a_zero_poll_is_raised_to_the_floor_where_it_is_used() {
        // `poll` is a public field, so a literal can bypass `with_poll`; between two
        // reads there must still be a full MIN_POLL, never a back-to-back RPC.
        let mut reads = 0;
        let start = Instant::now();
        wait_loop(
            || {
                reads += 1;
                Ok::<bool, AscomError>(reads >= 4)
            },
            "Slewing",
            WaitSpec { timeout: Duration::from_secs(30), poll: Duration::ZERO },
            |done| *done,
            |_| "was still true".to_string(),
        )
        .expect("the settled answer ends the wait");
        let elapsed = start.elapsed();
        assert_eq!(reads, 4);
        // Three intervals of 100 ms; a busy loop would finish in microseconds.
        assert!(elapsed >= Duration::from_millis(250), "three polls took {elapsed:?}");
    }

    #[test]
    fn a_poll_longer_than_the_timeout_does_not_overshoot_it() {
        let start = Instant::now();
        let err = wait_loop(
            || Ok::<bool, AscomError>(false),
            "Slewing",
            WaitSpec { timeout: Duration::from_millis(300), poll: Duration::from_secs(30) },
            |done| *done,
            |_| "was still true".to_string(),
        )
        .expect_err("a member that never settles must time out");
        let elapsed = start.elapsed();
        assert_eq!(err.kind, AscomErrorKind::Timeout);
        assert!(elapsed >= Duration::from_millis(250), "gave up after {elapsed:?}");
        assert!(elapsed < Duration::from_secs(3), "overshot a 300 ms timeout by {elapsed:?}");
    }

    #[test]
    fn a_timeout_names_the_member_and_what_it_last_reported() {
        let err = wait_loop(
            || Ok::<i32, AscomError>(-1),
            "Position",
            WaitSpec::new(Duration::from_millis(150)),
            |value| *value >= 0,
            |value| format!("still reported {value}"),
        )
        .expect_err("a wheel that never stops must time out");
        assert_eq!(err.kind, AscomErrorKind::Timeout);
        assert_eq!(err.member, "Position");
        assert_eq!(err.message, "Position still reported -1 after 150ms");
    }
}
