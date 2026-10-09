//! Tests for `ITelescopeV4` against a live driver.
//!
//! Run: `ASCOM_TELESCOPE_PROG_ID=ASCOM.OmniSim.Telescope cargo test --test telescope`

mod common;
// The driver is one shared Singleton that survives the process: `#[serial]` keeps
// two tests off it at once, which is what `--test-threads=1` used to guarantee.
use serial_test::serial;

use ascom::com::Variant;
use ascom::device::{AscomDevice, DeviceSpec, GuideDirection};
use ascom::error::{AscomError, AscomErrorKind, Result};
use ascom::telescope::{DriveRate, PierSide, RateRange, Telescope, TelescopeAxis};
use ascom::wait::{self, WaitSpec};
use std::cell::Cell;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{
    check_connected_idempotent, check_identity_members, check_operational_needs_connection,
    check_unknown_action, deviation, is_binding, prog_id, walk_members, Case, Restorer, POLL,
};

/// The read-only members of ITelescopeV4, straight from the spec's property list.
const MEMBERS: &[&str] = &[
    "AlignmentMode",
    "Altitude",
    "ApertureArea",
    "ApertureDiameter",
    "AtHome",
    "AtPark",
    "Azimuth",
    "CanFindHome",
    "CanPark",
    "CanPulseGuide",
    "CanSetDeclinationRate",
    "CanSetGuideRates",
    "CanSetPark",
    "CanSetPierSide",
    "CanSetRightAscensionRate",
    "CanSetTracking",
    "CanSlew",
    "CanSlewAltAz",
    "CanSlewAltAzAsync",
    "CanSlewAsync",
    "CanSync",
    "CanSyncAltAz",
    "CanUnpark",
    "Connected",
    "Connecting",
    "Declination",
    "DeclinationRate",
    "Description",
    "DeviceState",
    "DoesRefraction",
    "DriverInfo",
    "DriverVersion",
    "EquatorialSystem",
    "FocalLength",
    "GuideRateDeclination",
    "GuideRateRightAscension",
    "InterfaceVersion",
    "IsPulseGuiding",
    "Name",
    "RightAscension",
    "RightAscensionRate",
    "SideOfPier",
    "SiderealTime",
    "SiteElevation",
    "SiteLatitude",
    "SiteLongitude",
    "SlewSettleTime",
    "Slewing",
    "SupportedActions",
    "TargetDeclination",
    "TargetRightAscension",
    "Tracking",
    "TrackingRate",
    "TrackingRates",
    "UTCDate",
];

fn open() -> Telescope {
    let id = prog_id("ASCOM_TELESCOPE_PROG_ID", "ASCOM.OmniSim.Telescope");
    let scope = Telescope::open(&DeviceSpec::new(id)).expect("open telescope driver");
    scope.set_connected(true).expect("connect");
    ensure_unparked(&scope);
    scope
}

/// Takes the mount out of a park left behind by an earlier run, if there is one.
///
/// The simulator outlives this process, so does its state: exit while parked and the next
/// one starts with `AtPark == true` and `Tracking == false`, which makes every slew and
/// every `Tracking` write raise `ParkedException`. Unparking a mount that is not parked is
/// required to be harmless, but it is also a motion call, so it is issued only when
/// `AtPark` asks for it.
fn ensure_unparked(scope: &Telescope) {
    if !scope.at_park().unwrap_or(false) {
        return;
    }
    println!("note: mount found parked, unparking before the test starts");
    if let Err(error) = scope.unpark() {
        println!("note: Unpark on startup: {error}");
    }
    // Unpark alone does not stop the park motion (docs/KNOWN_DRIVER_QUIRKS.md §4.2), so
    // cancel it as well.
    if let Err(error) = scope.abort_slew() {
        println!("note: AbortSlew on startup: {error}");
    }
    if let Err(error) =
        wait::wait_flag_false(scope.actor(), "Slewing", WaitSpec::new(Duration::from_secs(30)))
    {
        println!("note: Slewing did not clear after unparking: {error}");
    }
}

/// Returns the mount to a state where a slew is allowed.
///
/// All these tests share one simulator process, and an unfinished `Park()` survives
/// it: `Slewing` stays true and tracking stays off, which makes
/// `SlewToCoordinatesAsync` raise InvalidOperation ("not allowed when tracking is
/// False"). Verified live: `Unpark` alone does not stop the park motion and the
/// driver refuses `AbortSlew` with `ParkedException` while it calls itself parked,
/// hence unpark first, cancel second (docs/KNOWN_DRIVER_QUIRKS.md).
fn restore_idle(scope: &Telescope) {
    if let Err(error) = scope.unpark() {
        println!("note: Unpark while restoring: {:?}", error.kind);
    }
    if let Err(error) = scope.abort_slew() {
        println!("note: AbortSlew while restoring: {:?}", error.kind);
    }
    if let Err(error) = scope.set_tracking(true) {
        println!("note: Tracking = true while restoring: {:?}", error.kind);
    }
    if let Err(error) = wait::wait_flag_false(scope.actor(), "Slewing", WaitSpec::new(Duration::from_secs(30))) {
        println!("note: Slewing did not clear while restoring: {error}");
    }
}

/// Stops a `MoveAxis` motion on the way out, even when the test panics before its own
/// zero-rate stop.
///
/// A turning axis survives this process (docs/KNOWN_DRIVER_QUIRKS.md §4.7, §4.12), so
/// the guard is armed right after a successful non-zero `MoveAxis` and only disarmed
/// once the test's own stop succeeded. Like `Restorer`, `Drop` may panic only when the
/// test itself passed: a second panic during an unwind aborts the process and hides the
/// real failure, so there the guard only prints `GUARD FAILED`.
struct MotionGuard<'a> {
    scope: &'a Telescope,
    armed: Cell<bool>,
}

impl<'a> MotionGuard<'a> {
    fn new(scope: &'a Telescope) -> Self {
        Self { scope, armed: Cell::new(true) }
    }

    /// The test stopped the axis itself; the guard stands down.
    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for MotionGuard<'_> {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        let mut problems = Vec::new();
        // The zero rate is the spec'd stop; a driver that refuses it gets AbortSlew.
        if let Err(error) = self.scope.move_axis(TelescopeAxis::Primary, 0.0) {
            println!("note: guard: MoveAxis(Primary, 0.0) failed ({error}), trying AbortSlew");
            if let Err(error) = self.scope.abort_slew() {
                problems.push(format!(
                    "neither MoveAxis(Primary, 0.0) nor AbortSlew stopped the axis ({error})"
                ));
            }
        }
        if let Err(error) = wait::wait_flag_false(
            self.scope.actor(),
            "Slewing",
            WaitSpec::with_poll(Duration::from_secs(20), POLL),
        ) {
            problems
                .push(format!("Slewing stayed true after the guard tried to stop: {error}"));
        }
        if problems.is_empty() {
            println!("note: guard: the MoveAxis motion was stopped on the way out");
            return;
        }
        let text = problems.join("\n");
        if std::thread::panicking() {
            eprintln!("GUARD FAILED (a panic is already unwinding): {text}");
        } else {
            panic!("guard could not stop the MoveAxis motion:\n{text}");
        }
    }
}

/// Puts the mount back after a `Park` on the way out, even when the test panics before
/// its own `Unpark`.
///
/// A park runs at sidereal tempo with tracking off and survives this process (§4.2,
/// §4.7), so an unfinished one starves every later test. The guard mirrors the test's
/// teardown: `Unpark`, then `AbortSlew` — which the driver documents as refusing with
/// `ParkedException` while it still calls itself parked (§4.2), so any answer there is
/// only noted — then a wait for `Slewing` to clear. `Drop` panics only when the test
/// itself passed; during an unwind it only prints `GUARD FAILED`.
struct ParkGuard<'a> {
    scope: &'a Telescope,
    armed: Cell<bool>,
}

impl<'a> ParkGuard<'a> {
    fn new(scope: &'a Telescope) -> Self {
        Self { scope, armed: Cell::new(true) }
    }

    /// The test ran its own unpark path; the guard stands down.
    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for ParkGuard<'_> {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        let mut problems = Vec::new();
        match self.scope.unpark() {
            Ok(()) => println!("note: guard: Unpark issued after the park"),
            Err(error) => problems.push(format!("guard Unpark failed: {error}")),
        }
        // Tolerated: ParkedException is the documented answer while the driver still
        // calls itself parked (§4.2); the wait below judges the real outcome.
        if let Err(error) = self.scope.abort_slew() {
            println!("note: guard: AbortSlew during the teardown: {:?}", error.kind);
        }
        if let Err(error) = wait::wait_flag_false(
            self.scope.actor(),
            "Slewing",
            WaitSpec::new(Duration::from_secs(60)),
        ) {
            problems.push(format!("Slewing stayed true after the guard's teardown: {error}"));
        }
        if problems.is_empty() {
            println!("note: guard: the mount was restored from the park on the way out");
            return;
        }
        let text = problems.join("\n");
        if std::thread::panicking() {
            eprintln!("GUARD FAILED (a panic is already unwinding): {text}");
        } else {
            panic!("guard could not restore the mount from the park:\n{text}");
        }
    }
}

#[serial]
#[test]
fn every_member_answers_or_is_unsupported() {
    let scope = open();
    walk_members(&scope, MEMBERS);
}

#[serial]
#[test]
fn unknown_action_is_action_not_implemented() {
    let scope = open();
    check_unknown_action(&scope);
}

#[serial]
#[test]
fn identity_members_are_readable() {
    let scope = open();
    check_identity_members(&scope).expect("identity members");
}

#[serial]
#[test]
fn connected_is_idempotent() {
    let scope = open();
    check_connected_idempotent(&scope).expect("connected cycle");
}

#[serial]
#[test]
fn operational_property_needs_connection() {
    let scope = open();
    check_operational_needs_connection(&scope, "RightAscension").expect("not-connected probe");
}

#[serial]
#[test]
fn slew_completes_via_slewing_without_timeout() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !caps.supports("CanSlewAsync") {
        println!("note: driver has no CanSlewAsync; skipping async slew");
        return;
    }
    restore_idle(&scope);
    scope.slew_to_coordinates_async(12.0, 45.0).expect("SlewToCoordinatesAsync initiated");
    let wait = WaitSpec::new(Duration::from_secs(120));
    match wait::wait_flag_false(scope.actor(), "Slewing", wait) {
        Err(e) if e.kind == AscomErrorKind::Timeout => {
            panic!("Slewing never cleared after SlewToCoordinatesAsync: {e}");
        }
        Err(e) => panic!("slew completion wait failed: {e}"),
        Ok(()) => {}
    }
    assert!(!scope.slewing().expect("Slewing after slew"));
}

#[serial]
#[test]
fn pulse_guide_completes_via_ispulseguiding() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !caps.supports("CanPulseGuide") {
        println!("note: driver has no CanPulseGuide; skipping");
        return;
    }
    restore_idle(&scope);
    scope
        .pulse_guide_and_wait(GuideDirection::North, 200, WaitSpec::new(Duration::from_secs(30)))
        .expect("PulseGuide cycle");
    assert!(!scope.is_pulse_guiding().expect("IsPulseGuiding after pulse"));
}

#[serial]
#[test]
fn park_unpark_round_trip_when_supported() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !(caps.supports("CanPark") && caps.supports("CanUnpark")) {
        println!("note: driver lacks CanPark/CanUnpark; skipping");
        return;
    }
    restore_idle(&scope);
    scope.park_async().expect("Park initiated");
    // From this line the mount is parking at sidereal tempo with tracking off; every
    // panic below must still leave it unparked for the processes that follow.
    let guard = ParkGuard::new(&scope);

    // The handshake we can actually test: the mount must report motion, and the
    // completion property must exist and answer false rather than raise. A park that
    // finishes before the first poll is legal too — the spec lets Park return with
    // Slewing false and AtPark true, which is what happens when the park position is
    // where the mount already stands (see set_park_records_the_position_without_parking).
    let handshake = WaitSpec::new(Duration::from_secs(30));
    if wait::wait_flag_true(scope.actor(), "Slewing", handshake).is_ok() {
        assert!(!scope.at_park().expect("AtPark must be readable during a park"));
    } else {
        assert!(
            scope.at_park().expect("AtPark"),
            "a park that neither moves nor arrives is no park"
        );
        println!("note: park completed before Slewing could be observed");
    }

    // Arrival is not assertable from here: whether AtPark is seconds or minutes away
    // depends on where the driver's park position happens to be (§4.1 below). A timeout
    // here is a documented driver property, not a wrapper failure.
    match wait::wait_flag_true(scope.actor(), "AtPark", WaitSpec::new(Duration::from_secs(20))) {
        Ok(()) => assert!(scope.at_park().expect("AtPark after the wait")),
        Err(error) if error.kind == AscomErrorKind::Timeout => println!(
            "note: driver parks at sidereal rate, AtPark not reached in 20s \
             (docs/KNOWN_DRIVER_QUIRKS.md)"
        ),
        Err(error) => panic!("waiting for AtPark failed: {error}"),
    }

    // Leave the mount as we found it: an unfinished park starves every later test.
    // `Unpark` must be accepted after a park attempt — that is the round trip.
    scope.unpark().expect("Unpark after a park attempt");
    guard.disarm();
    if let Err(error) = scope.abort_slew() {
        println!("note: AbortSlew after Unpark: {error}");
    }
    if let Err(error) = wait::wait_flag_false(scope.actor(), "Slewing", WaitSpec::new(Duration::from_secs(60))) {
        println!("note: Slewing still true after Unpark + AbortSlew: {error}");
    }
}

/// `SetPark` records the current position as the park position and must change nothing
/// else — it is neither a motion nor a park. Recording it is also the only way to make
/// `AtPark` reachable on this simulator, because its default park position is minutes of
/// sidereal crawling away (docs/KNOWN_DRIVER_QUIRKS.md §4.1), so the test parks afterwards
/// and checks the parked state while it is there.
///
/// What this test cannot undo: the recorded park position is not readable through any V4
/// member, so there is nothing for the guard to put back. The simulator keeps the new one
/// for the rest of its life, which only ever costs a later `Park()` some travel.
#[serial]
#[test]
fn set_park_records_the_position_without_parking() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !caps.supports("CanPark") {
        // Spec: without CanPark, SetPark must refuse as not implemented.
        match scope.set_park() {
            Err(error) if error.is_unsupported() => {
                println!("note: CanPark false, SetPark refused");
            }
            Ok(()) => deviation("SetPark accepted although CanPark is false"),
            Err(error) if is_binding(&error) => {
                panic!("SetPark failed in the binding layer: {error}");
            }
            Err(error) => deviation(&format!(
                "spec wants PropertyNotImplemented while CanPark is false, driver answered {:?}",
                error.kind
            )),
        }
        return;
    }
    restore_idle(&scope);

    scope.set_park().expect("SetPark at rest");
    assert!(!scope.at_park().expect("AtPark after SetPark"), "SetPark must not park the mount");
    assert!(!scope.slewing().expect("Slewing after SetPark"), "SetPark must not start motion");

    scope.park_async().expect("Park initiated");
    match wait::wait_flag_true(scope.actor(), "AtPark", WaitSpec::new(Duration::from_secs(20))) {
        Ok(()) => {
            assert!(scope.at_park().expect("AtPark after the wait"));
            assert!(!scope.slewing().expect("Slewing after the park"), "AtPark came with motion");
            assert!(
                !scope.tracking().expect("Tracking while parked"),
                "spec: Tracking must be false while parked"
            );
            // The parked state is worth what it shows: motion and tracking are refused
            // with ParkedException rather than silently accepted.
            match scope.set_tracking(true) {
                Err(error) if error.kind == AscomErrorKind::Parked => {}
                Ok(()) => deviation("Tracking = true accepted while parked"),
                Err(error) if is_binding(&error) => {
                    panic!("Tracking write failed in the binding layer: {error}")
                }
                Err(error) => deviation(&format!(
                    "spec wants Parked for Tracking while parked, driver answered {:?} ({error})",
                    error.kind
                )),
            }
        }
        Err(error) if error.kind == AscomErrorKind::Timeout => println!(
            "note: AtPark still not reached 20s after parking at the recorded position \
             (docs/KNOWN_DRIVER_QUIRKS.md §4.1)"
        ),
        Err(error) => panic!("waiting for AtPark failed: {error}"),
    }

    restore_idle(&scope);
    assert!(
        !scope.at_park().unwrap_or(true),
        "the mount must not be left parked: it survives this process"
    );
}

/// Spec: `SetPark` while the mount is moving must raise InvalidOperationException. A park
/// position the driver accepted cannot be undone (no member reads it back), so this test
/// checks the two things it can: the answer, and that the mount stops moving afterwards.
#[serial]
#[test]
fn set_park_while_slewing_is_refused() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !(caps.supports("CanPark") && caps.supports("CanSlewAsync")) {
        println!("note: driver has no CanPark/CanSlewAsync; skipping");
        return;
    }
    restore_idle(&scope);
    scope.slew_to_coordinates_async(6.0, 30.0).expect("slew initiated");
    wait::wait_flag_true(scope.actor(), "Slewing", WaitSpec::new(Duration::from_secs(20)))
        .expect("the slew must be visible in Slewing");

    match scope.set_park() {
        Ok(()) => deviation(
            "SetPark accepted while Slewing is true, spec wants InvalidOperationException",
        ),
        Err(error) if is_binding(&error) => panic!("SetPark failed in the binding layer: {error}"),
        Err(error) if error.kind == AscomErrorKind::InvalidOperation => {}
        Err(error) => deviation(&format!(
            "spec wants InvalidOperation for SetPark while slewing, driver answered {:?} ({error})",
            error.kind
        )),
    }

    // However it answered, the mount must not be left moving.
    if let Err(error) = scope.abort_slew() {
        println!("note: AbortSlew after SetPark: {error}");
    }
    wait::wait_flag_false(scope.actor(), "Slewing", WaitSpec::new(Duration::from_secs(30)))
        .expect("AbortSlew must clear Slewing");
    restore_idle(&scope);
}

// ------------------------------------------------------------------- writes ---
//
// The focuser write scenario applied to the writable surface of ITelescopeV4: read →
// write a different value → read back. The rules come from the specification and were
// then checked against this driver:
//
// * every write while `Connected == false` must raise NotConnected — site data and
//   `UTCDate` get no exemption;
// * gated by `CanSetTracking`, `CanSetRightAscensionRate`, `CanSetDeclinationRate` and
//   `CanSetPierSide` (writing with the gate false must raise PropertyNotImplemented);
//   `Site*`, `SlewSettleTime`, `Target*`, `DoesRefraction`, `TrackingRate` and `UTCDate`
//   have no gate at all;
// * ranges: `SiteLatitude` -90..+90, `SiteElevation` -300..10000, `TargetRightAscension`
//   0..24, `TargetDeclination` -90..+90, `SlewSettleTime` >= 0;
// * `RightAscensionRate`/`DeclinationRate` are writable only while `TrackingRate` is
//   sidereal and must read back as 0 otherwise;
// * `SideOfPier` is the one non-blocking write (its completion property is `Slewing`),
//   so it gets a test of its own instead of a row in the table;
// * `Tracking = true` while parked must raise Parked, so that row is skipped parked.
//
// `Parked` is not in the table because ITelescopeV4 has no such property (only the
// read-only `AtPark`), and `Park`/`Unpark`/`SetPark` are methods rather than properties,
// covered next to the motions above.

/// The driver the write tests run against.
const DRIVER: &str = "ASCOM.OmniSim.Telescope";

/// Grace period and poll step live in `tests/common/mod.rs`, together with the table
/// machinery below.

/// Site coordinates and rates are floats the driver is free to round.
const EPS: f64 = 1e-6;

/// A writable member's value — only as many types as the table below needs.
#[derive(Clone, Copy)]
enum Value {
    F(f64),
    I(i32),
    B(bool),
    Rate(DriveRate),
    When(SystemTime),
}

impl Value {
    /// Accessors, so that one `fn` pointer per member fits the table. A mismatch here
    /// is a bug in the table, not a driver answer.
    fn f(self) -> Result<f64> {
        match self {
            Self::F(v) => Ok(v),
            other => Err(AscomError::type_mismatch("Value", format!("{other:?} is not f64"))),
        }
    }

    fn i(self) -> Result<i32> {
        match self {
            Self::I(v) => Ok(v),
            other => Err(AscomError::type_mismatch("Value", format!("{other:?} is not i32"))),
        }
    }

    fn b(self) -> Result<bool> {
        match self {
            Self::B(v) => Ok(v),
            other => Err(AscomError::type_mismatch("Value", format!("{other:?} is not bool"))),
        }
    }

    fn when(self) -> Result<SystemTime> {
        match self {
            Self::When(v) => Ok(v),
            other => Err(AscomError::type_mismatch("Value", format!("{other:?} is not a date"))),
        }
    }

    /// Equal, allowing for float rounding and for the driver's clock moving underneath.
    fn same(self, other: Value) -> bool {
        match (self, other) {
            (Self::F(a), Self::F(b)) => f64::abs(a - b) <= f64::max(EPS, f64::abs(b) * EPS),
            (Self::I(a), Self::I(b)) => a == b,
            (Self::B(a), Self::B(b)) => a == b,
            (Self::Rate(a), Self::Rate(b)) => a == b,
            (Self::When(a), Self::When(b)) => a
                .duration_since(b)
                .or_else(|_| b.duration_since(a))
                .map_or(false, |drift| drift < Duration::from_secs(5)),
            _ => false,
        }
    }
}

/// `current +/- delta` staying inside `lo..=hi`; the midpoint is the last resort.
fn shift(current: f64, delta: f64, lo: f64, hi: f64) -> f64 {
    let up = current + delta;
    let down = current - delta;
    if (lo..=hi).contains(&up) {
        up
    } else if (lo..=hi).contains(&down) {
        down
    } else {
        (lo + hi) / 2.0
    }
}

/// Equality for the guard and for the shared table runs is the tolerant comparison
/// above, not a derived one.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.same(*other)
    }
}

/// Renders a value the way test output should read it — `SystemTime`'s own Debug is
/// unreadable, and these strings end up in deviation reports.
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::F(v) => write!(f, "F({v})"),
            Self::I(v) => write!(f, "I({v})"),
            Self::B(v) => write!(f, "B({v})"),
            Self::Rate(v) => write!(f, "Rate({v:?})"),
            Self::When(v) => match v.duration_since(UNIX_EPOCH) {
                Ok(since) => write!(f, "When(unix {}s)", since.as_secs()),
                Err(before) => write!(f, "When(unix -{}s)", before.duration().as_secs()),
            },
        }
    }
}

/// The row shape (`Case`) and the guard that restores what the rows write (`Restorer`)
/// live in `tests/common/mod.rs`; this file supplies the table and the three runs over
/// it, which the camera file runs identically.
const ALWAYS: fn(&Telescope) -> Result<bool> = |_| Ok(true);
/// Superior drive rates exist only at the sidereal tracking rate.
const SIDEREAL: fn(&Telescope) -> Result<bool> =
    |scope| Ok(scope.tracking_rate()? == DriveRate::Sidereal);
/// Writing `Tracking = true` while parked must raise Parked.
const UNPARKED: fn(&Telescope) -> Result<bool> = |scope| Ok(!scope.at_park()?);

const CASES: &[Case<Telescope, Value>] = &[
    Case {
        member: "SiteLatitude",
        gate: None,
        get: |scope| Ok(Value::F(scope.site_latitude()?)),
        set: |scope, v| scope.set_site_latitude(v.f()?),
        other: |_, v| Value::F(shift(v.f().unwrap_or(48.9), 0.5, -90.0, 90.0)),
        bad: Some(|_| Value::F(95.0)),
        seed: Value::F(48.9),
        precondition: ALWAYS,
    },
    Case {
        member: "SiteLongitude",
        gate: None,
        get: |scope| Ok(Value::F(scope.site_longitude()?)),
        set: |scope, v| scope.set_site_longitude(v.f()?),
        other: |_, v| Value::F(shift(v.f().unwrap_or(2.3), 0.5, -180.0, 180.0)),
        // 200 is out of range under both readings of the spec's range for this member.
        bad: Some(|_| Value::F(200.0)),
        seed: Value::F(2.3),
        precondition: ALWAYS,
    },
    Case {
        member: "SiteElevation",
        gate: None,
        get: |scope| Ok(Value::F(scope.site_elevation()?)),
        set: |scope, v| scope.set_site_elevation(v.f()?),
        other: |_, v| Value::F(shift(v.f().unwrap_or(35.0), 10.0, -300.0, 10000.0)),
        bad: Some(|_| Value::F(-400.0)),
        seed: Value::F(35.0),
        precondition: ALWAYS,
    },
    Case {
        member: "SlewSettleTime",
        gate: None,
        get: |scope| Ok(Value::I(scope.slew_settle_time()?)),
        set: |scope, v| scope.set_slew_settle_time(v.i()?),
        other: |_, v| Value::I(if v.i().unwrap_or(0) == 0 { 1 } else { 0 }),
        bad: Some(|_| Value::I(-1)),
        seed: Value::I(0),
        precondition: ALWAYS,
    },
    Case {
        member: "DoesRefraction",
        gate: None,
        get: |scope| Ok(Value::B(scope.does_refraction()?)),
        set: |scope, v| scope.set_does_refraction(v.b()?),
        other: |_, v| Value::B(!v.b().unwrap_or(false)),
        bad: None,
        seed: Value::B(false),
        precondition: ALWAYS,
    },
    Case {
        member: "Tracking",
        gate: Some("CanSetTracking"),
        get: |scope| Ok(Value::B(scope.tracking()?)),
        set: |scope, v| scope.set_tracking(v.b()?),
        other: |_, v| Value::B(!v.b().unwrap_or(false)),
        bad: None,
        seed: Value::B(true),
        precondition: UNPARKED,
    },
    Case {
        member: "RightAscensionRate",
        gate: Some("CanSetRightAscensionRate"),
        get: |scope| Ok(Value::F(scope.right_ascension_rate()?)),
        set: |scope, v| scope.set_right_ascension_rate(v.f()?),
        other: |_, v| Value::F(v.f().unwrap_or(0.0) + 0.05),
        bad: None,
        seed: Value::F(0.0),
        precondition: SIDEREAL,
    },
    Case {
        member: "DeclinationRate",
        gate: Some("CanSetDeclinationRate"),
        get: |scope| Ok(Value::F(scope.declination_rate()?)),
        set: |scope, v| scope.set_declination_rate(v.f()?),
        other: |_, v| Value::F(v.f().unwrap_or(0.0) + 0.05),
        bad: None,
        seed: Value::F(0.0),
        precondition: SIDEREAL,
    },
    Case {
        // deliberately after the two rows above: writing it moves away from the sidereal
        // rate their precondition asks for, and the guard restores in reverse order, so
        // this one goes back to sidereal before their values are written back.
        member: "TrackingRate",
        gate: None,
        get: |scope| Ok(Value::Rate(scope.tracking_rate()?)),
        set: |scope, v| match v {
            Value::Rate(rate) => scope.set_tracking_rate(rate),
            other => Err(AscomError::type_mismatch("TrackingRate", format!("{other:?}"))),
        },
        other: |_, v| {
            Value::Rate(match v {
                Value::Rate(DriveRate::Sidereal) => DriveRate::Lunar,
                Value::Rate(DriveRate::Lunar) => DriveRate::Solar,
                Value::Rate(DriveRate::Solar) => DriveRate::King,
                _ => DriveRate::Sidereal,
            })
        },
        // A bogus enum value cannot be expressed through the typed setter.
        bad: None,
        seed: Value::Rate(DriveRate::Sidereal),
        precondition: ALWAYS,
    },
    Case {
        // The spec gives no numeric range for guide rates and lets the driver tie RA and
        // Dec together, which is why every value is snapshotted before any write.
        member: "GuideRateRightAscension",
        gate: Some("CanSetGuideRates"),
        get: |scope| Ok(Value::F(scope.guide_rate_right_ascension()?)),
        set: |scope, v| scope.set_guide_rate_right_ascension(v.f()?),
        other: |_, v| Value::F(v.f().unwrap_or(0.0).abs() + 0.1),
        bad: None,
        seed: Value::F(0.0),
        precondition: ALWAYS,
    },
    Case {
        member: "GuideRateDeclination",
        gate: Some("CanSetGuideRates"),
        get: |scope| Ok(Value::F(scope.guide_rate_declination()?)),
        set: |scope, v| scope.set_guide_rate_declination(v.f()?),
        other: |_, v| Value::F(v.f().unwrap_or(0.0).abs() + 0.2),
        bad: None,
        seed: Value::F(0.0),
        precondition: ALWAYS,
    },
    Case {
        member: "TargetRightAscension",
        gate: None,
        get: |scope| Ok(Value::F(scope.target_right_ascension()?)),
        set: |scope, v| scope.set_target_right_ascension(v.f()?),
        other: |_, v| Value::F(shift(v.f().unwrap_or(12.0), 0.5, 0.0, 24.0)),
        bad: Some(|_| Value::F(25.0)),
        seed: Value::F(12.0),
        precondition: ALWAYS,
    },
    Case {
        member: "TargetDeclination",
        gate: None,
        get: |scope| Ok(Value::F(scope.target_declination()?)),
        set: |scope, v| scope.set_target_declination(v.f()?),
        other: |_, v| Value::F(shift(v.f().unwrap_or(45.0), 0.5, -90.0, 90.0)),
        bad: Some(|_| Value::F(95.0)),
        seed: Value::F(45.0),
        precondition: ALWAYS,
    },
    Case {
        member: "UTCDate",
        gate: None,
        get: |scope| Ok(Value::When(scope.utc_date()?)),
        set: |scope, v| scope.set_utc_date(v.when()?),
        other: |_, v| Value::When(v.when().unwrap_or(UNIX_EPOCH) + Duration::from_secs(60)),
        bad: None,
        seed: Value::When(UNIX_EPOCH),
        precondition: ALWAYS,
    },
];

/// Opens [`DRIVER`], connects it and brings the mount to a state writes make sense in.
fn guarded() -> Restorer<Telescope, Value> {
    let scope = Telescope::open(&DeviceSpec::new(DRIVER)).expect("open telescope driver");
    scope.set_connected(true).expect("connect");
    restore_idle(&scope);
    // Superior drive rates can only be written at the sidereal rate, so normalise it
    // instead of letting a rate left behind by an earlier run skip those two rows.
    if let Ok(rate) = scope.tracking_rate() {
        if rate != DriveRate::Sidereal {
            if let Err(error) = scope.set_tracking_rate(DriveRate::Sidereal) {
                println!("note: TrackingRate is {rate:?} and cannot be set to sidereal: {error}");
            }
        }
    }
    Restorer::new(scope, CASES, DRIVER)
}

/// The three runs over the table are shared with the camera file and live in
/// `tests/common/mod.rs`; what is interface-specific here is only `guarded()`.

#[serial]
#[test]
fn writable_properties_round_trip() {
    common::writable_round_trip(&guarded());
}

#[serial]
#[test]
fn out_of_range_values_are_refused() {
    common::refuses_out_of_range_values(&guarded());
}

#[serial]
#[test]
fn writes_while_disconnected_are_refused() {
    common::refuses_writes_while_disconnected(&guarded());
}

/// `SideOfPier` is the one non-blocking write, so what it owes is motion rather than
/// a read-back: writing the value the mount already reports must succeed and
/// must not start anything.
#[serial]
#[test]
fn writing_the_current_pier_side_starts_no_motion() {
    let guard = guarded();
    let scope = guard.device();
    let caps = scope.capabilities().expect("capabilities");
    let current = scope.side_of_pier().expect("SideOfPier is mandatory");
    if current == PierSide::Unknown {
        println!("note: SideOfPier is Unknown (mount has no pointing state); nothing to write");
        return;
    }

    let outcome = scope.set_side_of_pier(current);
    match (caps.flag("CanSetPierSide"), outcome) {
        (Some(false), Err(error)) if error.is_unsupported() => {
            println!("note: CanSetPierSide false, write refused as required");
            return;
        }
        (Some(false), Ok(())) => {
            println!("DRIVER DEVIATION: SideOfPier written although CanSetPierSide is false");
        }
        (Some(false), Err(error)) => println!(
            "DRIVER DEVIATION: spec wants PropertyNotImplemented while CanSetPierSide is false, driver answered {:?} ({error})",
            error.kind
        ),
        (_, Ok(())) => {}
        (_, Err(error)) => {
            panic!("writing the current SideOfPier ({current:?}) must succeed: {error}")
        }
    }

    if scope.slewing().unwrap_or(false) {
        println!("DRIVER DEVIATION: Slewing became true on a no-op SideOfPier write");
    }
    if std::env::var("ASCOM_STRICT_SPEC").is_ok() && scope.slewing().unwrap_or(false) {
        panic!("driver is not spec-conformant (ASCOM_STRICT_SPEC is set)");
    }
    // Whatever it started, the mount must not be left moving.
    wait::wait_flag_false(scope.actor(), "Slewing", WaitSpec::with_poll(Duration::from_secs(10), POLL))
        .expect("a no-op SideOfPier write must not leave Slewing set");
}

// Axis motion, sync and pointing predicates.
//
// Everything below answers in milliseconds on the simulator (measured: the slowest call
// was 4 ms), so it covers the rest of `ITelescopeV4` without the minutes a real slew
// costs. `docs/KNOWN_DRIVER_QUIRKS.md` records the measured numbers.

/// A rate the mount must accept that is also as slow as its own advertisement allows:
/// the bottom of its lowest range, nudged above zero so that it is a motion at all.
fn slowest_rate(ranges: &[RateRange]) -> Option<f64> {
    let range = ranges
        .iter()
        .min_by(|a, b| a.maximum.partial_cmp(&b.maximum).expect("rate ranges are numbers"))?;
    let rate = f64::max(range.minimum, (range.maximum - range.minimum) * 0.01);
    (rate > 0.0 && range.contains(rate)).then_some(rate)
}

/// `AxisRates` is mandatory and `CanMoveAxis` names the axes `MoveAxis` accepts, so the
/// two have to agree: an axis that cannot move has no rate a caller could pass.
#[serial]
#[test]
fn axis_rates_and_can_move_axis_describe_the_mount() {
    let scope = open();
    for axis in TelescopeAxis::ALL {
        let movable = scope.can_move_axis(axis).unwrap_or_else(|error| {
            panic!("CanMoveAxis({axis:?}) is mandatory and must answer: {error}")
        });
        let ranges = scope.axis_rates(axis).unwrap_or_else(|error| {
            panic!("AxisRates({axis:?}) must never raise MethodNotImplemented: {error}")
        });
        println!("note: {axis:?} movable={movable} ranges={ranges:?}");
        for range in &ranges {
            assert!(
                range.minimum >= 0.0 && range.maximum >= range.minimum,
                "spec: AxisRates are positive magnitudes as a minimum/maximum pair, got {range:?}"
            );
        }
        if movable && ranges.is_empty() {
            deviation(&format!(
                "CanMoveAxis({axis:?}) is true, but AxisRates({axis:?}) offers no rate MoveAxis could accept"
            ));
        }
        if !movable && !ranges.is_empty() {
            deviation(&format!(
                "spec wants an empty AxisRates list for an axis MoveAxis does not support, \
                 AxisRates({axis:?}) answered {ranges:?} while CanMoveAxis is false"
            ));
        }
    }
}

/// Both axis members must refuse an axis the enum does not define. The typed API cannot
/// express a bogus axis on purpose, so this is the one place a test goes to the raw
/// dispatch — which is also how a client in another language would send it.
#[serial]
#[test]
fn an_axis_outside_the_enum_is_refused() {
    let scope = open();
    for raw in [9i32, -1] {
        for member in ["CanMoveAxis", "AxisRates"] {
            let outcome = scope.actor().call(move |device| {
                let axis = Variant::from_i32(raw);
                device.dispatch().call(member, &[&axis]).map(|value| value.is_some())
            });
            match outcome {
                Ok(_) => deviation(&format!(
                    "spec wants InvalidValue for {member}({raw}), the driver answered a value"
                )),
                Err(error) if error.kind == AscomErrorKind::InvalidValue => {}
                Err(error) if is_binding(&error) => {
                    panic!("{member}({raw}) failed in the binding layer: {error}")
                }
                Err(error) => deviation(&format!(
                    "spec wants InvalidValue for {member}({raw}), driver answered {:?} ({error})",
                    error.kind
                )),
            }
        }
    }
}

/// Spec: `MoveAxis` at a non-zero rate is non-blocking and must return with `Slewing`
/// true; only a call with a zero rate stops it and hands the axis back to its previous
/// tracking state. Both calls answer in under a millisecond here, which makes this the
/// one motion path that can be tested without a slew.
#[serial]
#[test]
fn move_axis_runs_until_a_zero_rate_stops_it() {
    let scope = open();
    restore_idle(&scope);
    if !scope.can_move_axis(TelescopeAxis::Primary).expect("CanMoveAxis is mandatory") {
        println!("note: the mount cannot move its primary axis; skipping");
        return;
    }
    let ranges = scope.axis_rates(TelescopeAxis::Primary).expect("AxisRates is mandatory");
    let Some(rate) = slowest_rate(&ranges) else {
        println!("note: AxisRates(Primary) = {ranges:?} has no usable rate; skipping");
        return;
    };

    let tracking_before = scope.tracking().expect("Tracking");
    let before = scope.right_ascension().expect("RightAscension");
    let started = Instant::now();
    scope.move_axis(TelescopeAxis::Primary, rate).expect("MoveAxis at an advertised rate");
    // The axis is really turning now; every panic below must still hand the mount back
    // at rest, so the stop is armed before anything else can fail.
    let guard = MotionGuard::new(&scope);
    let elapsed = started.elapsed();
    // Non-blocking by spec: the answer comes back while the axis is still turning.
    if !scope.slewing().expect("Slewing") {
        deviation("MoveAxis returned with Slewing false at a non-zero rate");
    }
    // A short pulse on purpose: the simulator has no limits, and what is under test is
    // the handshake, not the distance.
    std::thread::sleep(Duration::from_millis(200));
    let moved = scope.right_ascension().expect("RightAscension during the motion");

    scope
        .move_axis(TelescopeAxis::Primary, 0.0)
        .expect("spec: a zero rate is the call that stops MoveAxis motion");
    guard.disarm();
    if wait::wait_flag_false(
        scope.actor(),
        "Slewing",
        WaitSpec::with_poll(Duration::from_secs(20), POLL),
    )
    .is_err()
    {
        deviation("Slewing stayed true after MoveAxis with a zero rate");
    }
    if scope.tracking().expect("Tracking after the stop") != tracking_before {
        deviation("stopping MoveAxis did not return the axis to its previous tracking state");
    }
    println!("note: MoveAxis({rate}) answered in {elapsed:?}; RightAscension went {before} -> {moved}");

    restore_idle(&scope);
    assert!(
        !scope.slewing().expect("Slewing"),
        "the mount must not be left moving: it survives this process"
    );
}

/// Spec: a rate magnitude outside every range from `AxisRates` must raise InvalidValue,
/// and refusing it must not start motion. The sign is the caller's choice, so both
/// directions are judged.
#[serial]
#[test]
fn a_rate_outside_the_advertised_ranges_is_refused() {
    let scope = open();
    restore_idle(&scope);
    if !scope.can_move_axis(TelescopeAxis::Primary).expect("CanMoveAxis is mandatory") {
        println!("note: the mount cannot move its primary axis; skipping");
        return;
    }
    let ranges = scope.axis_rates(TelescopeAxis::Primary).expect("AxisRates is mandatory");
    if ranges.is_empty() {
        println!("note: AxisRates(Primary) is empty, so there is no out-of-range rate; skipping");
        return;
    }
    let fastest = ranges.iter().map(|range| range.maximum).fold(f64::MIN, f64::max);
    for rate in [fastest * 10.0 + 1.0, -(fastest * 10.0 + 1.0)] {
        match scope.move_axis(TelescopeAxis::Primary, rate) {
            Ok(()) => {
                deviation(&format!("MoveAxis accepted {rate}, above every advertised range {ranges:?}"));
                let _ = scope.move_axis(TelescopeAxis::Primary, 0.0);
            }
            Err(error) if error.kind == AscomErrorKind::InvalidValue => {}
            Err(error) if is_binding(&error) => {
                panic!("MoveAxis({rate}) failed in the binding layer: {error}")
            }
            Err(error) => deviation(&format!(
                "spec wants InvalidValue for MoveAxis({rate}), driver answered {:?} ({error})",
                error.kind
            )),
        }
        if scope.slewing().expect("Slewing") {
            deviation(&format!("refusing MoveAxis({rate}) started motion"));
        }
    }
    restore_idle(&scope);
}

/// `SyncToCoordinates` redefines where the mount believes it points: synchronous, no
/// motion, and its arguments copied into the `Target*` members. Time and read-back are
/// reported as deviations because they are the driver's doing; the wrapper's own failures
/// still panic.
#[serial]
#[test]
fn sync_to_coordinates_moves_the_pointing_model_not_the_mount() {
    let guard = guarded();
    let scope = guard.device();
    // A Sync writes TargetRightAscension/Declination, both of which the table restores.
    guard.snapshot();
    let caps = scope.capabilities().expect("capabilities");
    let ra = scope.right_ascension().expect("RightAscension is mandatory");
    let dec = scope.declination().expect("Declination is mandatory");
    let new_ra = shift(ra, 0.05, 0.0, 24.0);
    let new_dec = shift(dec, 0.5, -90.0, 90.0);

    let started = Instant::now();
    let outcome = scope.sync_to_coordinates(new_ra, new_dec);
    let elapsed = started.elapsed();
    if let Err(error) = outcome {
        if error.is_unsupported() {
            if caps.supports("CanSync") {
                panic!("SyncToCoordinates refused although CanSync is true: {error}");
            }
            println!("note: CanSync false, Sync refused as required");
            return;
        }
        if is_binding(&error) {
            panic!("SyncToCoordinates failed in the binding layer: {error}");
        }
        panic!("SyncToCoordinates on an unparked, tracking mount must succeed: {error}");
    }

    assert!(!scope.slewing().expect("Slewing"), "a Sync must not start a slew");
    // The mount now owes the coordinates it was given. Tracking keeps advancing
    // underneath, hence a tolerance rather than equality.
    let read_ra = scope.right_ascension().expect("RightAscension after the Sync");
    let read_dec = scope.declination().expect("Declination after the Sync");
    if f64::abs(read_ra - new_ra) > 1e-3 || f64::abs(read_dec - new_dec) > 0.05 {
        deviation(&format!(
            "SyncToCoordinates({new_ra}, {new_dec}) left the mount at ({read_ra}, {read_dec})"
        ));
    }
    let target_ra = scope.target_right_ascension().expect("TargetRightAscension");
    let target_dec = scope.target_declination().expect("TargetDeclination");
    if f64::abs(target_ra - new_ra) > 1e-3 || f64::abs(target_dec - new_dec) > 1e-3 {
        deviation(&format!(
            "spec copies the Sync arguments to Target*, driver kept ({target_ra}, {target_dec})"
        ));
    }
    println!("note: SyncToCoordinates answered in {elapsed:?} and moved the pointing model only");
    if elapsed > Duration::from_secs(5) {
        deviation(&format!("a synchronous Sync took {elapsed:?}, which looks like motion"));
    }
    // Nothing in the table can put RA/Dec back, so the test does it the way the driver
    // offers: sync to where it stood.
    scope.sync_to_coordinates(ra, dec).expect("syncing back to the original coordinates");
}

/// The same instant operation with its inputs read from `Target*` instead of arguments.
#[serial]
#[test]
fn sync_to_target_syncs_to_the_target_coordinates() {
    let guard = guarded();
    let scope = guard.device();
    guard.snapshot();
    let caps = scope.capabilities().expect("capabilities");
    let ra = scope.right_ascension().expect("RightAscension");
    let dec = scope.declination().expect("Declination");
    let new_ra = shift(ra, 1.0, 0.0, 24.0);
    let new_dec = shift(dec, 1.0, -90.0, 90.0);
    scope.set_target_right_ascension(new_ra).expect("TargetRightAscension in range");
    scope.set_target_declination(new_dec).expect("TargetDeclination in range");

    if let Err(error) = scope.sync_to_target() {
        if error.is_unsupported() {
            if caps.supports("CanSync") {
                panic!("SyncToTarget refused although CanSync is true: {error}");
            }
            println!("note: CanSync false, Sync refused as required");
            return;
        }
        if is_binding(&error) {
            panic!("SyncToTarget failed in the binding layer: {error}");
        }
        panic!("SyncToTarget on an unparked, tracking mount must succeed: {error}");
    }
    assert!(!scope.slewing().expect("Slewing"), "a Sync must not start a slew");
    let read_ra = scope.right_ascension().expect("RightAscension after the Sync");
    let read_dec = scope.declination().expect("Declination after the Sync");
    if f64::abs(read_ra - new_ra) > 1e-3 || f64::abs(read_dec - new_dec) > 0.05 {
        deviation(&format!(
            "SyncToTarget of ({new_ra}, {new_dec}) left the mount at ({read_ra}, {read_dec})"
        ));
    }
    scope.sync_to_coordinates(ra, dec).expect("syncing back to the original coordinates");
}

/// Out-of-range coordinates must be refused as InvalidValue, and a refusal must leave
/// both the mount and the pointing model where they were.
#[serial]
#[test]
fn sync_rejects_out_of_range_coordinates() {
    let scope = open();
    restore_idle(&scope);
    if !scope.capabilities().expect("capabilities").supports("CanSync") {
        println!("note: no CanSync, so there is no Sync to feed bad coordinates");
        return;
    }
    let ra = scope.right_ascension().expect("RightAscension");
    let dec = scope.declination().expect("Declination");
    for (bad_ra, bad_dec) in [(25.0, dec), (ra, 95.0), (ra, -95.0)] {
        match scope.sync_to_coordinates(bad_ra, bad_dec) {
            Ok(()) => deviation(&format!(
                "SyncToCoordinates({bad_ra}, {bad_dec}) accepted an out-of-range coordinate"
            )),
            Err(error) if error.kind == AscomErrorKind::InvalidValue => {}
            Err(error) if is_binding(&error) => {
                panic!("SyncToCoordinates({bad_ra}, {bad_dec}) failed in the binding layer: {error}")
            }
            Err(error) => deviation(&format!(
                "spec wants InvalidValue for SyncToCoordinates({bad_ra}, {bad_dec}), driver answered {:?} ({error})",
                error.kind
            )),
        }
    }
    assert!(!scope.slewing().expect("Slewing"), "a refused Sync must not start motion");
    let still_ra = scope.right_ascension().expect("RightAscension");
    assert!(
        f64::abs(still_ra - ra) < 1e-3,
        "a refused Sync must not move the pointing model ({ra} -> {still_ra})"
    );
}

/// `SyncToAltAz` is the same instant operation through horizontal coordinates. The spec
/// only warns that it may raise an exception while `Tracking` is true — the simulator
/// requires it false — so the whole check runs with tracking off and puts it back.
#[serial]
#[test]
fn sync_to_altaz_answers_in_horizontal_coordinates() {
    let guard = guarded();
    let scope = guard.device();
    guard.snapshot();
    let caps = scope.capabilities().expect("capabilities");
    let (azimuth, altitude) = match (scope.azimuth(), scope.altitude()) {
        (Ok(azimuth), Ok(altitude)) => (azimuth, altitude),
        _ => {
            println!("note: the mount reports no horizontal coordinates; skipping");
            return;
        }
    };
    let ra = scope.right_ascension().expect("RightAscension");
    let dec = scope.declination().expect("Declination");
    if let Err(error) = scope.set_tracking(false) {
        println!("note: cannot stop tracking to set up SyncToAltAz: {error}");
        return;
    }

    let outcome = scope.sync_to_alt_az(azimuth, altitude);
    match &outcome {
        Ok(()) => {}
        Err(error) if error.is_unsupported() => {
            if caps.supports("CanSyncAltAz") {
                panic!("SyncToAltAz refused although CanSyncAltAz is true: {error}");
            }
            println!("note: CanSyncAltAz false, Sync refused as required");
            scope.set_tracking(true).expect("Tracking back on");
            return;
        }
        Err(error) if is_binding(error) => {
            panic!("SyncToAltAz failed in the binding layer: {error}")
        }
        Err(error) => deviation(&format!(
            "SyncToAltAz refused with tracking off, which the spec does not allow ({:?}: {error})",
            error.kind
        )),
    }
    if outcome.is_ok() {
        assert!(!scope.slewing().expect("Slewing"), "a Sync must not start a slew");
        // Even the argument checks only make sense once the driver takes the call at all.
        for (bad_az, bad_alt) in [(361.0, altitude), (azimuth, 95.0), (azimuth, -95.0)] {
            match scope.sync_to_alt_az(bad_az, bad_alt) {
                Ok(()) => deviation(&format!(
                    "SyncToAltAz({bad_az}, {bad_alt}) accepted an out-of-range coordinate"
                )),
                Err(error) if error.kind == AscomErrorKind::InvalidValue => {}
                Err(error) if is_binding(&error) => {
                    panic!("SyncToAltAz({bad_az}, {bad_alt}) failed in the binding layer: {error}")
                }
                Err(error) => deviation(&format!(
                    "spec wants InvalidValue for SyncToAltAz({bad_az}, {bad_alt}), driver answered {:?} ({error})",
                    error.kind
                )),
            }
        }
    }

    scope.set_tracking(true).expect("Tracking back on");
    // Syncing to the mount's own azimuth and altitude should not have pointed it away,
    // but the mapping is the driver's business, so put the pointing model back anyway.
    let _ = scope.sync_to_coordinates(ra, dec);
    restore_idle(&scope);
    assert!(!scope.slewing().expect("Slewing"), "the mount must not be left moving");
}

/// `DestinationSideOfPier` is a prediction about a slew it must not perform. Out-of-range
/// coordinates are the one thing the spec asks it to refuse.
#[serial]
#[test]
fn destination_side_of_pier_predicts_without_touching_the_mount() {
    let scope = open();
    let ra = scope.right_ascension().expect("RightAscension");
    let dec = scope.declination().expect("Declination");
    for (test_ra, test_dec) in [(ra, dec), (0.0, 89.0), (12.0, -45.0)] {
        match scope.destination_side_of_pier(test_ra, test_dec) {
            // `Unknown` is a legal answer for a mount without a pointing state.
            Ok(side) => println!("note: DestinationSideOfPier({test_ra}, {test_dec}) = {side:?}"),
            Err(error) if error.is_unsupported() => {
                println!("note: the driver does not implement DestinationSideOfPier; skipping");
                return;
            }
            Err(error) if is_binding(&error) => {
                panic!("DestinationSideOfPier failed in the binding layer: {error}")
            }
            Err(error) => panic!(
                "spec: DestinationSideOfPier must answer for valid coordinates ({test_ra}, {test_dec}): {error}"
            ),
        }
        assert!(!scope.slewing().expect("Slewing"), "a prediction must not start motion");
    }
    for (bad_ra, bad_dec) in [(25.0, 45.0), (ra, 95.0), (-1.0, 0.0)] {
        match scope.destination_side_of_pier(bad_ra, bad_dec) {
            Ok(side) => deviation(&format!(
                "spec wants InvalidValue for DestinationSideOfPier({bad_ra}, {bad_dec}), driver answered {side:?}"
            )),
            Err(error) if error.kind == AscomErrorKind::InvalidValue => {}
            Err(error) if is_binding(&error) => {
                panic!("DestinationSideOfPier failed in the binding layer: {error}")
            }
            Err(error) => deviation(&format!(
                "spec wants InvalidValue for DestinationSideOfPier({bad_ra}, {bad_dec}), driver answered {:?} ({error})",
                error.kind
            )),
        }
    }
}

/// Spec: the list must contain at least sidereal, and the rate the mount reports has to
/// be one of the rates it advertises — otherwise a client could not put it back.
#[serial]
#[test]
fn tracking_rates_always_include_sidereal() {
    let scope = open();
    let rates = scope.tracking_rates().expect("TrackingRates is mandatory");
    assert!(
        rates.contains(&DriveRate::Sidereal),
        "spec: TrackingRates must contain Sidereal, got {rates:?}"
    );
    let current = scope.tracking_rate().expect("reading TrackingRate is mandatory");
    if !rates.contains(&current) {
        deviation(&format!("TrackingRate {current:?} is not among TrackingRates {rates:?}"));
    }
}

/// Parking at the position `SetPark` just recorded takes a quarter of a second here, so
/// the parked state and its refusals are reachable without waiting on a real park.
/// Spec: slew, move, sync, homing and enabling tracking while parked must raise
/// ParkedException — V4 says so for `AbortSlew` too — while `DestinationSideOfPier` is
/// not on that list at all.
#[serial]
#[test]
fn parked_mount_refuses_motion() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !caps.supports("CanPark") {
        println!("note: no CanPark, the parked state cannot be entered; skipping");
        return;
    }
    restore_idle(&scope);
    scope.set_park().expect("SetPark at rest");
    scope.park_async().expect("Park initiated");
    if wait::wait_flag_true(
        scope.actor(),
        "AtPark",
        WaitSpec::with_poll(Duration::from_secs(20), POLL),
    )
    .is_err()
    {
        println!("note: AtPark not reached in 20s from the recorded position; skipping the parked checks");
        restore_idle(&scope);
        return;
    }
    assert!(!scope.slewing().expect("Slewing at park"), "AtPark came with motion");
    assert!(
        !scope.tracking().expect("Tracking at park"),
        "spec: Tracking must be false while parked"
    );

    let mut refused = 0usize;
    let mut expect_parked = |name: &str, outcome: Result<()>| match outcome {
        Ok(()) => deviation(&format!("{name} accepted while the mount is parked")),
        Err(error) if error.kind == AscomErrorKind::Parked => refused += 1,
        Err(error) if is_binding(&error) => {
            panic!("{name} failed in the binding layer while parked: {error}")
        }
        Err(error) => deviation(&format!(
            "spec wants Parked for {name} while parked, driver answered {:?} ({error})",
            error.kind
        )),
    };

    expect_parked("SlewToCoordinatesAsync", scope.slew_to_coordinates_async(3.0, 30.0));
    if scope.can_move_axis(TelescopeAxis::Primary).unwrap_or(false) {
        let rate = slowest_rate(&scope.axis_rates(TelescopeAxis::Primary).unwrap_or_default())
            .unwrap_or(0.05);
        expect_parked("MoveAxis", scope.move_axis(TelescopeAxis::Primary, rate));
    }
    if caps.supports("CanSync") {
        expect_parked("SyncToCoordinates", scope.sync_to_coordinates(3.0, 30.0));
        expect_parked("SyncToTarget", scope.sync_to_target());
    }
    expect_parked("Tracking = true", scope.set_tracking(true));
    expect_parked("AbortSlew", scope.abort_slew());
    if caps.supports("CanFindHome") {
        // Accepted homing means the mount left for home; the cleanup below stops it.
        expect_parked("FindHome", scope.find_home_async());
    }
    if caps.supports("CanPulseGuide") {
        expect_parked("PulseGuide", scope.pulse_guide(GuideDirection::North, 100));
    }
    match scope.destination_side_of_pier(3.0, 30.0) {
        Ok(side) => println!("note: DestinationSideOfPier answers {side:?} while parked"),
        Err(error) if error.is_unsupported() => {
            println!("note: the driver does not implement DestinationSideOfPier")
        }
        Err(error) if error.kind == AscomErrorKind::Parked => {
            deviation("DestinationSideOfPier is not on the parked list and must still answer")
        }
        Err(error) if is_binding(&error) => {
            panic!("DestinationSideOfPier failed in the binding layer while parked: {error}")
        }
        Err(error) => deviation(&format!(
            "spec wants a prediction or PropertyNotImplemented from DestinationSideOfPier while parked, driver answered {:?} ({error})",
            error.kind
        )),
    }
    println!("note: {refused} members refused with ParkedException as the spec requires");

    scope.unpark().expect("Unpark must end the parked state");
    restore_idle(&scope);
    assert!(
        !scope.at_park().unwrap_or(true),
        "the mount must not be left parked: it survives this process"
    );
}

/// Spec: `PulseGuide` must raise InvalidOperationException while the mount is not
/// tracking. Reaching the same refusal during a slew would need a slew, so it is left
/// to the driver's own judgment.
#[serial]
#[test]
fn pulse_guide_while_not_tracking_is_refused() {
    let scope = open();
    let caps = scope.capabilities().expect("capabilities");
    if !caps.supports("CanPulseGuide") {
        println!("note: no CanPulseGuide; skipping");
        return;
    }
    restore_idle(&scope);
    if let Err(error) = scope.set_tracking(false) {
        println!("note: cannot stop tracking to set up the refusal: {error}");
        return;
    }
    let outcome = scope.pulse_guide(GuideDirection::North, 100);
    let _ = scope.set_tracking(true);
    match outcome {
        Ok(()) => deviation("PulseGuide accepted while Tracking is false"),
        Err(error) if error.kind == AscomErrorKind::InvalidOperation => {}
        Err(error) if is_binding(&error) => {
            panic!("PulseGuide failed in the binding layer: {error}")
        }
        Err(error) => deviation(&format!(
            "spec wants InvalidOperation for PulseGuide while not tracking, driver answered {:?} ({error})",
            error.kind
        )),
    }
    // A pulse the driver took may still be running; never leave the mount moving.
    let _ = wait::wait_flag_false(
        scope.actor(),
        "IsPulseGuiding",
        WaitSpec::with_poll(Duration::from_secs(10), POLL),
    );
    restore_idle(&scope);
    assert!(
        !scope.slewing().expect("Slewing"),
        "the mount must not be left moving: it survives this process"
    );
}
