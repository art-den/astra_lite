//! Tests for `ICameraV4` against a live driver.
//!
//! Run: `ASCOM_CAMERA_PROG_ID=ASCOM.OmniSim.Camera cargo test --test camera`
//!
//! The read-only checklist honours that variable; every test that writes state — the
//! table runs at the bottom and the frame-writing exposure test above them — always runs
//! against the one driver named by `DRIVER`, because this is the only camera on the
//! machine that can be written to at all.

mod common;
// The driver is one shared Singleton that survives the process: `#[serial]` keeps
// two tests off it at once, which is what `--test-threads=1` used to guarantee.
use serial_test::serial;

use ascom::camera::{Camera, CameraState};
use ascom::com::Variant;
use ascom::device::{AscomDevice, DeviceSpec};
use ascom::error::{AscomError, AscomErrorKind, Result};
use ascom::image::bits_for_adu_max;
use ascom::wait;
use ascom::wait::WaitSpec;
use std::cell::Cell;
use std::time::{Duration, Instant};

use common::{
    check_connected_idempotent, check_identity_members, check_operational_needs_connection,
    check_unknown_action, is_binding, prog_id, report, walk_members, APPLY_TIMEOUT, Case,
    POLL, Restorer,
};

/// Read-only members of ICameraV4, from the spec's property list. `ImageArray` and
/// `ImageArrayVariant` are deliberately absent: the former may only be read after
/// `ImageReady`, the latter is never used (playbook §9.2).
const MEMBERS: &[&str] = &[
    "BayerOffsetX",
    "BayerOffsetY",
    "BinX",
    "BinY",
    "CCDTemperature",
    "CameraState",
    "CameraXSize",
    "CameraYSize",
    "CanAbortExposure",
    "CanAsymmetricBin",
    "CanFastReadout",
    "CanGetCoolerPower",
    "CanPulseGuide",
    "CanSetCCDTemperature",
    "CanStopExposure",
    "Connected",
    "Connecting",
    "CoolerOn",
    "CoolerPower",
    "Description",
    "DeviceState",
    "DriverInfo",
    "DriverVersion",
    "ElectronsPerADU",
    "ExposureMax",
    "ExposureMin",
    "ExposureResolution",
    "FastReadout",
    "FullWellCapacity",
    "Gain",
    "GainMax",
    "GainMin",
    "Gains",
    "HasShutter",
    "HeatSinkTemperature",
    "ImageReady",
    "InterfaceVersion",
    "IsPulseGuiding",
    "LastExposureDuration",
    "LastExposureStartTime",
    "MaxADU",
    "MaxBinX",
    "MaxBinY",
    "Name",
    "NumX",
    "NumY",
    "Offset",
    "OffsetMax",
    "OffsetMin",
    "Offsets",
    "PercentCompleted",
    "PixelSizeX",
    "PixelSizeY",
    "ReadoutMode",
    "ReadoutModes",
    "SensorName",
    "SensorType",
    // V4-only members. The driver reports InterfaceVersion 3, so it does not expose
    // all of them; `GetIDsOfNames` then fails, which must read as "not implemented"
    // rather than as a binding-layer failure (docs/KNOWN_DRIVER_QUIRKS.md).
    "Actions",
    "Member",
    "NormalReadout",
    "PostActions",
    "PreActions",
    "ShowDialog",
    "SupportsSafeMode",
    "UTCDate",
    "StartX",
    "StartY",
    "SupportedActions",
];

fn open() -> Camera {
    let id = prog_id("ASCOM_CAMERA_PROG_ID", "ASCOM.OmniSim.Camera");
    let camera = Camera::open(&DeviceSpec::new(id)).expect("open camera driver");
    camera.set_connected(true).expect("connect");
    camera
}

#[serial]
#[test]
fn every_member_answers_or_is_unsupported() {
    let camera = open();
    walk_members(&camera, MEMBERS);
}

#[serial]
#[test]
fn unknown_action_is_action_not_implemented() {
    let camera = open();
    check_unknown_action(&camera);
}

#[serial]
#[test]
fn identity_members_are_readable() {
    let camera = open();
    check_identity_members(&camera).expect("identity members");
}

#[serial]
#[test]
fn connected_is_idempotent() {
    let camera = open();
    check_connected_idempotent(&camera).expect("connected cycle");
}

#[serial]
#[test]
fn operational_property_needs_connection() {
    let camera = open();
    check_operational_needs_connection(&camera, "CCDTemperature").expect("not-connected probe");
}

/// Aborts an exposure the test did not see through, even when the test panics.
///
/// An exposure that outlives its test blocks every later one: the simulator is one
/// shared singleton and refuses a new `StartExposure` while it is busy. The guard is
/// therefore armed as soon as an exposure is in flight and stands down once the frame has
/// been read. Like the other guards, `Drop` may panic only when the test itself passed: a
/// second panic during an unwind aborts the process and hides the real failure, so during
/// an unwind this only prints `GUARD FAILED`.
struct ExposureGuard<'a> {
    camera: &'a Camera,
    armed: Cell<bool>,
}

impl<'a> ExposureGuard<'a> {
    fn new(camera: &'a Camera) -> Self {
        Self { camera, armed: Cell::new(true) }
    }

    /// The test read its frame; there is nothing left to abort.
    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for ExposureGuard<'_> {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        let mut problems = Vec::new();
        // A camera already back at Idle answers InvalidOperationException here, so it is
        // the wait below, not this answer, that judges the guard.
        if let Err(error) = self.camera.abort_exposure() {
            println!("note: guard: AbortExposure answered {:?} ({error})", error.kind);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = match self.camera.camera_state() {
                Ok(state) => state,
                Err(error) => {
                    problems.push(format!("CameraState after the guard's abort: {error}"));
                    break;
                }
            };
            if state == CameraState::Idle {
                break;
            }
            if Instant::now() >= deadline {
                problems.push(format!("CameraState is {state:?} after the guard's abort"));
                break;
            }
            std::thread::sleep(POLL);
        }
        if problems.is_empty() {
            println!("note: guard: the exposure left behind was aborted, the camera is idle");
            return;
        }
        let text = problems.join("\n");
        if std::thread::panicking() {
            eprintln!("GUARD FAILED (a panic is already unwinding): {text}");
        } else {
            panic!("guard could not return the camera to idle:\n{text}");
        }
    }
}

/// The async exposure cycle on a deliberately **non-square** sub-frame: the classic
/// transposition trap is invisible on a square frame, so we never use one here.
///
/// Writing the frame makes this a write test, so it runs against `DRIVER` under the same
/// guard as the table runs below: the frame the driver reports at entry goes back on the
/// way out, and an exposure the test does not see through is aborted by [`ExposureGuard`].
#[serial]
#[test]
fn non_square_exposure_completes_via_imageready() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();

    let sensor_x = camera.camera_x_size().expect("CameraXSize");
    let sensor_y = camera.camera_y_size().expect("CameraYSize");
    let num_x = sensor_x.min(96);
    let num_y = sensor_y.min(48).max(1);
    let (num_x, num_y) = if num_x == num_y { (num_x, (num_y / 2).max(1)) } else { (num_x, num_y) };
    camera.set_sub_frame(0, 0, num_x, num_y).expect("set non-square sub-frame");

    let duration = camera.exposure_min().unwrap_or(0.1).max(0.1);
    camera.start_exposure(duration, true).expect("StartExposure initiated");
    // An exposure is in flight from here on; every panic below must still hand the camera
    // back to the tests that run after this one.
    let abort = ExposureGuard::new(camera);

    // Completion property is ImageReady; it must be reached, not timed out.
    match wait::wait_flag_true(camera.actor(), "ImageReady", WaitSpec::new(Duration::from_secs(120)))
    {
        Err(e) if e.kind == AscomErrorKind::Timeout => {
            panic!("ImageReady never became true: {e}");
        }
        Err(e) => panic!("exposure completion wait failed: {e}"),
        Ok(()) => {}
    }

    let image = camera.read_image().expect("read ImageArray after ImageReady");
    abort.disarm();
    assert_eq!(image.width(), num_x as usize, "NumX must be the width");
    assert_eq!(image.height(), num_y as usize, "NumY must be the height");
    assert_ne!(image.width(), image.height(), "test frame must be non-square");
    println!(
        "detected orientation {:?} (ambiguous={}) for a {num_x}x{num_y} frame",
        image.layout.orientation, image.layout.ambiguous
    );
    assert!(!image.layout.ambiguous, "a non-square frame must prove its orientation");

    // FITS lists the fastest varying axis first, so NAXIS1 must be whatever axis the
    // driver really made fastest — which is the whole point of not transposing.
    let path = std::env::temp_dir().join("ascom_camera.fits");
    image.write_fits(&path).expect("write FITS");
    let bytes = std::fs::read(&path).expect("read back FITS");
    let header = String::from_utf8_lossy(&bytes[..2880]).into_owned();
    let axes = image.layout.axes_fastest_first();
    for (i, axis) in axes.iter().enumerate() {
        let declared = fits_value(&header, &format!("NAXIS{}", i + 1));
        assert_eq!(
            declared.parse::<usize>(),
            Ok(image.layout.len_of(*axis)),
            "NAXIS{} must describe the {:?} axis (fastest-first order)",
            i + 1,
            axis
        );
    }

    // And the first sample on disk must be memory index 0, big-endian: proof that the
    // buffer was written verbatim rather than rearranged.
    let data = &bytes[2880..];
    let first = image.data.value(0).expect("non-empty frame");
    let on_disk = match image.data {
        ascom::image::Pixels::I16(_) => i16::from_be_bytes([data[0], data[1]]) as f64,
        ascom::image::Pixels::I32(_) => i32::from_be_bytes([data[0], data[1], data[2], data[3]]) as f64,
        _ => f64::NAN,
    };
    assert_eq!(on_disk, first, "disk must start at memory index 0");
    let _ = std::fs::remove_file(&path);
}

fn fits_value(header: &str, key: &str) -> String {
    for chunk in header.as_bytes().chunks(80) {
        let card = String::from_utf8_lossy(chunk).into_owned();
        if card.starts_with(key) && card.len() >= 30 && &card[8..10] == "= " {
            return card[10..30].trim().to_string();
        }
    }
    panic!("{key} not found in FITS header");
}

// ------------------------------------------------------------------- writes ---
//
// The focuser scenario applied to the writable surface of ICameraV4: read → write a
// different value → wait until the change is visible → read back. Only two members have
// a wait that is more than a grace period: the frame geometry, whose change shows up in
// the next `ImageArray`, and the cooler, whose effect is a falling `CCDTemperature`.
//
// One driver, hard-coded on purpose: no other camera on this machine can be written to.
// It reports `InterfaceVersion` 3, so the V4-only members are absent, and `Gain`,
// `Offset`, `FastReadout` and `SetCCDTemperature` are not implemented at all — which is
// why the table is walked twice: once over the members that answer, once over the ones
// that must refuse reads and writes alike.
//
// Deliberately not tested: refusing writes while an exposure runs. The exception lists
// of BinX, BinY, StartX, StartY, NumX, NumY, Gain, Offset and FastReadout contain no
// InvalidOperationException, so the spec requires nothing there; the driver checks an
// incompatible frame at `StartExposure` instead (see `a_frame_that_does_not_fit`),
// which is what docs/KNOWN_DRIVER_QUIRKS.md records.

/// The driver the write tests run against.
const DRIVER: &str = "ASCOM.OmniSim.Camera";

/// Bound for the deliberate exposures below.
const EXPOSURE_TIMEOUT: Duration = Duration::from_secs(60);

/// Cooler setpoints are floats the driver is free to round.
const EPS: f64 = 1e-6;

/// A writable member's value — only as many types as the table below needs.
#[derive(Clone, Copy)]
enum Value {
    F(f64),
    I(i32),
    B(bool),
    /// `StartX`, `StartY`, `NumX`, `NumY`: writable only as one unit, because the
    /// wrapper refuses a frame that runs off the sensor and the driver checks the
    /// combination later, at `StartExposure`.
    Frame(i32, i32, i32, i32),
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

    fn frame(self) -> Result<(i32, i32, i32, i32)> {
        match self {
            Self::Frame(start_x, start_y, num_x, num_y) => Ok((start_x, start_y, num_x, num_y)),
            other => Err(AscomError::type_mismatch("SubFrame", format!("{other:?} is not a frame"))),
        }
    }

    fn same(self, other: Value) -> bool {
        match (self, other) {
            (Self::F(a), Self::F(b)) => f64::abs(a - b) <= f64::max(EPS, f64::abs(b) * EPS),
            (Self::I(a), Self::I(b)) => a == b,
            (Self::B(a), Self::B(b)) => a == b,
            (Self::Frame(x0, y0, w0, h0), Self::Frame(x1, y1, w1, h1)) => {
                x0 == x1 && y0 == y1 && w0 == w1 && h0 == h1
            }
            _ => false,
        }
    }
}

/// Equality for the assertions below is the same comparison the table tests use.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.same(*other)
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::F(v) => write!(f, "F({v})"),
            Self::I(v) => write!(f, "I({v})"),
            Self::B(v) => write!(f, "B({v})"),
            Self::Frame(start_x, start_y, num_x, num_y) => {
                write!(f, "Frame({num_x}x{num_y}+{start_x}+{start_y})")
            }
        }
    }
}

/// Largest legal frame extent for the binning the camera reports right now.
///
/// The spec expresses `StartX`/`NumX` in **binned** pixels, so the limit is
/// `CameraXSize / BinX`. A member that does not answer (a disconnected driver, for
/// instance) degrades to 1 rather than failing: `other` below only needs a bound.
fn limits(camera: &Camera) -> (i32, i32) {
    let x = camera.camera_x_size().unwrap_or(1).max(1) / camera.bin_x().unwrap_or(1).max(1);
    let y = camera.camera_y_size().unwrap_or(1).max(1) / camera.bin_y().unwrap_or(1).max(1);
    (x.max(1), y.max(1))
}

/// A frame that differs from `current` and still fits the sensor at the current binning.
fn other_frame(camera: &Camera, current: Value) -> Value {
    let (max_x, max_y) = limits(camera);
    for (start_x, start_y, num_x, num_y) in [
        (1, 1, max_x - 1, max_y - 1),
        (2, 2, max_x - 2, max_y - 2),
        (0, 0, 1, 1),
    ] {
        let candidate = Value::Frame(start_x, start_y, num_x, num_y);
        if num_x >= 1 && num_y >= 1 && !candidate.same(current) {
            return candidate;
        }
    }
    Value::Frame(0, 0, 1, 1)
}

/// The row shape (`Case`) and the guard that restores what the rows write (`Restorer`)
/// live in `tests/common/mod.rs`, shared with the telescope file.

const ALWAYS: fn(&Camera) -> Result<bool> = |_| Ok(true);

/// The table row of one member.
fn case(member: &str) -> &'static Case<Camera, Value> {
    CASES.iter().find(|candidate| candidate.member == member)
        .unwrap_or_else(|| panic!("{member} is not in CASES"))
}

const CASES: &[Case<Camera, Value>] = &[
    Case {
        // First in the table on purpose: the guard restores in reverse order, so the
        // frame goes back *after* the binning rows, and a full-size frame is only legal
        // at bin 1x1, where StartX/NumX coincide with unbinned pixels.
        member: "SubFrame",
        gate: None,
        get: |camera| {
            Ok(Value::Frame(
                camera.start_x()?,
                camera.start_y()?,
                camera.num_x()?,
                camera.num_y()?,
            ))
        },
        set: |camera, value| match value {
            Value::Frame(start_x, start_y, num_x, num_y) => {
                camera.set_sub_frame(start_x, start_y, num_x, num_y)
            }
            other => Err(AscomError::type_mismatch("SubFrame", format!("{other:?}"))),
        },
        other: other_frame,
        // Runs past the sensor. The spec defers this check to StartExposure, so the
        // refusal below comes from this wrapper's guard, which is stricter.
        bad: Some(|camera| {
            Value::Frame(
                0,
                0,
                camera.camera_x_size().unwrap_or(8) + 10,
                camera.camera_y_size().unwrap_or(6),
            )
        }),
        seed: Value::Frame(0, 0, 1, 1),
        precondition: ALWAYS,
    },
    Case {
        member: "BinX",
        gate: None,
        get: |camera| Ok(Value::I(camera.bin_x()?)),
        // BinX and BinY are only writable as a pair; the sibling keeps its value, which
        // is what `CanAsymmetricBin` decides whether the driver tolerates.
        set: |camera, value| camera.set_binning(value.i()?, camera.bin_y()?),
        other: |camera, value| {
            let max = camera.max_bin_x().unwrap_or(1).max(1);
            let current = value.i().unwrap_or(1);
            Value::I(if current < max { current + 1 } else { (current - 1).max(1) })
        },
        bad: Some(|camera| Value::I(camera.max_bin_x().unwrap_or(1) + 1)),
        seed: Value::I(1),
        precondition: ALWAYS,
    },
    Case {
        member: "BinY",
        gate: None,
        get: |camera| Ok(Value::I(camera.bin_y()?)),
        set: |camera, value| camera.set_binning(camera.bin_x()?, value.i()?),
        other: |camera, value| {
            let max = camera.max_bin_y().unwrap_or(1).max(1);
            let current = value.i().unwrap_or(1);
            Value::I(if current < max { current + 1 } else { (current - 1).max(1) })
        },
        bad: Some(|camera| Value::I(camera.max_bin_y().unwrap_or(1) + 1)),
        seed: Value::I(1),
        precondition: ALWAYS,
    },
    Case {
        member: "CoolerOn",
        gate: None,
        get: |camera| Ok(Value::B(camera.cooler_on()?)),
        set: |camera, value| camera.set_cooler_on(value.b()?),
        other: |_, value| Value::B(!value.b().unwrap_or(false)),
        bad: None,
        seed: Value::B(false),
        precondition: ALWAYS,
    },
    Case {
        member: "SetCCDTemperature",
        gate: Some("CanSetCCDTemperature"),
        get: |camera| Ok(Value::F(camera.target_ccd_temperature()?)),
        set: |camera, value| camera.set_target_ccd_temperature(value.f()?),
        other: |_, value| Value::F(value.f().unwrap_or(-15.0) - 1.0),
        // The spec has Conform reject setpoints above +100C or below -280C.
        bad: Some(|_| Value::F(1_000.0)),
        seed: Value::F(-15.0),
        precondition: ALWAYS,
    },
    Case {
        member: "FastReadout",
        gate: Some("CanFastReadout"),
        get: |camera| Ok(Value::B(camera.fast_readout()?)),
        set: |camera, value| camera.set_fast_readout(value.b()?),
        other: |_, value| Value::B(!value.b().unwrap_or(false)),
        bad: None,
        seed: Value::B(false),
        precondition: ALWAYS,
    },
    Case {
        member: "Gain",
        gate: None,
        get: |camera| Ok(Value::I(camera.gain()?)),
        set: |camera, value| camera.set_gain(value.i()?),
        other: |_, value| Value::I(value.i().unwrap_or(0) + 1),
        // Its range is GainMin..GainMax; where those are unimplemented the number below
        // is only a probe of how the driver reacts to nonsense.
        bad: Some(|camera| Value::I(camera.gain_max().unwrap_or(1_000_000) + 1)),
        seed: Value::I(1),
        precondition: ALWAYS,
    },
    Case {
        member: "Offset",
        gate: None,
        get: |camera| Ok(Value::I(camera.offset()?)),
        set: |camera, value| camera.set_offset(value.i()?),
        other: |_, value| Value::I(value.i().unwrap_or(0) + 1),
        bad: Some(|camera| Value::I(camera.offset_max().unwrap_or(1_000_000) + 1)),
        seed: Value::I(1),
        precondition: ALWAYS,
    },
];

/// Brings the camera back to `Idle`, so that no write lands mid-exposure.
///
/// All these tests share one simulator process, and an exposure started by an earlier
/// run or a failed test survives it; `CameraState` is what says so.
fn restore_idle(camera: &Camera) {
    let busy = camera.camera_state().map(|state| state != CameraState::Idle).unwrap_or(false);
    if !busy {
        return;
    }
    if let Err(error) = camera.abort_exposure() {
        println!("note: AbortExposure while restoring: {error}");
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while camera.camera_state().map(|state| state != CameraState::Idle).unwrap_or(false)
        && Instant::now() < deadline
    {
        std::thread::sleep(POLL);
    }
}

/// Opens [`DRIVER`], connects it and leaves the camera idle.
fn guarded() -> Restorer<Camera, Value> {
    let camera = Camera::open(&DeviceSpec::new(DRIVER)).expect("open camera driver");
    camera.set_connected(true).expect("connect");
    restore_idle(&camera);
    Restorer::new(camera, CASES, DRIVER)
}

/// The `SubFrame` value of a snapshot.
fn frame_value(snapshot: &[(&'static Case<Camera, Value>, Value)]) -> Value {
    snapshot
        .iter()
        .find(|(candidate, _)| candidate.member == "SubFrame")
        .expect("SubFrame is in the table")
        .1
}

/// Waits for a written value to be readable, then reports what the member says.
fn wait_for(
    camera: &Camera,
    case: &'static Case<Camera, Value>,
    wanted: Value,
) -> Option<Value> {
    let deadline = Instant::now() + APPLY_TIMEOUT;
    let mut now = (case.get)(camera).ok();
    while !now.is_some_and(|value| value.same(wanted)) && Instant::now() < deadline {
        std::thread::sleep(POLL);
        now = (case.get)(camera).ok();
    }
    now
}

/// read → write a different value → read back, for every writable member the driver
/// answers. The run itself is shared with the telescope file and lives in
/// `tests/common/mod.rs`; what stays here is the table and the camera's own tests.
#[serial]
#[test]
fn writable_properties_round_trip() {
    common::writable_round_trip(&guarded());
}

/// A member the driver does not implement must refuse the **write** the same way it
/// refuses the read. A write that is accepted but lands nowhere is invisible to the
/// client, which then believes the camera is configured the way it asked.
#[serial]
#[test]
fn unimplemented_members_refuse_writes() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();
    let mut implemented = 0;

    for case in CASES {
        match (case.get)(camera) {
            Ok(_) => implemented += 1,
            Err(error) if error.is_unsupported() => {
                let wanted = (case.other)(camera, case.seed);
                match (case.set)(camera, wanted) {
                    Ok(()) => failures.push(format!(
                        "{}: accepted {wanted:?} although reading it raises Unsupported",
                        case.member
                    )),
                    Err(error) if error.is_unsupported() => {
                        println!("note: {} refuses reads and writes alike", case.member);
                    }
                    Err(error) if is_binding(&error) => failures.push(format!(
                        "{}: write failed in the binding layer: {error}",
                        case.member
                    )),
                    Err(error) => deviations.push(format!(
                        "{}: spec wants PropertyNotImplemented, driver answered {:?} ({error})",
                        case.member, error.kind
                    )),
                }
            }
            Err(error) => println!("note: {}: read failed with {:?}", case.member, error.kind),
        }
    }

    // Nothing to prove if the driver implements the whole table.
    println!("note: {implemented} of {} members are implemented", CASES.len());
    report(&failures, &deviations);
}

/// A value the spec rejects must be refused, and the refusal must not change anything.
/// `SubFrame` is refused by this wrapper's own frame guard, which is stricter than the
/// spec; the error text carries which layer answered.
#[serial]
#[test]
fn out_of_range_values_are_refused() {
    common::refuses_out_of_range_values(&guarded());
}

/// `BinX`/`BinY` straight to the driver, stepped around the guard in
/// `Camera::set_binning`.
///
/// The wrapper rejects an impossible binning before the driver sees it, so testing
/// *the driver's* answer — which the wrapper then has to classify — requires going
/// around that check, exactly as `move_raw` does for the focuser.
fn bin_raw(camera: &Camera, member: &str, value: i32) -> Result<()> {
    let member = member.to_string();
    camera.actor().call(move |device| device.dispatch().set_i32(&member, value))
}

/// What the driver itself says to a binning above `MaxBinX`, which the spec puts at
/// `InvalidValue`. The wrapper's guard normally answers first; here it does not.
#[serial]
#[test]
fn driver_refuses_a_binning_above_max_bin() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let max_x = camera.max_bin_x().expect("MaxBinX is mandatory");
    let asked = max_x + 1;
    let before = camera.bin_x().expect("BinX is mandatory");
    match bin_raw(camera, "BinX", asked) {
        Ok(()) => deviations.push(format!("BinX accepted {asked}, although MaxBinX is {max_x}")),
        Err(error) if is_binding(&error) => {
            failures.push(format!("BinX = {asked} failed in the binding layer: {error}"));
        }
        Err(error) if error.kind != AscomErrorKind::InvalidValue => deviations.push(format!(
            "BinX: spec wants InvalidValue for {asked}, driver answered {:?} ({error})",
            error.kind
        )),
        Err(error) => println!("note: the driver refused BinX = {asked}: {error}"),
    }

    let after = camera.bin_x().expect("BinX after the attempt");
    if after != before {
        failures.push(format!("BinX moved from {before} to {after} on a write of {asked}"));
    }
    report(&failures, &deviations);
}

/// Every write while disconnected must raise `NotConnected`. For `BinX`/`BinY` and the
/// frame members the refusal comes from the limit reads this wrapper has to do first
/// (`MaxBinX`, `CameraXSize`), which the driver refuses the same way.
#[serial]
#[test]
fn writes_while_disconnected_are_refused() {
    common::refuses_writes_while_disconnected(&guarded());
}

/// The frame members are the write with a real apply step: the spec promises that
/// `NumX`/`NumY` are the dimensions of the next `ImageArray`, so the change is only
/// proven by an exposure, not by a read-back.
#[serial]
#[test]
fn a_written_frame_is_the_next_image() {
    let guard = guarded();
    let camera = guard.device();
    let before = guard.snapshot();
    let current = frame_value(&before);

    let (max_x, max_y) = limits(camera);
    let start_x = 2;
    let start_y = 3;
    let num_x = 64.min(max_x - start_x).max(1);
    let mut num_y = 32.min(max_y - start_y).max(1);
    if num_x == num_y {
        // A square frame cannot tell width from height, and that is the trap this
        // crate exists to avoid, so make it non-square.
        num_y = (num_y / 2).max(1);
    }
    let mut wanted = Value::Frame(start_x, start_y, num_x, num_y);
    if wanted.same(current) {
        wanted = Value::Frame(start_x + 1, start_y, num_x, num_y);
    }
    // Written from `wanted`, so that the shift above cannot be dropped on the way here.
    let (start_x, start_y, num_x, num_y) = wanted.frame().expect("frame built above");

    camera
        .set_sub_frame(start_x, start_y, num_x, num_y)
        .unwrap_or_else(|error| panic!("writing the {wanted:?} frame must succeed: {error}"));
    let now = wait_for(camera, case("SubFrame"), wanted);
    assert_eq!(now, Some(wanted), "the frame must read back as written");

    let duration = camera.exposure_min().unwrap_or(0.01).max(0.01);
    let image = camera
        .expose(duration, true, WaitSpec::new(EXPOSURE_TIMEOUT))
        .expect("short exposure of the written frame");
    assert_eq!(image.width(), num_x as usize, "NumX must be the width of ImageArray");
    assert_eq!(image.height(), num_y as usize, "NumY must be the height of ImageArray");
    println!("note: {wanted:?} produced a {}x{} image", image.width(), image.height());

    // The spec keeps the promise right after the exposure too, not only before it.
    let after = (case("SubFrame").get)(camera).expect("frame members after an exposure");
    assert_eq!(after, wanted, "NumX/NumY must still describe the frame just delivered");
    let state = camera.camera_state().expect("CameraState after an exposure");
    assert_eq!(state, CameraState::Idle, "camera must be idle again");
}

/// Refusing an impossible frame must leave the geometry alone. The spec puts this check
/// in `StartExposure` rather than in the property writes, so the wrapper's guard is the
/// stricter of the two and it is what answers here.
#[serial]
#[test]
fn a_frame_that_does_not_fit_is_refused() {
    let guard = guarded();
    let camera = guard.device();
    let before = guard.snapshot();
    let current = frame_value(&before).frame().expect("SubFrame snapshot");

    let sensor_x = camera.camera_x_size().expect("CameraXSize");
    let sensor_y = camera.camera_y_size().expect("CameraYSize");
    let error = camera
        .set_sub_frame(0, 0, sensor_x + 10, sensor_y)
        .expect_err("a frame running past the sensor must be refused");
    println!("note: an off-sensor frame was refused by {error}");
    assert_eq!(error.kind, AscomErrorKind::InvalidValue, "refusal must be InvalidValue");

    let after = (case("SubFrame").get)(camera).expect("frame members after a refused write");
    assert_eq!(after.frame().expect("frame snapshot"), current, "a refused write must change nothing");

    // The limit is expressed in binned pixels, so at Bin 2 the same sensor accepts half
    // of what it accepted at Bin 1 — and no more than half. Getting this wrong either
    // way shows up here: too strict rejects a legal frame, too lax lets through one the
    // driver would only reject at StartExposure (docs/KNOWN_DRIVER_QUIRKS.md §2.13).
    camera.set_binning(2, 2).expect("binning for the binned-limit check");
    let (binned_x, binned_y) = (sensor_x / 2, sensor_y / 2);
    camera
        .set_sub_frame(0, 0, binned_x, binned_y)
        .unwrap_or_else(|error| panic!("the {binned_x}x{binned_y} frame fits at Bin 2: {error}"));
    let fitted = (case("SubFrame").get)(camera).expect("frame members at Bin 2");
    assert_eq!(fitted, Value::Frame(0, 0, binned_x, binned_y), "the binned extent must be accepted");

    let error = camera
        .set_sub_frame(0, 0, binned_x + 1, binned_y)
        .expect_err("one binned pixel past the limit must be refused");
    println!("note: one pixel past the Bin 2 limit was refused by {error}");
    assert_eq!(error.kind, AscomErrorKind::InvalidValue, "refusal must be InvalidValue");
    let after = (case("SubFrame").get)(camera).expect("frame members after the second refusal");
    assert_eq!(after, fitted, "a refused write must change nothing here either");
}

/// `CoolerOn` is the write whose effect takes time: the spec promises `CoolerPower == 0`
/// while it is false, and a simulator that models cooling starts moving
/// `CCDTemperature` within a second or two of turning it on.
#[serial]
#[test]
fn cooler_on_moves_the_detector_temperature() {
    let guard = guarded();
    let camera = guard.device();
    let caps = camera.capabilities().expect("capabilities");
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let start = camera.cooler_on().expect("CoolerOn is mandatory");
    let temperature = camera.ccd_temperature().expect("CCDTemperature is mandatory");
    let wanted = !start;
    camera
        .set_cooler_on(wanted)
        .unwrap_or_else(|error| panic!("writing CoolerOn = {wanted} must succeed: {error}"));

    let deadline = Instant::now() + APPLY_TIMEOUT;
    while camera.cooler_on().unwrap_or(start) != wanted && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
    if camera.cooler_on().unwrap_or(start) != wanted {
        failures.push(format!("CoolerOn does not read back {wanted}"));
    }

    // Watching the effect is information, not a requirement: the spec does not oblige a
    // simulator to model a thermal response at all.
    let mut extremes = vec![temperature];
    let watch = Instant::now() + Duration::from_secs(3);
    while Instant::now() < watch {
        match camera.ccd_temperature() {
            Ok(value) => extremes.push(value),
            Err(error) => failures.push(format!("CCDTemperature: {error}")),
        }
        std::thread::sleep(POLL);
    }
    let drift = if wanted {
        temperature - extremes.iter().cloned().fold(f64::INFINITY, f64::min)
    } else {
        extremes.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - temperature
    };
    println!(
        "note: CoolerOn = {wanted} moved CCDTemperature by {drift:+.2} C from {temperature:.2} C \
         (CanGetCoolerPower = {:?})",
        caps.flag("CanGetCoolerPower")
    );
    if wanted && drift <= 0.0 {
        println!("note: the driver models no thermal response to the cooler");
    }

    // Back to how it was found, then the two hard promises about cooler power.
    camera.set_cooler_on(start).expect("restore CoolerOn");
    let can_read_power = caps.flag("CanGetCoolerPower");
    match camera.cooler_power() {
        Err(error) if is_binding(&error) => failures.push(format!("CoolerPower: {error}")),
        Ok(power) => {
            if can_read_power == Some(false) {
                deviations.push(format!(
                    "CoolerPower reads {power} although CanGetCoolerPower is false, spec wants PropertyNotImplemented"
                ));
            }
            // The spec's own note: power must be zero whenever the cooler is off.
            if power != 0.0 {
                deviations.push(format!(
                    "CoolerPower reads {power} while CoolerOn is false; the spec requires zero"
                ));
            } else {
                println!("note: CoolerPower reads 0 with the cooler off, as the spec requires");
            }
        }
        Err(error) => println!("note: CoolerPower refused with {:?}: {error}", error.kind),
    }

    report(&failures, &deviations);
}

// ------------------------------------------- frame geometry and exposure lifecycle ---
//
// Everything below delivers a real frame. The geometry members are only proven by the
// `ImageArray` that follows them (`NumX`/`NumY` are promises about the next transfer),
// and the exposure lifecycle — a dark frame, an abort — cannot be checked by a read-back
// at all.
//
// What DRIVER reports, measured for these tests (docs/KNOWN_DRIVER_QUIRKS.md §2.19–2.20):
// an 800x600 monochrome sensor, MaxBinX = MaxBinY = 4, CanAsymmetricBin = true,
// ExposureMin = 0.001 s, ExposureMax = 3600 s, MaxADU = 65535, CanAbortExposure = true.

/// `StartExposure` straight to the driver, stepped around the wrapper's own duration
/// check so that a negative case measures the driver rather than `ascom::camera`.
fn exposure_raw(camera: &Camera, duration: f64, light: bool) -> Result<()> {
    camera.actor().call(move |device| {
        let duration = Variant::from_f64(duration);
        let light = Variant::from_bool(light);
        device.dispatch().call_void("StartExposure", &[&duration, &light])
    })
}

/// `NumX` straight to the driver, around the frame guard in `Camera::set_sub_frame`.
fn num_x_raw(camera: &Camera, num_x: i32) -> Result<()> {
    camera.actor().call(move |device| device.dispatch().set_i32("NumX", num_x))
}

/// The shortest exposure this camera accepts for a light frame.
fn shortest(camera: &Camera) -> f64 {
    camera.exposure_min().unwrap_or(0.01).max(0.01)
}

/// A non-square extent that fits the binned limit with one pixel of origin to spare.
///
/// Non-square because a square frame cannot tell width from height, and an origin other
/// than zero because a frame at `0,0` cannot show a stale `StartX`/`StartY`.
fn non_square(limit_x: i32, limit_y: i32) -> (i32, i32) {
    let num_x = i32::min(48, limit_x - 1).max(1);
    let mut num_y = i32::min(24, limit_y - 1).max(1);
    if num_x == num_y {
        num_y = (num_y / 2).max(1);
    }
    (num_x, num_y)
}

/// Every binning the camera allows must deliver a frame whose dimensions are the binned
/// `NumX`/`NumY`. Walked from 1 to `MaxBinX` rather than over 1/2/4: the spec sets no
/// power-of-two rule, so an odd binning has to work as well.
#[serial]
#[test]
fn an_image_at_every_supported_binning() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let sensor_x = camera.camera_x_size().expect("CameraXSize is mandatory");
    let sensor_y = camera.camera_y_size().expect("CameraYSize is mandatory");
    let max_bin =
        i32::min(camera.max_bin_x().expect("MaxBinX is mandatory"), camera.max_bin_y().expect("MaxBinY"));
    let duration = shortest(camera);

    for bin in 1..=i32::max(max_bin, 1) {
        if let Err(error) = camera.set_binning(bin, bin) {
            failures.push(format!("set_binning({bin},{bin}): {error}"));
            break;
        }
        // The limit is per axis and expressed in binned pixels, so it shrinks with bin.
        let (num_x, num_y) = non_square(sensor_x / bin, sensor_y / bin);
        if let Err(error) = camera.set_sub_frame(1, 1, num_x, num_y) {
            failures.push(format!("sub-frame {num_x}x{num_y}+1+1 at bin {bin}: {error}"));
            break;
        }
        let image = match camera.expose(duration, true, WaitSpec::new(EXPOSURE_TIMEOUT)) {
            Ok(image) => image,
            Err(error) => {
                failures.push(format!("exposure at bin {bin}x{bin}: {error}"));
                break;
            }
        };
        if image.width() != num_x as usize || image.height() != num_y as usize {
            failures.push(format!(
                "bin {bin}x{bin}: ImageArray is {}x{}, NumX/NumY said {num_x}x{num_y}",
                image.width(),
                image.height()
            ));
        }
        let expected = image.width() * image.height() * image.planes();
        if image.data.len() != expected {
            failures.push(format!(
                "bin {bin}: the array holds {} elements, {expected} expected",
                image.data.len()
            ));
        }
        // The spec keeps the promise after the frame too, not only before it.
        let (now_x, now_y) = (camera.num_x().unwrap_or(-1), camera.num_y().unwrap_or(-1));
        if (now_x, now_y) != (num_x, num_y) {
            deviations.push(format!(
                "bin {bin}: NumX/NumY read {now_x}x{now_y} after a {num_x}x{num_y} frame"
            ));
        }
        println!(
            "note: bin {bin}x{bin} delivered {}x{} ({} elements, BinX={})",
            image.width(),
            image.height(),
            image.data.len(),
            camera.bin_x().unwrap_or(-1),
        );
    }

    report(&failures, &deviations);
}

/// The spec forbids the geometry members from checking compatibility with the binning and
/// puts that check in `StartExposure`, where `InvalidValue` is obligatory and its message
/// must say what was wrong. So an over-limit frame has to be **accepted** by the property
/// and refused by the exposure, without the driver quietly repairing the geometry.
///
/// Goes around `Camera::set_sub_frame`, whose own limit is deliberately stricter than the
/// spec's (it refuses such a frame before the driver sees it).
#[serial]
#[test]
fn a_crop_incompatible_with_binning_is_refused_at_start_exposure() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let sensor_x = camera.camera_x_size().expect("CameraXSize");
    if camera.max_bin_x().unwrap_or(1) < 2 || camera.max_bin_y().unwrap_or(1) < 2 {
        println!("note: this camera cannot bin 2x2, nothing to check here");
        return;
    }
    camera.set_binning(2, 2).expect("bin 2x2");
    let over = sensor_x / 2 + 1;

    match num_x_raw(camera, over) {
        Ok(()) => {}
        Err(error) if is_binding(&error) => {
            failures.push(format!("writing NumX = {over} failed in the binding layer: {error}"));
            report(&failures, &deviations);
            return;
        }
        Err(error) => deviations.push(format!(
            "the spec forbids checking the frame on write, the driver refused NumX = {over} \
             with {:?} ({error})",
            error.kind
        )),
    }
    let written = camera.num_x().unwrap_or(-1);
    println!("note: NumX = {written} accepted at Bin 2 (binned limit would be {})", sensor_x / 2);

    let error = match exposure_raw(camera, shortest(camera), true) {
        Ok(()) => {
            // Hand the over-limit frame back to the guard: it has to go away.
            deviations.push(format!(
                "StartExposure accepted a frame of {written} columns at Bin 2, spec wants InvalidValue"
            ));
            report(&failures, &deviations);
            return;
        }
        Err(error) => error,
    };
    assert!(
        !is_binding(&error),
        "StartExposure with an over-limit frame failed in the binding layer: {error}"
    );
    if error.kind != AscomErrorKind::InvalidValue {
        deviations.push(format!(
            "spec wants InvalidValue for an incompatible frame, driver answered {:?} ({error})",
            error.kind
        ));
    }
    // The spec calls an explanatory message "vital" here.
    if !error.message.contains("NumX") && !error.message.contains("frame") {
        deviations.push(format!("the refusal does not name the offending geometry: {error}"));
    }
    println!("note: StartExposure refused the over-limit frame: {}", error.message);

    let state = camera
        .camera_state()
        .unwrap_or_else(|error| panic!("CameraState after a refused StartExposure: {error}"));
    if state != CameraState::Idle {
        failures.push(format!("a refused StartExposure left CameraState at {state:?}"));
    }
    if camera.num_x().unwrap_or(-1) != written {
        deviations.push(format!(
            "the refused exposure changed NumX from {written} to {}",
            camera.num_x().unwrap_or(-1)
        ));
    }

    // And the camera must still work with a frame that does fit.
    let (num_x, num_y) = non_square(sensor_x / 2, camera.camera_y_size().unwrap_or(2) / 2);
    camera
        .set_sub_frame(0, 0, num_x, num_y)
        .unwrap_or_else(|error| panic!("writing a legal {num_x}x{num_y} frame: {error}"));
    let image = camera
        .expose(shortest(camera), true, WaitSpec::new(EXPOSURE_TIMEOUT))
        .expect("an exposure after the refusal must work");
    println!(
        "note: after the refusal a legal {num_x}x{num_y} frame still works: {}x{}",
        image.width(),
        image.height()
    );

    report(&failures, &deviations);
}

/// `CanAsymmetricBin` decides whether the two axes may differ. When it is true, an
/// asymmetric frame must arrive with exactly the dimensions the geometry members report
/// and the binned limit must shrink on one axis only; when it is false, the spec obliges
/// the driver to mirror the other axis on write.
#[serial]
#[test]
fn asymmetric_binning_agrees_with_the_capability_flag() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let caps = camera.capabilities().expect("capabilities");
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let sensor_x = camera.camera_x_size().expect("CameraXSize");
    let sensor_y = camera.camera_y_size().expect("CameraYSize");

    if caps.flag("CanAsymmetricBin") != Some(true) {
        // The rule for this branch: writing one axis sets the other to the same value.
        match camera.actor().call(|device| device.dispatch().set_i32("BinX", 2)) {
            Ok(()) => {
                let now_y = camera.bin_y().unwrap_or(-1);
                if now_y != 2 {
                    deviations.push(format!(
                        "CanAsymmetricBin is false, so writing BinX = 2 had to set BinY as \
                         well; BinY reads {now_y}"
                    ));
                }
            }
            Err(error) if is_binding(&error) => {
                failures.push(format!("writing BinX failed in the binding layer: {error}"))
            }
            Err(error) => deviations.push(format!(
                "spec wants BinY to follow BinX, driver answered {:?} ({error})",
                error.kind
            )),
        }
        println!("note: CanAsymmetricBin is false, checked the mirroring rule only");
        report(&failures, &deviations);
        return;
    }

    let (bin_x, bin_y) = (2, 1);
    camera
        .set_binning(bin_x, bin_y)
        .unwrap_or_else(|error| panic!("CanAsymmetricBin is true, so {bin_x}x{bin_y} must be accepted: {error}"));
    assert_eq!(
        (camera.bin_x().unwrap_or(0), camera.bin_y().unwrap_or(0)),
        (bin_x, bin_y),
        "the driver must report the asymmetric binning it accepted"
    );

    // The binned limit shrinks on the binned axis only, which a symmetric camera cannot
    // reproduce: 400 columns and 600 rows at one and the same time.
    let (limit_x, limit_y) = limits(camera);
    assert_eq!(limit_x, sensor_x / bin_x, "x limit must be binned");
    assert_eq!(limit_y, sensor_y / bin_y, "y limit must not be binned at BinY 1");
    let (num_x, num_y) = non_square(limit_x, limit_y);
    camera
        .set_sub_frame(0, 1, num_x, num_y)
        .unwrap_or_else(|error| panic!("{num_x}x{num_y} fits {limit_x}x{limit_y}: {error}"));

    let image = camera
        .expose(shortest(camera), true, WaitSpec::new(EXPOSURE_TIMEOUT))
        .expect("an asymmetric frame must expose");
    assert_eq!(image.width(), num_x as usize, "width must follow NumX at BinX {bin_x}");
    assert_eq!(image.height(), num_y as usize, "height must follow NumY at BinY {bin_y}");
    assert_ne!(image.width(), image.height(), "the frame must prove both axes");
    println!(
        "note: bin {bin_x}x{bin_y} delivered {}x{} from {num_x}x{num_y} (limits {limit_x}x{limit_y})",
        image.width(),
        image.height(),
    );

    // One pixel past the binned x limit is still the wrapper's business.
    let error = camera
        .set_sub_frame(0, 0, limit_x + 1, limit_y)
        .expect_err("one pixel past the x limit must be refused");
    assert_eq!(error.kind, AscomErrorKind::InvalidValue, "refusal must be InvalidValue");
    report(&failures, &deviations);
}

/// `StartExposure(0, false)` is how an application asks for a dark or bias frame, and the
/// spec obliges the driver to honour it although `ExposureMin` must be non-zero. After a
/// frame that was acquired successfully the two `LastExposure*` members must answer.
#[serial]
#[test]
fn a_dark_frame_of_zero_seconds_delivers_a_frame() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let (limit_x, limit_y) = limits(camera);
    let (num_x, num_y) = non_square(limit_x, limit_y);
    camera.set_sub_frame(1, 0, num_x, num_y).expect("a frame to check the dark exposure against");

    // ExposureMin is 0.001 here, so this only succeeds if the dark frame is exempt from
    // the lower bound — which is what `exposure_allowed` is for. That exemption is a
    // verified fact about this very driver (docs/KNOWN_DRIVER_QUIRKS.md §2.20), so an
    // "unimplemented" answer here is a regression in behaviour this harness records, not
    // a feature this camera happens to lack.
    let image = match camera.expose(0.0, false, WaitSpec::new(EXPOSURE_TIMEOUT)) {
        Ok(image) => image,
        Err(error) if error.is_unsupported() => {
            deviations.push(format!(
                "StartExposure(0, false) answered {:?} although this driver is known to \
                 honour a zero-second dark frame: {error}",
                error.kind
            ));
            report(&failures, &deviations);
            return;
        }
        Err(error) => {
            failures.push(format!("StartExposure(0, false): {error}"));
            report(&failures, &deviations);
            return;
        }
    };
    assert_eq!(image.width(), num_x as usize, "a dark frame obeys the same geometry");
    assert_eq!(image.height(), num_y as usize, "a dark frame obeys the same geometry");
    assert!(!image.data.is_empty(), "a dark frame must still carry pixels");
    println!(
        "note: dark frame {}x{}, {:?}",
        image.width(),
        image.height(),
        image.stats().map(|stats| (stats.min, stats.max)),
    );

    // Both members are optional (PropertyNotImplemented is allowed), but once they answer
    // they must describe the frame just taken. "Before the first frame" is not
    // reproducible here: the simulator is one shared singleton whose state survives.
    match camera.last_exposure_duration() {
        Ok(duration) if duration >= 0.0 => println!("note: LastExposureDuration = {duration} s"),
        Ok(duration) => failures.push(format!("LastExposureDuration is {duration}, below zero")),
        Err(error) if error.is_unsupported() => println!("note: LastExposureDuration not implemented"),
        Err(error) => failures.push(format!(
            "LastExposureDuration must answer after a successful frame: {error}"
        )),
    }
    match camera.last_exposure_start_time() {
        Ok(text) => println!("note: LastExposureStartTime = {text}"),
        Err(error) if error.is_unsupported() => println!("note: LastExposureStartTime not implemented"),
        Err(error) => failures.push(format!(
            "LastExposureStartTime must answer after a successful frame: {error}"
        )),
    }

    report(&failures, &deviations);
}

/// `AbortExposure` must discard the frame in flight and bring the camera back to idle,
/// must not raise when the camera is already idle, and must leave the camera usable.
#[serial]
#[test]
fn aborting_an_exposure_returns_the_camera_to_idle() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let caps = camera.capabilities().expect("capabilities");
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    if caps.flag("CanAbortExposure") != Some(true) {
        println!("note: CanAbortExposure is not true, nothing to abort");
        return;
    }
    restore_idle(camera);

    // The spec puts PercentCompleted outside idle; a driver that answers 0 here is
    // reporting progress that does not exist.
    if let Ok(percent) = camera.percent_completed() {
        deviations.push(format!(
            "PercentCompleted reads {percent} while CameraState is Idle, spec wants InvalidOperationException"
        ));
    }

    // Long enough to still be running when the abort is issued.
    let long = f64::min(camera.exposure_max().unwrap_or(30.0), 5.0).max(shortest(camera));
    camera.start_exposure(long, true).expect("a long exposure must start");

    let state = camera.camera_state().unwrap_or_else(|error| panic!("CameraState mid-exposure: {error}"));
    if state == CameraState::Idle {
        deviations.push(format!("CameraState is Idle although an exposure of {long} s is running"));
    }
    assert!(!camera.image_ready().unwrap_or(true), "ImageReady must be false while exposing");
    match camera.percent_completed() {
        Ok(percent) => println!("note: PercentCompleted mid-exposure = {percent}"),
        Err(error) => failures.push(format!("PercentCompleted while {state:?}: {error}")),
    }
    // Reading the frame too early is the spec's InvalidOperationException; this wrapper
    // answers it from ImageReady rather than letting the driver fail obscurely.
    let early = camera.read_image().expect_err("no frame may be read before ImageReady");
    assert_eq!(early.kind, AscomErrorKind::InvalidOperation, "too early: {early}");

    camera.abort_exposure().expect("AbortExposure");
    let deadline = Instant::now() + Duration::from_secs(10);
    // A binding-layer failure must not stand in for the state under test: defaulting it
    // to Idle would end the wait at once and pass the assertion below on a value the
    // driver never reported.
    while camera
        .camera_state()
        .unwrap_or_else(|error| panic!("CameraState after abort: {error}"))
        != CameraState::Idle
        && Instant::now() < deadline
    {
        std::thread::sleep(POLL);
    }
    let after = camera
        .camera_state()
        .unwrap_or_else(|error| panic!("CameraState after abort: {error}"));
    assert_eq!(after, CameraState::Idle, "AbortExposure must return the camera to idle");

    // The discarded data must not be readable: the driver says there is no frame, and the
    // wrapper turns that into InvalidOperationException rather than a stale array.
    assert!(!camera.image_ready().unwrap_or(true), "ImageReady must be false after an abort");
    let discarded = camera
        .read_image()
        .expect_err("an aborted exposure must leave no frame behind");
    assert_eq!(discarded.kind, AscomErrorKind::InvalidOperation, "after abort: {discarded}");

    camera.abort_exposure().expect("AbortExposure at idle must not raise");

    let image = camera
        .expose(shortest(camera), true, WaitSpec::new(EXPOSURE_TIMEOUT))
        .expect("the camera must still expose normally after an abort");
    println!("note: after the abort the camera delivered {}x{}", image.width(), image.height());

    report(&failures, &deviations);
}

/// `MaxADU` must answer before any frame and must describe the frame that arrives: the
/// spec ties it to the bit depth, the pixel type must carry that depth, and no pixel may
/// exceed the stated maximum. The plane count comes from `SensorType`.
#[serial]
#[test]
fn the_frame_agrees_with_max_adu_and_the_sensor_type() {
    let guard = guarded();
    let camera = guard.device();
    guard.snapshot();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    let adu_max = match camera.max_adu() {
        Ok(value) => value,
        Err(error) => {
            failures.push(format!("MaxADU is mandatory and must answer before a frame: {error}"));
            report(&failures, &deviations);
            return;
        }
    };
    let sensor = camera.sensor_type().expect("SensorType is mandatory");
    assert!(adu_max > 0, "MaxADU = {adu_max} cannot describe a real converter");

    let image = camera
        .expose(shortest(camera), true, WaitSpec::new(EXPOSURE_TIMEOUT))
        .expect("a short frame");

    if image.bit_depth != bits_for_adu_max(adu_max) {
        failures.push(format!(
            "the frame is {} bit while MaxADU = {adu_max} describes {} bit",
            image.bit_depth,
            bits_for_adu_max(adu_max),
        ));
    }
    if image.planes() != sensor.planes() {
        deviations.push(format!(
            "{sensor:?} transfers {} planes, the frame carries {}",
            sensor.planes(),
            image.planes(),
        ));
    }
    let stats = image.stats().expect("a delivered frame has pixels");
    if stats.max > adu_max as f64 {
        deviations.push(format!(
            "a pixel reaches {:?} although MaxADU is {adu_max}",
            stats.max
        ));
    }
    println!(
        "note: MaxADU = {adu_max} -> {} bit (FITS BITPIX {}), sensor {sensor:?}, {} plane(s), \
         frame values {:?}..{:?}",
        image.bit_depth,
        image.data.bitpix(),
        image.planes(),
        stats.min,
        stats.max,
    );

    report(&failures, &deviations);
}
