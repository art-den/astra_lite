//! `IFocuserV4`.
//!
//! The smallest of the three interfaces, which is why it is the one the framework
//! is debugged against first.
//!
//! Two behaviours that catch people out:
//! * [`Focuser::move_to`] is asynchronous — it returns as soon as motion has
//!   *started*; completion is `IsMoving == false`.
//! * the meaning of the `Position` argument depends on `Absolute`: an absolute
//!   focuser takes a target in `[0, MaxStep]`, a relative one a delta in
//!   `[-MaxIncrement, +MaxIncrement]`.

use std::sync::{Arc, Mutex};

use crate::actor::Actor;
use crate::com::variant::Variant;
use crate::device::{AscomDevice, DeviceSpec, StateValue};
use crate::error::{AscomError, AscomErrorKind, Result};
use crate::wait::{WaitSpec, wait_flag_false};

/// Capability members captured in [`AscomDevice::capabilities`].
///
/// `Link` is COM-only and means "the device is reachable", which is a capability of
/// the connection rather than a `CanXxx`, but it belongs in the same snapshot.
pub const FLAG_MEMBERS: &[&str] = &["TempCompAvailable", "Link"];

/// A focuser living on its own STA COM thread. `Send + Clone`.
#[derive(Clone)]
pub struct Focuser {
    actor: Actor,
    spec: DeviceSpec,
    /// Outcome of the recommended one-off `Halt()` probe: `None` until probed.
    halt: Arc<Mutex<Option<bool>>>,
}

impl Focuser {
    /// Instantiates the driver named by `spec`.
    ///
    /// Nothing is connected yet: the driver object exists, `Connected` is whatever
    /// the driver defaults to (in practice `false`).
    pub fn open(spec: &DeviceSpec) -> Result<Self> {
        Ok(Self {
            actor: Actor::spawn(&spec.prog_id)?,
            spec: spec.clone(),
            halt: Arc::new(Mutex::new(None)),
        })
    }

    /// Builds the focuser around an in-process mock driver answering `members`.
    /// Test seam only (`feature = "mock"`); it replaces instantiation alone.
    #[cfg(feature = "mock")]
    pub fn open_mock(
        members: Vec<(&'static str, crate::com::mock::Member)>,
        prog_id: &str,
    ) -> Result<Self> {
        Ok(Self {
            actor: Actor::spawn_mock(members)?,
            spec: DeviceSpec::new(prog_id),
            halt: Arc::new(Mutex::new(None)),
        })
    }

    /// `Some(true)` once [`Focuser::probe_halt`] has shown that `Halt()` works,
    /// `Some(false)` when the driver rejected it, `None` when not probed yet.
    ///
    /// Probing is opt-in on purpose: the spec recommends calling `Halt()` at
    /// initialisation to decide whether to offer a stop button, but calling it
    /// unconditionally would stop a focuser that is legitimately moving.
    pub fn halt_supported(&self) -> Option<bool> {
        *self.halt.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Calls `Halt()` once and records whether the driver implements it.
    ///
    /// `MethodNotImplementedException` is the "no" answer; anything else (including
    /// `NotConnected`) leaves the probe inconclusive so a later attempt can retry.
    pub fn probe_halt(&self) -> Result<bool> {
        let outcome = self.actor.call(|device| device.dispatch().call_void("Halt", &[]));
        match outcome {
            Ok(()) => {
                self.record_halt(true);
                Ok(true)
            }
            // MethodNotImplementedException is the driver's "no".
            Err(err) if err.kind == AscomErrorKind::Unsupported => {
                self.record_halt(false);
                Ok(false)
            }
            // Anything else (e.g. NotConnected) leaves the question open.
            Err(err) => Err(err),
        }
    }

    fn record_halt(&self, supported: bool) {
        if let Ok(mut slot) = self.halt.lock() {
            *slot = Some(supported);
        }
    }

    pub fn prog_id(&self) -> &str {
        &self.spec.prog_id
    }

    // Properties -------------------------------------------------------------

    /// Positions are absolute targets when this is `true`, deltas when `false`.
    pub fn absolute(&self) -> Result<bool> {
        self.actor.call(|device| device.dispatch().get_bool("Absolute"))
    }

    pub fn is_moving(&self) -> Result<bool> {
        self.actor.call(|device| device.dispatch().get_bool("IsMoving"))
    }

    pub fn position(&self) -> Result<i32> {
        self.actor.call(|device| device.dispatch().get_i32("Position"))
    }

    pub fn max_increment(&self) -> Result<i32> {
        self.actor.call(|device| device.dispatch().get_i32("MaxIncrement"))
    }

    pub fn max_step(&self) -> Result<i32> {
        self.actor.call(|device| device.dispatch().get_i32("MaxStep"))
    }

    /// Size of one step in the focuser's own units; `0` means undocumented.
    pub fn step_size(&self) -> Result<f64> {
        self.actor.call(|device| device.dispatch().get_f64("StepSize"))
    }

    /// Ambient temperature at the focuser, in **degrees Celsius**.
    ///
    /// A driver with no readable sensor is supposed to raise
    /// `PropertyNotImplementedException`, but some answer `-1` for "no value".
    /// That sentinel is passed through unfiltered — a real −1 °C is physically
    /// possible — so callers cannot tell the two apart from the number alone.
    /// Pre-2019 drivers may report the value in units other than Celsius.
    pub fn temperature(&self) -> Result<f64> {
        self.actor.call(|device| device.dispatch().get_f64("Temperature"))
    }

    pub fn temp_comp(&self) -> Result<bool> {
        self.actor.call(|device| device.dispatch().get_bool("TempComp"))
    }

    pub fn set_temp_comp(&self, enabled: bool) -> Result<()> {
        self.actor.call(move |device| device.dispatch().set_bool("TempComp", enabled))
    }

    /// COM-only: `false` means the device is unreachable. That is *not* a connection
    /// error, so it must not be reported as one.
    pub fn link(&self) -> Result<bool> {
        self.actor.call(|device| device.dispatch().get_bool("Link"))
    }

    // Motion -----------------------------------------------------------------

    /// Starts a move, returning as soon as the driver has accepted it.
    ///
    /// The range check happens here, before the driver sees the value, so that
    /// nonsense from a caller cannot reach the hardware. A real
    /// `InvalidValueException` from the driver is still passed through unchanged.
    pub fn move_to(&self, position: i32) -> Result<()> {
        let snapshot = self.range_facts()?;
        check_move_target(&snapshot, position)?;
        self.actor.call(move |device| {
            let target = Variant::from_i32(position);
            device.dispatch().call_void("Move", &[&target])
        })
    }

    /// [`Focuser::move_to`] followed by waiting for `IsMoving == false`.
    pub fn move_to_and_wait(&self, position: i32, spec: WaitSpec) -> Result<()> {
        self.move_to(position)?;
        wait_flag_false(&self.actor, "IsMoving", spec)
    }

    /// Stops motion immediately, if the driver implements `Halt()`.
    pub fn halt(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("Halt", &[]))
    }

    /// `Absolute`, `MaxStep` and `MaxIncrement` as the driver reports them now.
    ///
    /// Read directly rather than from the capability cache: these are mandatory
    /// members, not `CanXxx`, and a driver may reconfigure its limits while connected.
    fn range_facts(&self) -> Result<MoveLimits> {
        Ok(MoveLimits {
            absolute: self.absolute()?,
            max_step: self.max_step()?,
            max_increment: self.max_increment()?,
        })
    }

    /// Aggregated driver state; empty when the driver exposes none.
    pub fn state(&self) -> Result<Vec<StateValue>> {
        self.device_state()
    }
}

/// Everything needed to decide whether a `Move` argument is sane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveLimits {
    pub absolute: bool,
    pub max_step: i32,
    pub max_increment: i32,
}

/// Client-side range check for `Move(Position)`.
///
/// Returns `InvalidValue` in the same shape a driver would, so callers can not tell
/// a rejected-by-us value from a rejected-by-driver one except in `source`.
pub fn check_move_target(limits: &MoveLimits, position: i32) -> Result<()> {
    if limits.absolute {
        if position < 0 || position > limits.max_step {
            return Err(AscomError::local(
                AscomErrorKind::InvalidValue,
                "Move",
                format!(
                    "absolute focuser: {position} is outside [0, {}] (MaxStep)",
                    limits.max_step
                ),
            )
            .with_source("ascom::focuser"));
        }
        return Ok(());
    }
    // Compared in i64: MaxIncrement is driver data, and negating an i32::MIN
    // overflows (panic in debug; in release it wraps back to i32::MIN, turning the
    // bound into one that accepts nearly every delta). A negative limit is nonsense,
    // so it accepts nothing.
    let delta = i64::from(position);
    let max = i64::from(limits.max_increment);
    if max < 0 || delta < -max || delta > max {
        return Err(AscomError::local(
            AscomErrorKind::InvalidValue,
            "Move",
            format!(
                "relative focuser: {delta} is outside [{}, {max}] (MaxIncrement)",
                -max
            ),
        )
        .with_source("ascom::focuser"));
    }
    Ok(())
}

impl AscomDevice for Focuser {
    fn actor(&self) -> &Actor {
        &self.actor
    }

    fn flag_members() -> &'static [&'static str] {
        FLAG_MEMBERS
    }

    fn spec(&self) -> DeviceSpec {
        self.spec.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(absolute: bool, max_step: i32, max_increment: i32) -> MoveLimits {
        MoveLimits { absolute, max_step, max_increment }
    }

    #[test]
    fn absolute_focuser_accepts_only_a_target_in_range() {
        let limits = limits(true, 1000, 100);
        assert!(check_move_target(&limits, 0).is_ok());
        assert!(check_move_target(&limits, 1000).is_ok());

        let below = check_move_target(&limits, -1).unwrap_err();
        assert_eq!(below.kind, AscomErrorKind::InvalidValue);
        assert!(below.message.contains("MaxStep"));

        let above = check_move_target(&limits, 1001).unwrap_err();
        assert_eq!(above.kind, AscomErrorKind::InvalidValue);
        // MaxIncrement must not be used as the bound for an absolute focuser.
        assert!(above.message.contains("[0, 1000]"));
    }

    #[test]
    fn relative_focuser_accepts_only_a_delta_in_range() {
        let limits = limits(false, 1000, 100);
        assert!(check_move_target(&limits, -100).is_ok());
        assert!(check_move_target(&limits, 100).is_ok());
        assert!(check_move_target(&limits, 0).is_ok());

        let above = check_move_target(&limits, 101).unwrap_err();
        assert_eq!(above.kind, AscomErrorKind::InvalidValue);
        assert!(above.message.contains("[-100, 100]"));
        assert_eq!(check_move_target(&limits, -101).unwrap_err().kind, AscomErrorKind::InvalidValue);
        // MaxStep must not be used as the bound for a relative focuser.
        assert!(!above.message.contains("1000"));
    }

    #[test]
    fn a_degenerate_focuser_accepts_only_zero() {
        let limits = limits(false, 0, 0);
        assert!(check_move_target(&limits, 0).is_ok());
        assert!(check_move_target(&limits, 1).is_err());
    }

    #[test]
    fn a_hostile_max_increment_is_not_negated_in_i32() {
        // MaxIncrement comes straight from the driver and narrow_i32 admits i32::MIN,
        // whose negation overflows: panic in debug, wrap in release.
        let limits = limits(false, 50_000, i32::MIN);
        for position in [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX] {
            let err = check_move_target(&limits, position)
                .expect_err("a nonsensical MaxIncrement must accept nothing");
            assert_eq!(err.kind, AscomErrorKind::InvalidValue);
        }
    }

    #[test]
    fn a_negative_max_increment_accepts_nothing() {
        let limits = limits(false, 50_000, -5);
        assert!(check_move_target(&limits, 0).is_err());
        assert!(check_move_target(&limits, 5).is_err());
        assert!(check_move_target(&limits, -5).is_err());
    }
}
