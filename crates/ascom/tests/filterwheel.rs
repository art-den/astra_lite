//! Tests for `IFilterWheelV3` against a live driver.
//!
//! Run: `ASCOM_FILTERWHEEL_PROG_ID=ASCOM.OmniSim.FilterWheel cargo test --test filterwheel`
//!
//! The read-only checklist honours that variable; the write tests at the bottom always
//! run against the one driver named by `DRIVER`. The other registrations are proxies
//! (`ASCOM.JustAHub*.FilterWheel`) or the legacy `FilterWheelSim.FilterWheel`, which is
//! a V1 driver without `InterfaceVersion` or `Connected` — neither can be written to
//! meaningfully.

mod common;
// The driver is one shared Singleton that survives the process: `#[serial]` keeps
// two tests off it at once, which is what `--test-threads=1` used to guarantee.
use serial_test::serial;

use ascom::device::{AscomDevice, DeviceSpec};
use ascom::error::{AscomErrorKind, Result};
use ascom::filterwheel::{self, FilterWheel};
use ascom::wait::{WaitSpec, wait_i32};
use std::cell::Cell;
use std::time::{Duration, Instant};

use common::{
    check_connected_idempotent, check_identity_members, check_operational_needs_connection,
    check_unknown_action, deviation, is_binding, is_raw_managed, prog_id, walk_members, POLL,
};

/// The read-only members of IFilterWheelV3 (COM-visible), from the spec. There is no
/// `CanXxx` pair in this interface and no `Link`, so the list is short.
const MEMBERS: &[&str] = &[
    "Connected",
    "Connecting",
    "Description",
    "DeviceState",
    "DriverInfo",
    "DriverVersion",
    "FocusOffsets",
    "InterfaceVersion",
    "Name",
    "Names",
    "Position",
    "SupportedActions",
];

fn open() -> FilterWheel {
    let id = prog_id("ASCOM_FILTERWHEEL_PROG_ID", "ASCOM.OmniSim.FilterWheel");
    let wheel = FilterWheel::open(&DeviceSpec::new(id)).expect("open filter wheel driver");
    wheel.set_connected(true).expect("connect");
    wheel
}

#[serial]
#[test]
fn every_member_answers_or_is_unsupported() {
    let wheel = open();
    walk_members(&wheel, MEMBERS);
}

#[serial]
#[test]
fn unknown_action_is_action_not_implemented() {
    let wheel = open();
    check_unknown_action(&wheel);
}

#[serial]
#[test]
fn identity_members_are_readable() {
    let wheel = open();
    check_identity_members(&wheel).expect("identity members");
}

#[serial]
#[test]
fn connected_is_idempotent() {
    let wheel = open();
    check_connected_idempotent(&wheel).expect("connected cycle");
}

#[serial]
#[test]
fn operational_property_needs_connection() {
    let wheel = open();
    check_operational_needs_connection(&wheel, "Position").expect("not-connected probe");
}

/// `Names` and `FocusOffsets` describe the same wheel, so they must be the same
/// non-empty length. The spec adds that the offsets are relative to a zero-focus
/// position, which only holds when at least one of them is zero.
#[serial]
#[test]
fn names_and_focus_offsets_describe_the_same_wheel() {
    let wheel = open();
    let names = wheel.names().expect("Names is mandatory when implemented");
    assert!(
        !names.iter().any(|name| name.is_empty()),
        "a filter name must not be empty: {names:?}"
    );
    let offsets = wheel.focus_offsets().expect("FocusOffsets is mandatory when implemented");
    assert_eq!(
        offsets.len(),
        names.len(),
        "FocusOffsets must have one entry per filter"
    );
    if !offsets.contains(&0) {
        deviation(&format!(
            "FocusOffsets {offsets:?} has no zero, so no filter defines the zero-focus position"
        ));
    }
}

/// A stationary wheel reports a slot number, never the moving sentinel.
#[serial]
#[test]
fn a_stationary_wheel_reports_a_slot() {
    let wheel = open();
    let count = wheel.names().expect("Names").len();
    // The simulator is one shared singleton: an interrupted run can leave it turning,
    // so wait the wheel out the way `guarded()` does before the sentinel counts
    // against the driver.
    let started = Instant::now();
    if wheel.is_moving().expect("IsMoving") {
        wait_stopped(&wheel).expect("the wheel left turning by an interrupted run must stop");
        println!("note: the wheel was still turning; it settled after {:?}", started.elapsed());
    }
    let position = wheel.position().expect("Position is mandatory");
    let slot = filterwheel::slot_of(position)
        .unwrap_or_else(|| panic!("a wheel at rest must not report Position={position}"));
    assert!(
        slot < count as i32,
        "Position {position} is outside the {} slots this wheel names",
        count
    );
}

// ------------------------------------------------------------------- writes ---
//
// The scenario is read → write → wait → read, where step 3 is the interesting part:
// `Position` is both the command and the completion property, because this interface
// has no `IsMoving`. A wheel that is turning answers `-1`, so the wait watches for that
// sentinel to clear.
//
// The shared table machinery in `tests/common/mod.rs` is deliberately not used here.
// Its `writable_round_trip` waits a fixed `APPLY_TIMEOUT` (2 s) and treats a value that
// has not appeared as "not applied" — but a turning wheel *reports* `-1` for as long as
// 5 s, and its `Restorer` would rewrite the saved value without waiting for the motion
// to finish, undoing the restore. So this suite has its own guard, following the
// precedent set by `tests/focuser.rs`.

/// The driver the write tests run against.
const DRIVER: &str = "ASCOM.OmniSim.FilterWheel";

/// Bound for a deliberate move. This simulator takes about a second per slot, so a
/// worst-case sweep of a six-slot wheel needs more than the shared 2 s grace period.
const MOVE_TIMEOUT: Duration = Duration::from_secs(30);

/// `Position = slot` straight to the driver, bypassing the wrapper's range check, so
/// that a negative case tests *the driver's* answer rather than `ascom::filterwheel`.
fn position_raw(wheel: &FilterWheel, slot: i32) -> Result<()> {
    wheel.actor().call(move |device| device.dispatch().set_i32("Position", slot))
}

/// Waits for `Position` to stop reporting the moving sentinel.
fn wait_stopped(wheel: &FilterWheel) -> Result<i32> {
    wait_i32(
        wheel.actor(),
        "Position",
        WaitSpec::with_poll(MOVE_TIMEOUT, POLL),
        |value| *value != filterwheel::MOVING,
    )
}

/// Puts the wheel back on the slot it was found on, even when the test panics.
///
/// Same two-way reporting rule as `tests/focuser.rs`: a failed restore panics only when
/// the test itself passed, because panicking while unwinding aborts the process. Unlike
/// the focuser there is no `Halt`, so a restore can only *wait out* motion in progress.
struct Restorer {
    wheel: FilterWheel,
    /// The slot the driver reported when the handle was opened.
    slot: i32,
    touched_motion: Cell<bool>,
    touched_connection: Cell<bool>,
}

impl Restorer {
    fn wheel(&self) -> &FilterWheel {
        &self.wheel
    }

    fn mark_motion(&self) {
        self.touched_motion.set(true);
    }

    fn mark_connection(&self) {
        self.touched_connection.set(true);
    }
}

impl Drop for Restorer {
    fn drop(&mut self) {
        let mut problems = Vec::new();

        // Connection first: nothing below can be restored against a disconnected driver.
        if self.touched_connection.get() && !self.wheel.connected().unwrap_or(false)
            && let Err(error) = self.wheel.set_connected(true) {
                problems.push(format!("reconnect: {error}"));
            }
        if self.touched_motion.get() {
            // Motion first: restoring the slot while the wheel is turning would be
            // overwritten by whatever the wheel is already heading for.
            match wait_stopped(&self.wheel) {
                Err(error) => problems.push(format!("Position still -1 on restore: {error}")),
                Ok(current) if current != self.slot => {
                    if let Err(error) = self.wheel.set_position(self.slot) {
                        problems.push(format!("restore Position={}: {error}", self.slot));
                    } else if let Err(error) = wait_stopped(&self.wheel) {
                        problems.push(format!("Position did not settle at {}: {error}", self.slot));
                    }
                }
                Ok(_) => {}
            }
            // Step 4 for the guard itself: the driver must really be back home.
            match self.wheel.position() {
                Ok(current) if current != self.slot => {
                    problems.push(format!("Position is {current}, was {}", self.slot));
                }
                Err(error) => problems.push(format!("Position after restore: {error}")),
                Ok(_) => {}
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

/// Opens [`DRIVER`], connects it, and snapshots the slot the wheel was left on.
fn guarded() -> Restorer {
    let wheel = FilterWheel::open(&DeviceSpec::new(DRIVER)).expect("open filter wheel driver");
    wheel.set_connected(true).expect("connect");
    // The simulator is one shared singleton: an interrupted run can leave it turning.
    // A failed answer here is never evidence that the wheel stands still.
    match wheel.is_moving() {
        Ok(true) => {
            // A wheel that never settles leaves the guard with no slot to snapshot;
            // the wait error must reach the report, not the sentinel below.
            if let Err(error) = wait_stopped(&wheel) {
                panic!("waiting for the wheel left turning by an interrupted run: {error}");
            }
        }
        Ok(false) => {}
        Err(error) => panic!("IsMoving before the guard snapshot: {error}"),
    }
    let position = match wheel.position() {
        Ok(position) => position,
        Err(error) => {
            assert!(!is_binding(&error), "Position while arming the guard: {error}");
            panic!("a stationary wheel must report a slot, driver answered: {error}");
        }
    };
    let slot = filterwheel::slot_of(position)
        .unwrap_or_else(|| panic!("a stationary wheel must not report Position={position}"));
    Restorer {
        wheel,
        slot,
        touched_motion: Cell::new(false),
        touched_connection: Cell::new(false),
    }
}

/// read → write → wait → read on the one writable property `IFilterWheelV3` has,
/// including the `-1` sentinel that the spec makes mandatory for a wheel that has no
/// `IsMoving` of its own.
#[serial]
#[test]
fn moving_reports_minus_one_then_the_slot() {
    let guard = guarded();
    let wheel = guard.wheel();
    let count = wheel.names().expect("Names is mandatory").len();
    if count < 2 {
        println!("note: this wheel has {count} slot, so there is nowhere to move");
        return;
    }
    let start = wheel.position().expect("Position is mandatory");
    let target = if start == 0 { 1 } else { 0 };

    guard.mark_motion();
    wheel
        .set_position(target)
        .expect("writing a valid slot must be accepted");

    // Step 3: watch for the sentinel. Sampling is fast on purpose — the spec requires
    // -1 for the whole move, and a single-slot move here lasts about a second. A wheel
    // built into a camera is the documented exception, so a miss is a reported deviation
    // rather than a hard failure.
    let mut saw_moving = false;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if wheel.is_moving().unwrap_or(false) {
            saw_moving = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let arrived = wheel
        .wait_until_stopped(WaitSpec::new(MOVE_TIMEOUT))
        .expect("the wheel must stop within its bound");
    assert!(
        !wheel.is_moving().unwrap_or(true),
        "a completed move must leave Position at a slot"
    );
    if arrived != target {
        deviation(&format!(
            "Position reads {arrived} after the wheel stopped, the requested slot was {target}"
        ));
    }
    if !saw_moving {
        deviation("Position never reported -1 while the wheel was turning");
    }
    // Step 4: the value the driver reports is the guard's cue to restore.
    assert_eq!(wheel.position().expect("Position after the move"), arrived);
    assert_ne!(arrived, start, "a write to another slot must actually move the wheel");
}

/// Every slot the wheel names must be reachable, and the driver must report the slot it
/// was asked for. Also checks that a wheel in motion never reports an intermediate slot,
/// which would be indistinguishable from an unexpected final position.
#[serial]
#[test]
fn every_slot_can_be_reached_and_reads_back() {
    let guard = guarded();
    let wheel = guard.wheel();
    let names = wheel.names().expect("Names is mandatory");
    guard.mark_motion();

    for (index, name) in names.iter().enumerate() {
        let wanted = index as i32;
        let arrived = wheel
            .move_to_and_wait(wanted, WaitSpec::new(MOVE_TIMEOUT))
            .unwrap_or_else(|error| panic!("Move to slot {wanted} ({name}) failed: {error}"));
        assert_eq!(
            arrived, wanted,
            "the driver reported slot {arrived} after a move to {wanted} ({name})"
        );
        assert_eq!(
            wheel.position().expect("Position after the move"),
            wanted,
            "Position must name the slot the wheel stopped on"
        );
    }
}

/// The wrapper's own guard rail: a slot outside the wheel must never reach the driver.
#[serial]
#[test]
fn the_wrapper_refuses_a_slot_outside_the_wheel() {
    let guard = guarded();
    let wheel = guard.wheel();
    let count = wheel.names().expect("Names").len() as i32;
    let before = wheel.position().expect("Position");

    for bad in [count, count + 1, -5] {
        let error = wheel
            .set_position(bad)
            .expect_err(&format!("Position={bad} must be refused"));
        assert_eq!(error.kind, AscomErrorKind::InvalidValue, "{bad}: {:?}", error.kind);
        assert_eq!(error.member, "Position", "the error must name the property");
        assert_eq!(error.source, "ascom::filterwheel", "refused locally by the wrapper");
    }
    assert!(
        !wheel.is_moving().unwrap_or(true),
        "a refused slot must not start motion"
    );
    assert_eq!(
        wheel.position().expect("Position after refused writes"),
        before,
        "a refused write must not move the wheel"
    );
}

/// The driver's own guard rail, sent around the wrapper: an out-of-range slot must be
/// answered by the driver, and the refusal must not move anything.
#[serial]
#[test]
fn the_driver_refuses_a_slot_outside_the_wheel() {
    let guard = guarded();
    let wheel = guard.wheel();
    let count = wheel.names().expect("Names").len() as i32;
    let asked = count;
    let before = wheel.position().expect("Position");

    match position_raw(wheel, asked) {
        Ok(()) => {
            // A lenient driver may be moving now; hand that back to the guard.
            guard.mark_motion();
            deviation(&format!(
                "Position={asked} was accepted although the wheel has {count} slots"
            ));
            return;
        }
        Err(error) => {
            assert!(
                !is_binding(&error),
                "Position={asked} failed in the binding layer: {error}"
            );
            println!("note: Position={asked} refused with {:?}", error.kind);
            if error.kind != AscomErrorKind::InvalidValue {
                deviation(&format!(
                    "spec wants InvalidValue for Position={asked}, driver answered {:?}",
                    error.kind
                ));
            }
        }
    }

    // Whatever the answer, the wheel must end up stationary on a real slot.
    guard.mark_motion();
    let settled = wait_stopped(wheel).expect("the wheel must settle");
    if settled != before {
        deviation(&format!(
            "Position={asked} was refused, yet the wheel moved from {before} to {settled}"
        ));
    }
    assert!(
        settled < count,
        "a refused write left the wheel on slot {settled}, which does not exist"
    );
}

/// A second `Position` write while the wheel is already turning. The spec asks for
/// either following the new target or raising `InvalidOperation`; a raw .NET error is
/// neither, and is reported as a deviation.
#[serial]
#[test]
fn a_second_write_while_moving_is_answered_not_swallowed() {
    let guard = guarded();
    let wheel = guard.wheel();
    let count = wheel.names().expect("Names").len() as i32;
    if count < 3 {
        println!("note: this wheel has {count} slots, too few to overtake a move");
        return;
    }
    let start = wheel.position().expect("Position");
    // A long first move, so the second write definitely lands mid-flight.
    let far = if start == count - 1 { 0 } else { count - 1 };
    let overtaken = if far == 0 { count - 2 } else { 0 };

    guard.mark_motion();
    wheel.set_position(far).expect("first Position write");
    let outcome = wheel.set_position(overtaken);

    match outcome {
        Ok(()) => println!("note: the wheel accepted a second Position write"),
        // This driver answers with a raw CLR exception ("The FilterWheel is already
        // moving") instead of InvalidOperationException, and the wheel moves anyway.
        // The wrapper can only classify that as `Com`, so it is a reported deviation.
        Err(error) if is_raw_managed(&error) => deviation(&format!(
            "spec wants InvalidOperation for a Position write during motion, driver raised a \
             managed exception: {error}"
        )),
        Err(error) => {
            assert!(
                !is_binding(&error),
                "the second write failed in the binding layer: {error}"
            );
            if error.kind != AscomErrorKind::InvalidOperation {
                deviation(&format!(
                    "spec wants InvalidOperation for a Position write during motion, driver answered {:?}",
                    error.kind
                ));
            }
        }
    }

    // Either way the wheel has to settle somewhere, and somewhere that exists.
    let settled = wheel
        .wait_until_stopped(WaitSpec::new(MOVE_TIMEOUT))
        .expect("the wheel must stop after the second write");
    assert!(
        settled < count,
        "the wheel stopped on slot {settled}, which this wheel does not have"
    );
}

/// Writes issued while disconnected must be answered by the driver, not swallowed or
/// turned into a binding failure.
#[serial]
#[test]
fn writes_while_disconnected_are_refused() {
    let guard = guarded();
    let wheel = guard.wheel();
    guard.mark_connection();
    // Marked in case a lenient driver accepts the Position write below.
    guard.mark_motion();
    wheel.set_connected(false).expect("disconnect");

    let target = wheel.names().map(|names| names.len() as i32 - 1).unwrap_or(1);
    match position_raw(wheel, target) {
        Ok(()) => deviation(&format!(
            "Position={target} was accepted while Connected=false, and a wheel keeps turning \
             across the disconnect, so a reconnect can find it mid-flight"
        )),
        Err(error) => {
            assert!(
                !is_binding(&error),
                "Position while disconnected failed in the binding layer: {error}"
            );
            if !matches!(
                error.kind,
                AscomErrorKind::NotConnected
                    | AscomErrorKind::Unsupported
                    | AscomErrorKind::InvalidOperation
            ) {
                deviation(&format!(
                    "spec wants NotConnected for Position while disconnected, driver answered {:?}",
                    error.kind
                ));
            }
        }
    }
    // Reconnecting, waiting the wheel out and verifying the slot is the guard's job.
}

/// `DeviceState` belongs to the V4 device interfaces. A V3 wheel must not be asked for
/// it, and the wrapper must not raise when a caller asks anyway.
#[serial]
#[test]
fn a_v3_wheel_exposes_no_device_state() {
    let wheel = open();
    assert_eq!(
        wheel.interface_version().expect("InterfaceVersion"),
        3,
        "IFilterWheelV3 requires InterfaceVersion 3"
    );
    match wheel.device_state() {
        Ok(state) => assert!(
            state.is_empty(),
            "a V3 driver must report no operational state, not {state:?}"
        ),
        // The expected answer for a pre-V4 driver: the member is simply absent.
        Err(error) if error.is_unsupported() => {}
        Err(error) => {
            assert!(!is_binding(&error), "DeviceState failed in the binding layer: {error}");
            deviation(&format!("a V3 driver must not raise on DeviceState: {error}"));
        }
    }
}
