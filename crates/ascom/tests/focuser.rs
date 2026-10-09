//! Tests for `IFocuserV4` against a live driver.
//!
//! Run: `ASCOM_FOCUSER_PROG_ID=ASCOM.OmniSim.Focuser cargo test --test focuser`
//!
//! The read-only checklist honours that variable; the write tests at the bottom always
//! run against the one driver named by `DRIVER`, because writing to a proxy or to a
//! legacy driver would prove nothing.

mod common;
// The driver is one shared Singleton that survives the process: `#[serial]` keeps
// two tests off it at once, which is what `--test-threads=1` used to guarantee.
use serial_test::serial;

use ascom::com::Variant;
use ascom::com::dispatch::NOT_IMPLEMENTED;
use ascom::device::{AscomDevice, DeviceSpec};
use ascom::error::{AscomErrorKind, Result};
use ascom::focuser::Focuser;
use ascom::wait::{WaitSpec, wait_flag_false};
use std::cell::Cell;
use std::time::{Duration, Instant};

use common::{
    check_connected_idempotent, check_identity_members, check_operational_needs_connection,
    check_unknown_action, deviation, is_binding, is_raw_managed, prog_id, walk_members,
    APPLY_TIMEOUT, POLL,
};

/// The read-only members of IFocuserV4 (COM-visible), from the spec.
const MEMBERS: &[&str] = &[
    "Absolute",
    "Connected",
    "Connecting",
    "Description",
    "DeviceState",
    "DriverInfo",
    "DriverVersion",
    "InterfaceVersion",
    "IsMoving",
    "Link",
    "MaxIncrement",
    "MaxStep",
    "Name",
    "Position",
    "StepSize",
    "SupportedActions",
    "TempComp",
    "TempCompAvailable",
    "Temperature",
];

fn open() -> Focuser {
    let id = prog_id("ASCOM_FOCUSER_PROG_ID", "ASCOM.OmniSim.Focuser");
    let focuser = Focuser::open(&DeviceSpec::new(id)).expect("open focuser driver");
    focuser.set_connected(true).expect("connect");
    focuser
}

#[serial]
#[test]
fn every_member_answers_or_is_unsupported() {
    let focuser = open();
    walk_members(&focuser, MEMBERS);
}

#[serial]
#[test]
fn unknown_action_is_action_not_implemented() {
    let focuser = open();
    check_unknown_action(&focuser);
}

#[serial]
#[test]
fn identity_members_are_readable() {
    let focuser = open();
    check_identity_members(&focuser).expect("identity members");
}

#[serial]
#[test]
fn connected_is_idempotent() {
    let focuser = open();
    check_connected_idempotent(&focuser).expect("connected cycle");
}

#[serial]
#[test]
fn operational_property_needs_connection() {
    let focuser = open();
    check_operational_needs_connection(&focuser, "Position").expect("not-connected probe");
}

// ------------------------------------------------------------------- writes ---
//
// The write scenario is: read the property, write it, wait until *the driver* reports
// the change, read it back. Step 3 exists only where there is something to wait for:
// `Move` has `IsMoving`, while `TempComp` has no completion property in the spec and
// answers with the new value immediately, so its wait is a short grace period.
//
// One driver, hard-coded on purpose. The registry also lists `ASCOM.DeviceHub.Focuser`
// and `ASCOM.JustAHub*.Focuser`, which are proxies with no device behind them, and the
// legacy `FocusSim.Focuser`, which does not expose `Connected` at all — none of them
// can be written to meaningfully.
//
// NOTE, deliberately not covered: `Halt()` against a *moving* focuser. A 50-step move
// on this simulator finishes in well under a second, so stopping it mid-flight would
// be a race; that test needs a move long enough to observe the stop. `halt_supported()`
// and `probe_halt()` only prove the member exists.

/// The driver the write tests run against.
const DRIVER: &str = "ASCOM.OmniSim.Focuser";

/// Bound for a deliberate move. The grace period, the poll step and the convention for
/// reporting a driver deviation live in `tests/common/mod.rs`.
const MOVE_TIMEOUT: Duration = Duration::from_secs(30);

/// A read-only bool member that some drivers do not implement at all.
fn flag(focuser: &Focuser, member: &str) -> Option<bool> {
    match focuser.probe(member) {
        Ok(value) if value != NOT_IMPLEMENTED => value.parse().ok(),
        _ => None,
    }
}

/// `Move(Position)` straight to the driver, bypassing the wrapper's own range check.
///
/// `Focuser::move_to` rejects nonsense locally, so testing *the driver's* answer to a
/// bad argument — which the wrapper then has to classify — requires going around it.
fn move_raw(focuser: &Focuser, position: i32) -> Result<()> {
    focuser.actor().call(move |device| {
        let target = Variant::from_i32(position);
        device.dispatch().call_void("Move", &[&target])
    })
}

/// Puts the focuser back the way it was found, even when the test panics.
///
/// Drop must not panic while another panic is unwinding — that aborts the process and
/// hides the real failure — so a failed restore is reported two ways: when the test
/// itself passed it *is* a failure and panics; when the test is already failing it goes
/// to stderr, because the test's own reason is the more useful one. This matters
/// because the simulator is one shared singleton: state left behind breaks the tests
/// that run after it.
struct Restorer {
    focuser: Focuser,
    /// What the driver reported when the handle was opened.
    temp_comp: Option<bool>,
    position: i32,
    /// Only what the test actually touched gets rewritten. `Cell` so that marking from
    /// a test that already holds `&Focuser` stays a borrow-checker non-event.
    touched_motion: Cell<bool>,
    touched_temp_comp: Cell<bool>,
    touched_connection: Cell<bool>,
}

impl Restorer {
    fn focuser(&self) -> &Focuser {
        &self.focuser
    }

    fn mark_motion(&self) {
        self.touched_motion.set(true);
    }

    fn mark_temp_comp(&self) {
        self.touched_temp_comp.set(true);
    }

    fn mark_connection(&self) {
        self.touched_connection.set(true);
    }
}

impl Drop for Restorer {
    fn drop(&mut self) {
        let mut problems = Vec::new();

        // Connection first: nothing below can be restored against a disconnected driver.
        if self.touched_connection.get() && !self.focuser.connected().unwrap_or(false) {
            if let Err(error) = self.focuser.set_connected(true) {
                problems.push(format!("reconnect: {error}"));
            }
        }
        if self.touched_motion.get() {
            // Motion before position: a move still running would undo the position.
            let _ = self.focuser.halt();
            let idle = wait_flag_false(
                self.focuser.actor(),
                "IsMoving",
                WaitSpec::with_poll(Duration::from_secs(5), POLL),
            );
            if let Err(error) = idle {
                problems.push(format!("IsMoving still set after Halt: {error}"));
            }
            match self.focuser.position() {
                Ok(current) if current != self.position => {
                    if let Err(error) =
                        self.focuser.move_to_and_wait(self.position, WaitSpec::new(MOVE_TIMEOUT))
                    {
                        problems.push(format!(
                            "Move({}) back from {current}: {error}",
                            self.position
                        ));
                    }
                }
                Err(error) => problems.push(format!("Position to restore: {error}")),
                Ok(_) => {}
            }
        }
        if self.touched_temp_comp.get() {
            if let Some(wanted) = self.temp_comp {
                if let Err(error) = self.focuser.set_temp_comp(wanted) {
                    problems.push(format!("restore TempComp={wanted}: {error}"));
                }
            }
        }

        // Step 4 for the guard itself: verify the driver really is back where we started.
        if self.touched_motion.get() {
            match self.focuser.position() {
                Ok(current) if current != self.position => {
                    problems.push(format!("Position is {current}, was {}", self.position));
                }
                Err(error) => problems.push(format!("Position after restore: {error}")),
                Ok(_) => {}
            }
        }
        if self.touched_temp_comp.get() {
            match (self.temp_comp, self.focuser.temp_comp()) {
                (None, _) => problems.push(
                    "TempComp was written but its original value was never read".to_string(),
                ),
                (Some(wanted), Ok(current)) if current != wanted => {
                    problems.push(format!("TempComp is {current}, was {wanted}"));
                }
                (_, Err(error)) => problems.push(format!("TempComp after restore: {error}")),
                _ => {}
            }
        }

        if problems.is_empty() {
            return;
        }
        let text = format!("could not restore {DRIVER}:\n{}", problems.join("\n"));
        if std::thread::panicking() {
            eprintln!("RESTORE FAILED (a panic is already unwinding): {text}");
        } else {
            panic!("{text}");
        }
    }
}

/// Opens [`DRIVER`], connects it, and snapshots everything the write tests may change.
fn guarded() -> Restorer {
    let focuser = Focuser::open(&DeviceSpec::new(DRIVER)).expect("open focuser driver");
    focuser.set_connected(true).expect("connect");
    let temp_comp = focuser.temp_comp().ok();
    let position = focuser.position().expect("Position is mandatory");
    Restorer {
        focuser,
        temp_comp,
        position,
        touched_motion: Cell::new(false),
        touched_temp_comp: Cell::new(false),
        touched_connection: Cell::new(false),
    }
}

/// read → write → wait → read on the one writable property `IFocuserV4` has.
#[serial]
#[test]
fn temp_comp_write_is_reported_by_the_driver() {
    let guard = guarded();
    let focuser = guard.focuser();
    let start = focuser.temp_comp().expect("TempComp is mandatory");

    if flag(focuser, "TempCompAvailable") != Some(true) {
        // Nothing to write. The spec's answer for a property that is not implemented
        // is PropertyNotImplemented; refusing it as an invalid value is also accepted.
        let error =
            focuser.set_temp_comp(!start).expect_err("writing TempComp must be refused");
        println!("note: TempCompAvailable is not true, write refused with {:?}", error.kind);
        assert!(
            error.is_unsupported() || error.kind == AscomErrorKind::InvalidValue,
            "writing TempComp while TempCompAvailable is false gave {:?}",
            error.kind
        );
        assert_eq!(
            focuser.temp_comp().expect("TempComp still readable"),
            start,
            "a refused write must not change the value"
        );
        return;
    }

    guard.mark_temp_comp();
    focuser
        .set_temp_comp(!start)
        .expect("TempCompAvailable is true, so the write must be accepted");

    // Step 3: there is no completion property, so wait for the driver to *report* it.
    let deadline = Instant::now() + APPLY_TIMEOUT;
    while focuser.temp_comp().unwrap_or(start) == start && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
    // Step 4: read back.
    let applied = focuser.temp_comp().expect("TempComp readable after the write");
    if applied != !start {
        deviation(&format!(
            "TempComp still reads {applied} {APPLY_TIMEOUT:?} after writing {}",
            !start
        ));
    }
    // Restoring the original value, and checking that it took, is the guard's job.
}

/// `Move` is the write with a real completion property, and `Position` the property it
/// changes: read it, move, wait for `IsMoving == false`, read it again.
#[serial]
#[test]
fn move_changes_position_and_is_reported() {
    let guard = guarded();
    let focuser = guard.focuser();
    let absolute = focuser.absolute().expect("Absolute is mandatory");
    let start = focuser.position().expect("Position is mandatory");
    let step = focuser.max_increment().expect("MaxIncrement").clamp(1, 50);
    let max_step = focuser.max_step().expect("MaxStep");

    // An absolute focuser takes a target, a relative one a delta; never aim past MaxStep.
    let target = if !absolute {
        step
    } else if start + step <= max_step {
        start + step
    } else if start - step >= 0 {
        start - step
    } else {
        panic!("no room to move: Position={start}, MaxStep={max_step}, step={step}");
    };

    guard.mark_motion();
    focuser
        .move_to_and_wait(target, WaitSpec::new(MOVE_TIMEOUT))
        .expect("Move followed by IsMoving == false");

    let after = focuser.position().expect("Position after Move");
    assert!(
        !focuser.is_moving().expect("IsMoving after the move"),
        "a completed move must leave IsMoving false"
    );
    if absolute {
        assert_ne!(after, start, "a Move of {step} steps must actually change Position");
        assert_eq!(after, target, "an absolute focuser must end at the requested position");
    } else {
        assert_eq!(after - start, step, "a relative focuser must move by the requested delta");
    }
}

/// The completion property of a move is `IsMoving`, so the wait must end because the
/// driver cleared the flag, never because the bound expired.
#[serial]
#[test]
fn move_completes_via_ismoving_without_timeout() {
    let guard = guarded();
    let focuser = guard.focuser();
    let start = focuser.position().expect("position");
    let step = focuser.max_increment().unwrap_or(20).clamp(1, 20);
    let target = start + step;

    // Exactly one Move: `move_to_and_wait` starts it itself, and a second write issued
    // while the device is still moving is answered by a raw CLR exception, which the
    // wrapper can only classify as `Com` (see `common::is_raw_managed`).
    guard.mark_motion();
    // Spec: needless polling is discouraged, so 100 ms; the bound is generous.
    let spec = WaitSpec::new(Duration::from_secs(60));
    match focuser.move_to_and_wait(target, spec) {
        Err(error) if error.kind == AscomErrorKind::Timeout => {
            panic!("IsMoving never cleared after Move: {error}");
        }
        Err(error) if is_raw_managed(&error) => {
            deviation(&format!("Move({target}) raised a raw non-ASCOM error: {error}"));
            // Nothing to assert about completion here; the guard still restores.
            return;
        }
        Err(error) => panic!("Move failed: {error}"),
        Ok(()) => {}
    }
    assert!(!focuser.is_moving().expect("IsMoving after move"));
}

/// Error handling: an out-of-range position must be refused, and the refusal must not
/// move anything. Goes through `move_raw` because the wrapper would reject it first.
#[serial]
#[test]
fn driver_refuses_a_position_beyond_max_step() {
    let guard = guarded();
    let focuser = guard.focuser();
    let max_step = focuser.max_step().expect("MaxStep");
    let asked = max_step + 1;
    let before = focuser.position().expect("Position");

    if let Err(error) = move_raw(focuser, asked) {
        assert!(!is_binding(&error), "Move({asked}) failed in the binding layer: {error}");
        println!("note: Move({asked}) refused with {:?}", error.kind);
        if error.kind != AscomErrorKind::InvalidValue {
            deviation(&format!(
                "spec wants InvalidValue for Move({asked}), driver answered {:?}",
                error.kind
            ));
        }
    } else {
        // A driver that accepted it may be moving now: hand that back to the guard.
        guard.mark_motion();
        deviation(&format!("Move({asked}) was accepted although MaxStep={max_step}"));
        return;
    }

    // A failed IsMoving read must not stand in for the state under test: defaulting it
    // to false would pass this assertion on a value the driver never reported.
    let moving = focuser
        .is_moving()
        .unwrap_or_else(|error| panic!("IsMoving after a refused Move({asked}): {error}"));
    assert!(!moving, "a refused Move must not start motion");
    assert_eq!(
        focuser.position().expect("Position after a refused Move"),
        before,
        "a refused Move must not change Position"
    );
}

/// Writes issued while disconnected must be answered by the driver, not swallowed or
/// turned into a binding failure.
#[serial]
#[test]
fn writes_while_disconnected_are_refused() {
    let guard = guarded();
    let focuser = guard.focuser();
    guard.mark_connection();
    // Marked in case a lenient driver accepts the TempComp write below.
    guard.mark_temp_comp();
    // Same reason for the motion branch: a driver that accepts Move while disconnected
    // leaves the shared Singleton moving, and the guard only halts and repositions when
    // it knows motion was touched. Marking up front is harmless when the write is refused.
    guard.mark_motion();
    focuser.set_connected(false).expect("disconnect");

    let attempts: [(&str, Result<()>); 2] =
        [("Move", move_raw(focuser, 10)), ("TempComp", focuser.set_temp_comp(true))];
    for (member, outcome) in attempts {
        match outcome {
            Ok(()) => deviation(&format!("{member} was accepted while disconnected")),
            Err(error) => {
                assert!(
                    !is_binding(&error),
                    "{member} while disconnected failed in the binding layer: {error}"
                );
                if !matches!(
                    error.kind,
                    AscomErrorKind::NotConnected
                        | AscomErrorKind::Unsupported
                        | AscomErrorKind::InvalidOperation
                ) {
                    deviation(&format!(
                        "spec wants NotConnected for {member} while disconnected, driver answered {:?}",
                        error.kind
                    ));
                }
            }
        }
    }
    // Reconnecting and verifying the restored state is the guard's job.
}
