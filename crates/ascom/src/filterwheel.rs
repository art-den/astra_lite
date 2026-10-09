//! `IFilterWheelV3`.
//!
//! The smallest interface in the standard: two arrays that describe the wheel and one
//! writable integer that moves it. Two behaviours catch people out:
//! * `Position` is the motion command *and* its completion property. Writing it starts
//!   the rotation and returns; reading it answers [`MOVING`] (`-1`) until the wheel
//!   stops, and a legal slot number must never be reported while the wheel turns — so
//!   completion is an integer predicate rather than a `IsMoving`-style flag.
//! * the interface declares no `CanXxx` member at all, so
//!   [`AscomDevice::capabilities`] reports an
//!   empty flag map by design rather than by omission.
//!
//! A wheel is not cancellable: nothing in the interface stops a change in flight, and a
//! second write while one is running is answered differently by every driver (the
//! OmniSim wheel raises a raw .NET error and moves to the refused slot anyway). Prefer
//! [`FilterWheel::move_to_and_wait`] over a bare [`FilterWheel::set_position`].

use crate::actor::Actor;
use crate::com::collections;
use crate::com::Dispatch;
use crate::device::{AscomDevice, DeviceSpec, StateValue};
use crate::error::{AscomError, AscomErrorKind, Result};
use crate::wait::{WaitSpec, wait_i32};

/// Capability members captured in [`AscomDevice::capabilities`].
///
/// Empty because `IFilterWheelV3` defines no `CanXxx` member: every wheel offers the
/// same surface, and what varies is the number of slots, which is data rather than a
/// capability.
pub const FLAG_MEMBERS: &[&str] = &[];

/// What `Position` answers while the wheel is turning.
///
/// Mandatory in the spec, with one documented exception: a wheel built into a camera may
/// answer the written slot immediately and never report this value.
pub const MOVING: i32 = -1;

/// A filter wheel living on its own STA COM thread. `Send + Clone`.
#[derive(Clone)]
pub struct FilterWheel {
    actor: Actor,
    spec: DeviceSpec,
}

impl FilterWheel {
    /// Instantiates the driver named by `spec`.
    ///
    /// Nothing is connected yet: the driver object exists, `Connected` is whatever the
    /// driver defaults to (in practice `false`).
    pub fn open(spec: &DeviceSpec) -> Result<Self> {
        Ok(Self { actor: Actor::spawn(&spec.prog_id)?, spec: spec.clone() })
    }

    pub fn prog_id(&self) -> &str {
        &self.spec.prog_id
    }

    // The wheel, as the driver describes it -----------------------------------

    /// `Names`: the name of every slot, indexed by slot number.
    ///
    /// Its length is the number of slots, which is also the bound on [`Self::position`].
    /// A driver with no names is required to answer `"Filter 1"`, `"Filter 2"`, …
    pub fn names(&self) -> Result<Vec<String>> {
        self.actor.call(|device| read_names(device.dispatch()))
    }

    /// `FocusOffsets`: focuser offset of every slot, indexed by slot number.
    ///
    /// The spec wants at least one offset to be zero (the reference the others are
    /// measured against) and all zeros when the wheel has no offsets to offer; whether
    /// either holds is the driver's business, so this reports what it says.
    pub fn focus_offsets(&self) -> Result<Vec<i32>> {
        self.actor.call(|device| {
            let value = match device.dispatch().try_get("FocusOffsets")? {
                Some(v) => v,
                None => return Ok(Vec::new()),
            };
            collections::ints(&value)
        })
    }

    // Position ----------------------------------------------------------------

    /// `Position` exactly as the driver answers it: a slot number while the wheel stands
    /// still, [`MOVING`] while it turns.
    pub fn position(&self) -> Result<i32> {
        self.actor.call(|device| device.dispatch().get_i32("Position"))
    }

    /// The slot the wheel stands at, or `None` while it turns.
    pub fn filter(&self) -> Result<Option<i32>> {
        self.position().map(slot_of)
    }

    /// True while a change is in flight.
    pub fn is_moving(&self) -> Result<bool> {
        self.position().map(|position| position == MOVING)
    }

    /// Writes `Position`, which starts the rotation. Non-blocking: it returns as soon as
    /// the driver accepted the change.
    ///
    /// The bound comes from `Names`, and is checked here first so that nonsense from a
    /// caller cannot reach the hardware. It is skipped only when the wheel documents no
    /// slots at all — `Names` unimplemented, or an empty answer — because there is then
    /// nothing to check against and the driver judges the value itself. Any other
    /// failure to read `Names` is an error and aborts the write rather than silently
    /// removing the bound.
    pub fn set_position(&self, slot: i32) -> Result<()> {
        self.actor.call(move |device| {
            check_position(device.dispatch(), slot)?;
            device.dispatch().set_i32("Position", slot)
        })
    }

    /// Waits for the wheel to stop, returning the slot it reports.
    ///
    /// `Position` is the completion property here, so this waits for anything but
    /// [`MOVING`] rather than for a flag.
    pub fn wait_until_stopped(&self, spec: WaitSpec) -> Result<i32> {
        wait_i32(&self.actor, "Position", spec, |position| *position != MOVING)
    }

    /// [`Self::set_position`] followed by [`Self::wait_until_stopped`].
    ///
    /// It returns the slot the wheel reports when it stops rather than `()`: the spec
    /// promises that number equals `slot`, but a wheel that went elsewhere is a fact
    /// about the device that the caller must be able to see, not something this wrapper
    /// should turn into an error of its own making.
    pub fn move_to_and_wait(&self, slot: i32, spec: WaitSpec) -> Result<i32> {
        self.set_position(slot)?;
        self.wait_until_stopped(spec)
    }

    /// Aggregated driver state; empty when the driver exposes none.
    pub fn state(&self) -> Result<Vec<StateValue>> {
        self.device_state()
    }
}

/// Reads `Names` off a dispatch, mapping "the driver does not implement the member"
/// to an empty list.
fn read_names(dispatch: &Dispatch) -> Result<Vec<String>> {
    let Some(value) = dispatch.try_get("Names")? else {
        return Ok(Vec::new());
    };
    collections::strings(&value)
}

/// How many slots the driver describes; `0` means it describes none, which
/// [`check_slot`] reads as "no bound known".
///
/// Only the absence of the member becomes an empty list: every other failure to read
/// `Names` propagates, because a failed read is not evidence that the wheel has no
/// slots.
fn slot_bound(dispatch: &Dispatch) -> Result<usize> {
    Ok(read_names(dispatch)?.len())
}

/// Judges `slot` against the wheel's own bound before the write.
///
/// A wheel that documents no slots leaves the value to the driver. A `Names` read that
/// fails for any other reason aborts the write: swallowing `Disconnected`, `Com` or
/// `ValueNotSet` into "no bound known" would drop the client-side bound silently and
/// hand an unvalidated slot to the hardware.
fn check_position(dispatch: &Dispatch, slot: i32) -> Result<()> {
    match slot_bound(dispatch) {
        Ok(count) => check_slot(count, slot),
        Err(err) if err.is_unsupported() => Ok(()),
        Err(err) => Err(err),
    }
}

/// Turns a raw `Position` reading into a slot number.
pub fn slot_of(position: i32) -> Option<i32> {
    if position == MOVING { None } else { Some(position) }
}

/// Client-side bound for `Position`: valid slots are `0..slot_count`.
///
/// Returns `InvalidValue` in the shape a driver would use, so a caller cannot tell a
/// value rejected here from one the driver rejected, except in `source`.
///
/// An empty list means the driver described no slots, which leaves the wrapper with no
/// bound to check against; the value is passed through for the driver to judge.
///
/// A count above `i32::MAX` is more slots than a slot number can address, so the bound
/// saturates at `i32::MAX` and every value a caller can pass is legal; a truncated one
/// would wrap and reject everything.
pub fn check_slot(slot_count: usize, slot: i32) -> Result<()> {
    if slot_count == 0 {
        return Ok(());
    }
    let last = match i32::try_from(slot_count) {
        Ok(count) => count - 1,
        Err(_) => i32::MAX,
    };
    if slot < 0 || slot > last {
        return Err(AscomError::local(
            AscomErrorKind::InvalidValue,
            "Position",
            format!("filter wheel has {slot_count} slots: {slot} is outside [0, {last}]"),
        )
        .with_source("ascom::filterwheel"));
    }
    Ok(())
}

impl AscomDevice for FilterWheel {
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
    use crate::com::mock::{Element, Member, MockDevice};
    use crate::com::variant::Variant;
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{DISP_E_MEMBERNOTFOUND, E_FAIL, E_NOTIMPL, RPC_E_DISCONNECTED};

    /// An ASCOM exception HRESULT, e.g. code `0x402` = ValueNotSet.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    /// A driver object answering exactly the configured members; a name it does not
    /// carry is refused with `DISP_E_UNKNOWNNAME`, i.e. classified as "the driver does
    /// not implement the member". The returned `Variant` keeps the mock alive.
    fn mock_dispatch(members: Vec<(&'static str, Member)>) -> (Variant, Dispatch) {
        let variant = MockDevice::new(members).into_variant();
        let dispatch = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        (variant, dispatch)
    }

    #[test]
    fn slots_run_from_zero_to_one_below_the_name_count() {
        assert!(check_slot(6, 0).is_ok());
        assert!(check_slot(6, 5).is_ok());

        let above = check_slot(6, 6).unwrap_err();
        assert_eq!(above.kind, AscomErrorKind::InvalidValue);
        assert_eq!(above.member, "Position");
        assert!(above.message.contains("[0, 5]"), "{}", above.message);
        // The count itself is not a slot, whatever the driver's own message says.
        assert!(!above.message.contains("6]"));

        let below = check_slot(6, -1).unwrap_err();
        assert_eq!(below.kind, AscomErrorKind::InvalidValue);
    }

    #[test]
    fn a_driver_that_names_no_slots_judges_the_value_itself() {
        assert!(check_slot(0, 0).is_ok());
        assert!(check_slot(0, 12).is_ok(), "no bound is known, so none is enforced");
    }

    #[test]
    fn minus_one_is_motion_and_no_slot() {
        assert_eq!(MOVING, -1);
        assert_eq!(slot_of(MOVING), None);
        assert_eq!(slot_of(0), Some(0));
        assert_eq!(slot_of(5), Some(5));
    }

    #[test]
    fn the_interface_declares_no_capability_members() {
        assert!(FLAG_MEMBERS.is_empty(), "IFilterWheelV3 has no CanXxx member");
    }

    #[test]
    fn the_error_names_the_wrapper_as_its_source() {
        let error = check_slot(4, 9).unwrap_err();
        assert_eq!(error.source, "ascom::filterwheel");
    }

    /// `slot_count` is `usize`, the function is `pub`, and a count above `i32::MAX`
    /// is not a slot number: truncated it became a negative bound (rejecting every
    /// slot and advertising it in the message), and `i32::MIN - 1` panicked outright
    /// in a debug build.
    #[test]
    fn a_slot_count_no_i32_can_reach_keeps_every_slot_legal() {
        assert!(check_slot(1 << 31, 0).is_ok());
        assert!(check_slot(1 << 31, i32::MAX).is_ok());
        assert!(check_slot(1 << 32, 99).is_ok());
        assert!(check_slot(usize::MAX, i32::MAX).is_ok());

        // A rejection is still possible (a negative slot), and the range it prints
        // must be the one actually enforced, not a wrapped-around `-1`.
        let error = check_slot(1 << 32, -1).unwrap_err();
        assert_eq!(error.kind, AscomErrorKind::InvalidValue);
        assert!(error.message.contains("[0, 2147483647]"), "{}", error.message);
        assert!(!error.message.contains("[0, -1]"), "lying bound: {}", error.message);
    }

    /// The bound the driver states is enforced, so a slot that cannot exist never
    /// reaches the hardware.
    #[test]
    fn the_bound_the_driver_states_is_enforced() {
        let (_keep, dispatch) = mock_dispatch(vec![("Names", Member::Value(Element::Str("Lum")))]);
        assert_eq!(slot_bound(&dispatch).expect("one name is one slot"), 1);
        assert!(check_position(&dispatch, 0).is_ok());

        let error = check_position(&dispatch, 1).expect_err("a single slot has no slot 1");
        assert_eq!(error.kind, AscomErrorKind::InvalidValue);
        assert_eq!(error.member, "Position");
    }

    /// Every shape of "this driver does not describe its slots" leaves the wrapper
    /// with no bound, so the value goes to the driver, which is allowed to refuse it.
    #[test]
    fn only_an_undocumented_wheel_leaves_the_value_to_the_driver() {
        // No `Names` member in the interface at all.
        let (_keep, dispatch) = mock_dispatch(vec![("Position", Member::Value(Element::Int(0)))]);
        assert_eq!(slot_bound(&dispatch).expect("absence is not a failure"), 0);
        assert!(check_position(&dispatch, 99).is_ok());

        for scode in [ascom_hr(0x400), E_NOTIMPL, DISP_E_MEMBERNOTFOUND] {
            let (_keep, dispatch) = mock_dispatch(vec![("Names", Member::Refuses(scode))]);
            assert_eq!(
                slot_bound(&dispatch).expect("absence is not a failure"),
                0,
                "hr 0x{:08X} is not absence",
                scode.0 as u32
            );
            assert!(check_position(&dispatch, 99).is_ok());
        }
    }

    /// The other half of the same rule: a `Names` read that fails for a reason other
    /// than absence must abort the write. Dropping it into "no bound known" silently
    /// removes the client-side bound and lets an unvalidated slot reach the hardware.
    #[test]
    fn a_names_read_failure_is_not_an_undocumented_wheel() {
        for (scode, kind) in [
            // "no value" is an error, never a missing bound.
            (ascom_hr(0x402), AscomErrorKind::ValueNotSet),
            (ascom_hr(0x407), AscomErrorKind::NotConnected),
            // Binding-layer failures are not the driver's silence either.
            (RPC_E_DISCONNECTED, AscomErrorKind::Disconnected),
            (E_FAIL, AscomErrorKind::Com),
        ] {
            let (_keep, dispatch) = mock_dispatch(vec![("Names", Member::Refuses(scode))]);
            let error = check_position(&dispatch, 1)
                .expect_err("a failed read must not become an unchecked write");
            assert_eq!(error.kind, kind, "misclassified through Names: {error}");
        }
    }
}
