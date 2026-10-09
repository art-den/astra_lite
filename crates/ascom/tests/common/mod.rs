//! Shared helpers for the live-driver tests.
//!
//! These tests talk to a **live** ASCOM driver and read the ProgID of the device
//! under test from an environment variable:
//!
//! ```text
//! ASCOM_FOCUSER_PROG_ID=ASCOM.OmniSim.Focuser cargo test --test focuser
//! ```

#![allow(dead_code)]

use ascom::com::dispatch::NOT_IMPLEMENTED;
use ascom::device::AscomDevice;
use ascom::error::{AscomError, AscomErrorKind, Result};
use std::cell::{Cell, RefCell};
use std::fmt::Debug;
use std::time::{Duration, Instant};

/// The ProgID from `var`, or `fallback` (the OmniSim driver for that family).
pub fn prog_id(var: &str, fallback: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| fallback.to_string())
}

/// Walks every read-only member of an interface through [`AscomDevice::probe`].
///
/// The first checklist item for an interface: every member must answer with a value or
/// an "unimplemented" style domain error — never a panic, never a hang, and never
/// a *binding-layer* error (`Com`/`NotFound`/`Disconnected`/`Timeout`), which would
/// mean the late binding in this wrapper, not the driver, is at fault.
///
/// Because an object that simply does not expose a name also counts as
/// "unimplemented" (see `AscomErrorKind::from_hresult`), a name **table** that is
/// wholly wrong would pass silently. The distribution is therefore printed, and a
/// walk in which nothing at all is implemented is treated as a failure.
pub fn walk_members<D: AscomDevice>(device: &D, members: &[&str]) {
    let mut offenders = Vec::new();
    let mut unimplemented = Vec::new();
    for member in members {
        // `probe` returns Ok(rendered) or Err(classified); it cannot panic or hang
        // unless the driver itself hangs (which the harness would time out on).
        match device.probe(member) {
            Ok(value) => {
                if value == NOT_IMPLEMENTED {
                    unimplemented.push(*member);
                }
            }
            Err(error) => {
                let binding_bug = matches!(
                    error.kind,
                    AscomErrorKind::Com
                        | AscomErrorKind::NotFound
                        | AscomErrorKind::Disconnected
                        | AscomErrorKind::Timeout
                );
                if binding_bug {
                    offenders.push(format!("{member}: {:?}", error.kind));
                } else if error.is_unsupported() {
                    unimplemented.push(*member);
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "members that failed with a binding-layer error (wrapper bug, not driver):\n{}",
        offenders.join("\n")
    );
    if !unimplemented.is_empty() {
        println!(
            "note: {} of {} members reported not implemented: {:?}",
            unimplemented.len(),
            members.len(),
            unimplemented
        );
    }
    assert!(
        unimplemented.len() < members.len(),
        "no member of this interface is implemented — wrong ProgID, or a member table that does not match the driver"
    );
}

/// An unknown `Action` name must be **rejected**, never silently accepted.
///
/// Two verified deviations shape this check:
/// * `Action` needs **two** arguments; a one-argument call yields `E_INVALIDARG`.
/// * The OmniSim COM proxy throws a raw .NET parse exception when
///   `ActionParameters` is empty, so an empty string is retried with `"0"`.
///
/// The spec accepts two answers here: `ActionNotImplementedException` for a driver
/// that has actions but not this one, and plain `MethodNotImplementedException`
/// (`Unsupported`) for a driver that implements no actions at all. Anything else is
/// a deviation, which this reports rather than hides.
/// Set `ASCOM_STRICT_SPEC=1` to turn a deviation into a failure.
pub fn check_unknown_action<D: AscomDevice>(device: &D) {
    let name = "__definitely_not_a_real_action__";
    let mut outcome = device.action(name, "");
    if let Err(error) = &outcome {
        // Only the driver's raw CLR exception justifies a retry; a genuine wrapper
        // `Com` must fail fast, not be retried into a pass.
        if is_raw_managed(error) {
            println!(
                "note: Action({name}, \"\") raised a raw non-ASCOM error ({error}); \
                 retrying with a non-empty parameter string"
            );
            outcome = device.action(name, "0");
        }
    }
    let error = match outcome {
        Ok(answer) => panic!("unknown action unexpectedly returned {answer:?}"),
        Err(error) => error,
    };
    assert!(
        !is_binding(&error),
        "unknown Action must be rejected by the driver, not by the binding layer: {error}"
    );
    if !matches!(
        error.kind,
        AscomErrorKind::ActionNotImplemented | AscomErrorKind::Unsupported
    ) {
        println!(
            "DRIVER DEVIATION: spec wants ActionNotImplemented/MethodNotImplemented, \
             driver answered {:?} ({error})",
            error.kind
        );
        if std::env::var("ASCOM_STRICT_SPEC").is_ok() {
            panic!("driver is not spec-conformant (ASCOM_STRICT_SPEC is set)");
        }
    }
}

/// The five identity members every interface version requires.
pub fn check_identity_members<D: AscomDevice>(device: &D) -> Result<()> {
    let version = device.interface_version()?;
    assert!(version >= 2, "InterfaceVersion should be >= 2, got {version}");
    assert!(!device.name()?.trim().is_empty(), "Name must not be blank");
    assert!(!device.description()?.trim().is_empty(), "Description must not be blank");
    assert!(!device.driver_info()?.trim().is_empty(), "DriverInfo must not be blank");
    assert!(!device.driver_version()?.trim().is_empty(), "DriverVersion must not be blank");
    Ok(())
}

/// `Connected = true` twice, then `false` twice, is idempotent and each read agrees.
pub fn check_connected_idempotent<D: AscomDevice>(device: &D) -> Result<()> {
    device.set_connected(true)?;
    device.set_connected(true)?;
    assert!(device.connected()?, "connected must read true after two sets to true");
    device.set_connected(false)?;
    device.set_connected(false)?;
    assert!(!device.connected()?, "connected must read false after two sets to false");
    Ok(())
}

/// An *operational* property read while disconnected must raise `NotConnected`
/// (or `Unsupported`). Some simulators are lenient and still answer; that is
/// recorded, not failed, but any *other* error is a real problem.
pub fn check_operational_needs_connection<D: AscomDevice>(
    device: &D,
    member: &str,
) -> Result<()> {
    device.set_connected(false)?;
    match device.probe(member) {
        Ok(value) => {
            println!("note: {member} answered {value:?} while disconnected (lenient driver)");
        }
        Err(error) => assert!(
            matches!(error.kind, AscomErrorKind::NotConnected | AscomErrorKind::Unsupported),
            "{member} while disconnected must be NotConnected/Unsupported, got {:?}",
            error.kind
        ),
    }
    Ok(())
}

// ------------------------------------------------------------- write tests ---
//
// Every interface with writable members runs the same three table-driven tests
// under the same guard, so the machinery lives here. A test file then keeps its
// `CASES` table, its `guarded()` and the tests that only make sense for that
// interface; the third copy of the same guard is what this replaced.

/// Grace period for a written value to become visible. Most writable members have no
/// completion property and answer with the new value immediately; the interfaces that
/// do have one (`IsMoving`, `Slewing`, `ImageReady`) wait on it in their own test.
pub const APPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll step; the spec asks callers not to poll faster than this.
pub const POLL: Duration = Duration::from_millis(100);

/// True for failures of the binding layer, which are never the driver's fault.
///
/// One exception: a driver that lets a managed exception escape also arrives as `Com`,
/// and that is the driver's doing. [`is_raw_managed`] separates the two.
pub fn is_binding(error: &AscomError) -> bool {
    matches!(
        error.kind,
        AscomErrorKind::Com
            | AscomErrorKind::NotFound
            | AscomErrorKind::Disconnected
            | AscomErrorKind::Timeout
    )
}

/// True when a `Com` failure is really a managed exception the driver let escape.
///
/// `FACILITY_URT` (0x13) in the HRESULT means the CLR raised instead of an ASCOM
/// exception, which the wrapper cannot classify any better than `Com`. The OmniSim
/// drivers do it for an `Action` with empty parameters and for a second `Position`
/// write while the wheel is turning, so such a failure is a driver deviation rather
/// than a fault of this binding, and a test that expects a domain error must tell the
/// two `Com` cases apart.
pub fn is_raw_managed(error: &AscomError) -> bool {
    error.kind == AscomErrorKind::Com && error.hresult & 0x001F_0000 == 0x0013_0000
}

/// Reports a driver deviation, failing only when `ASCOM_STRICT_SPEC` is set — the same
/// convention `check_unknown_action` above uses.
pub fn deviation(what: &str) {
    println!("DRIVER DEVIATION: {what}");
    if std::env::var("ASCOM_STRICT_SPEC").is_ok() {
        panic!("driver is not spec-conformant (ASCOM_STRICT_SPEC is set): {what}");
    }
}

/// Ends a table run: wrapper faults always fail the test, driver deviations only under
/// `ASCOM_STRICT_SPEC`.
pub fn report(failures: &[String], deviations: &[String]) {
    for note in deviations {
        println!("DRIVER DEVIATION: {note}");
    }
    assert!(failures.is_empty(), "writable members that failed:\n{}", failures.join("\n"));
    if !deviations.is_empty() && std::env::var("ASCOM_STRICT_SPEC").is_ok() {
        panic!(
            "driver is not spec-conformant (ASCOM_STRICT_SPEC is set): {} deviation(s)",
            deviations.len()
        );
    }
}

/// One writable member of an interface: how to read it, how to write it, what to write
/// instead, and which value the specification puts outside the allowed range.
///
/// `V` is the test file's own value type; it only has to compare two values with the
/// tolerance the driver deserves (implement `PartialEq` in terms of that comparison).
pub struct Case<D, V> {
    pub member: &'static str,
    /// `CanXxx` gate; `None` where the spec defines none.
    pub gate: Option<&'static str>,
    pub get: fn(&D) -> Result<V>,
    pub set: fn(&D, V) -> Result<()>,
    /// A different, still legal value for the round trip. Bounds usually come from the
    /// driver, so the closure gets the handle.
    pub other: fn(&D, V) -> V,
    /// A value the spec rejects; `None` where the spec gives no numbers to break.
    pub bad: Option<fn(&D) -> V>,
    /// Stand-in for the current value in tests that cannot read the member.
    pub seed: V,
    /// State the write needs; a row whose precondition is not met is skipped.
    pub precondition: fn(&D) -> Result<bool>,
}

/// How to put one member back, and how to check that it took.
struct Undo<D, V> {
    member: &'static str,
    saved: V,
    get: fn(&D) -> Result<V>,
    set: fn(&D, V) -> Result<()>,
}

/// Snapshots every member of a table before the first write and puts it all back on
/// the way out, even when the test panics.
///
/// The snapshot is taken up front rather than per row because members written together
/// can move each other (the spec lets one guide-rate write change the other, and a
/// sub-frame is four members at once), so repair has to work from what the driver
/// reported at the start.
///
/// Panicking from `Drop` is only allowed when the test itself passed: a panic while
/// another panic is unwinding aborts the process and hides the real failure, so during
/// an unwind this only prints `RESTORE FAILED`. It matters because the simulator is one
/// shared singleton — state left behind breaks every test that runs after it.
pub struct Restorer<D: AscomDevice + 'static, V: Copy + PartialEq + Debug + 'static> {
    device: D,
    rows: &'static [Case<D, V>],
    undo: RefCell<Vec<Undo<D, V>>>,
    /// `Cell`, so that marking from a test already holding `&D` stays a borrow-checker
    /// non-event.
    disconnected: Cell<bool>,
    driver: &'static str,
}

impl<D: AscomDevice + 'static, V: Copy + PartialEq + Debug + 'static> Restorer<D, V> {
    pub fn new(device: D, rows: &'static [Case<D, V>], driver: &'static str) -> Self {
        Self {
            device,
            rows,
            undo: RefCell::new(Vec::new()),
            disconnected: Cell::new(false),
            driver,
        }
    }

    pub fn device(&self) -> &D {
        &self.device
    }

    /// Reads and remembers every member of the table. Members the driver refuses to
    /// answer are reported and left out, which is how a member this driver does not
    /// implement drops out of the round trip. A binding-layer failure is the one
    /// exception: it panics, because a member missing from `undo` would silently skip
    /// its tests and leave the shared simulator unrestored.
    pub fn snapshot(&self) -> Vec<(&'static Case<D, V>, V)> {
        let mut out = Vec::new();
        for row in self.rows {
            match (row.get)(&self.device) {
                Ok(value) => {
                    self.undo.borrow_mut().push(Undo {
                        member: row.member,
                        saved: value,
                        get: row.get,
                        set: row.set,
                    });
                    out.push((row, value));
                }
                // Nothing to snapshot yet is normal: Site*/Target* may answer
                // InvalidOperation before their first write, and an unimplemented
                // member has no value at all.
                Err(error)
                    if error.is_unsupported() || error.kind == AscomErrorKind::InvalidOperation =>
                {
                    println!("note: {} has nothing to snapshot yet ({:?})", row.member, error.kind);
                }
                // Running without this member would silently skip its round trip and
                // never restore it on the shared singleton; do not run blind.
                Err(error) if is_binding(&error) => panic!(
                    "cannot snapshot {}: binding-layer failure ({error}); \
                     the harness must not run without this member",
                    row.member
                ),
                Err(error) => println!("note: cannot snapshot {}: {error}", row.member),
            }
        }
        out
    }

    /// Tells `Drop` that the test disconnected the device and it has to reconnect first.
    pub fn mark_disconnected(&self) {
        self.disconnected.set(true);
    }
}

impl<D: AscomDevice + 'static, V: Copy + PartialEq + Debug + 'static> Drop for Restorer<D, V> {
    fn drop(&mut self) {
        let mut problems = Vec::new();
        if self.disconnected.get() && !self.device.connected().unwrap_or(false) {
            if let Err(error) = self.device.set_connected(true) {
                problems.push(format!("reconnect: {error}"));
            }
        }
        // Last written, first restored: a value whose legality depends on another
        // member has to go back after the one it depends on, which is what the order
        // of the table encodes.
        for undo in self.undo.borrow().iter().rev() {
            if let Err(error) = (undo.set)(&self.device, undo.saved) {
                problems.push(format!("restore {}: {error}", undo.member));
                continue;
            }
            match (undo.get)(&self.device) {
                Ok(current) if current != undo.saved => problems.push(format!(
                    "{} reads {current:?}, was {:?}",
                    undo.member, undo.saved
                )),
                Err(error) => problems.push(format!("verify {}: {error}", undo.member)),
                Ok(_) => {}
            }
        }
        if problems.is_empty() {
            return;
        }
        let text = format!("could not restore {}:\n{}", self.driver, problems.join("\n"));
        if std::thread::panicking() {
            eprintln!("RESTORE FAILED (a panic is already unwinding): {text}");
        } else {
            panic!("{text}");
        }
    }
}

/// read → write a different value → wait until the driver reports it → read back, for
/// every row the driver answers.
pub fn writable_round_trip<
    D: AscomDevice + 'static,
    V: Copy + PartialEq + Debug + 'static,
>(
    guard: &Restorer<D, V>,
) {
    let device = guard.device();
    let caps = device.capabilities().expect("capabilities");
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    for (row, before) in guard.snapshot() {
        let wanted = (row.other)(device, before);

        if let Some(gate) = row.gate {
            match caps.flag(gate) {
                // Gate false: the spec wants the write refused as not implemented.
                Some(false) => match (row.set)(device, wanted) {
                    Ok(()) => {
                        failures.push(format!("{}: write accepted although {gate} is false", row.member))
                    }
                    Err(error) if error.is_unsupported() => {
                        println!("note: {} is not writable ({gate} false)", row.member);
                    }
                    // A raw managed exception is the driver's doing, not the
                    // binding layer's (see `is_raw_managed`).
                    Err(error) if is_binding(&error) && !is_raw_managed(&error) => {
                        failures.push(format!(
                            "{}: write failed in the binding layer: {error}",
                            row.member
                        ))
                    }
                    Err(error) => deviations.push(format!(
                        "{}: {gate} is false, spec wants PropertyNotImplemented, driver answered {:?} ({error})",
                        row.member, error.kind
                    )),
                },
                Some(true) => {}
                None => println!("note: {} does not report {gate}; writing anyway", row.member),
            }
        }

        if !(row.precondition)(device).unwrap_or(false) {
            println!("note: {} skipped, its precondition is not met right now", row.member);
            continue;
        }

        if let Err(error) = (row.set)(device, wanted) {
            if is_binding(&error) && !is_raw_managed(&error) {
                failures.push(format!("{}: writing {wanted:?}: {error}", row.member));
                continue;
            }
            deviations.push(format!(
                "{} refused {wanted:?} although the gate and state allow it: {error}",
                row.member
            ));
            continue;
        }

        let deadline = Instant::now() + APPLY_TIMEOUT;
        let mut now = (row.get)(device).ok();
        while !now.is_some_and(|value| value == wanted) && Instant::now() < deadline {
            std::thread::sleep(POLL);
            now = (row.get)(device).ok();
        }
        if !now.is_some_and(|value| value == wanted) {
            deviations.push(format!(
                "{} still reads {now:?} {APPLY_TIMEOUT:?} after writing {wanted:?}",
                row.member
            ));
        }
    }

    report(&failures, &deviations);
    // The guard restores and verifies everything on the way out.
}

/// A value the spec rejects must be refused, and the refusal must not change anything.
pub fn refuses_out_of_range_values<D: AscomDevice + 'static, V: Copy + PartialEq + Debug + 'static>(
    guard: &Restorer<D, V>,
) {
    let device = guard.device();
    let mut failures = Vec::new();
    let mut deviations = Vec::new();

    for (row, before) in guard.snapshot() {
        let Some(bad_value) = row.bad else {
            println!("note: {} has no spec range to test", row.member);
            continue;
        };
        let bad = bad_value(device);
        let refused = match (row.set)(device, bad) {
            Ok(()) => {
                deviations.push(format!("{} accepted the out-of-range {bad:?}", row.member));
                false
            }
            // A wrapper that guards the value itself answers here rather than the
            // driver; the error text carries which layer raised it. A raw managed
            // exception is the driver's fault, so it falls through to the deviation
            // arm below.
            Err(error) if is_binding(&error) && !is_raw_managed(&error) => {
                failures.push(format!(
                    "{}: {bad:?} failed in the binding layer: {error}",
                    row.member
                ));
                true
            }
            Err(error) => {
                if error.kind != AscomErrorKind::InvalidValue {
                    deviations.push(format!(
                        "{}: spec wants InvalidValue for {bad:?}, driver answered {:?} ({error})",
                        row.member, error.kind
                    ));
                }
                true
            }
        };
        // Only a refused write promises to leave the value alone.
        if refused {
            match (row.get)(device) {
                Ok(after) if after != before => failures.push(format!(
                    "{}: a refused write changed {before:?} to {after:?}",
                    row.member
                )),
                Err(error) => failures.push(format!(
                    "{}: unreadable after a refused write: {error}",
                    row.member
                )),
                Ok(_) => {}
            }
        }
    }

    report(&failures, &deviations);
}

/// Every write issued while disconnected must raise `NotConnected`. Where a wrapper has
/// to read a limit first (`MaxBinX`, `CameraXSize`), the refusal may come from that
/// read — the driver refuses those reads the same way.
pub fn refuses_writes_while_disconnected<D: AscomDevice + 'static, V: Copy + PartialEq + Debug + 'static>(
    guard: &Restorer<D, V>,
) {
    let device = guard.device();
    let caps = device.capabilities().expect("capabilities");
    // Values have to be read while still connected: disconnected reads raise NotConnected.
    let snapshot = guard.snapshot();
    guard.mark_disconnected();
    device.set_connected(false).expect("disconnect");
    println!("note: Connected reads {:?} after disconnecting", device.connected().ok());

    let mut failures = Vec::new();
    let mut deviations = Vec::new();
    let mut accepted = Vec::new();
    for (row, before) in &snapshot {
        // A false gate is already a "cannot write", so there is nothing to prove there.
        if row.gate.is_some_and(|gate| caps.flag(gate) == Some(false)) {
            continue;
        }
        let wanted = (row.other)(device, *before);
        match (row.set)(device, wanted) {
            Ok(()) => accepted.push(row.member),
            Err(error) if is_binding(&error) && !is_raw_managed(&error) => failures.push(format!(
                "{}: write while disconnected failed in the binding layer: {error}",
                row.member
            )),
            Err(error) if error.kind != AscomErrorKind::NotConnected => deviations.push(format!(
                "{}: spec wants NotConnected while disconnected, driver answered {:?} ({error})",
                row.member, error.kind
            )),
            Err(_) => {}
        }
    }
    // One line per finding, not one per member: the interesting part is the set.
    if !accepted.is_empty() {
        deviations.push(format!(
            "these accepted a write while disconnected, spec wants NotConnected: {}",
            accepted.join(", ")
        ));
    }

    report(&failures, &deviations);
    // Reconnecting and restoring the values is the guard's job.
}
