//! `ITelescopeV4`.
//!
//! The "async" members of this interface only *initiate*; the outcome arrives
//! through the `Slewing`/`AtHome`/`AtPark` completion properties. Each blocking
//! wrapper here follows one pattern: initiate, then
//! [`crate::wait::wait_flag_false`] on the completion property, so a timeout is
//! reported as [`crate::error::AscomErrorKind::Timeout`]
//! rather than as a driver failure.
//!
//! `GuideDirection` lives in [`crate::device`] because `Camera.PulseGuide` shares it.

use crate::actor::Actor;
use crate::com::collections;
use crate::com::variant::{AscomEnum, Variant};
use crate::com::Dispatch;
use crate::device::{AscomDevice, DeviceSpec, GuideDirection, StateValue};
use crate::error::{AscomError, AscomErrorKind, Result};
use crate::wait::{WaitSpec, wait_flag_false};

/// Capability members captured in [`AscomDevice::capabilities`].
///
/// These are the members a control loop would otherwise re-read on every iteration,
/// and each read is a cross-process RPC.
pub const FLAG_MEMBERS: &[&str] = &[
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
];

/// A mount living on its own STA COM thread. `Send + Clone`.
#[derive(Clone)]
pub struct Telescope {
    actor: Actor,
    spec: DeviceSpec,
}

impl Telescope {
    /// Instantiates the driver named by `spec`; does not connect.
    pub fn open(spec: &DeviceSpec) -> Result<Self> {
        Ok(Self { actor: Actor::spawn(&spec.prog_id)?, spec: spec.clone() })
    }

    pub fn prog_id(&self) -> &str {
        &self.spec.prog_id
    }

    // Slewing ----------------------------------------------------------------

    /// Starts a slew to an equatorial position; returns as soon as it has begun.
    pub fn slew_to_coordinates_async(&self, right_ascension: f64, declination: f64) -> Result<()> {
        self.slew("SlewToCoordinatesAsync", right_ascension, declination)
    }

    /// Slews to an equatorial position and waits for `Slewing == false`.
    ///
    /// If the driver has no asynchronous slew (`CanSlewAsync == false`), the
    /// synchronous member is called on the COM thread instead, which simply blocks
    /// that thread for the duration.
    pub fn slew_to_coordinates(&self, right_ascension: f64, declination: f64, wait: WaitSpec) -> Result<()> {
        if self.capabilities()?.supports("CanSlewAsync") {
            self.slew_to_coordinates_async(right_ascension, declination)?;
        } else {
            self.slew("SlewToCoordinates", right_ascension, declination)?;
        }
        wait_flag_false(&self.actor, "Slewing", wait)
    }

    /// Starts a slew to an alt-az position; returns as soon as it has begun.
    pub fn slew_to_alt_az_async(&self, azimuth: f64, altitude: f64) -> Result<()> {
        self.slew("SlewToAltAzAsync", azimuth, altitude)
    }

    /// Slews to an alt-az position and waits for `Slewing == false`.
    pub fn slew_to_alt_az(&self, azimuth: f64, altitude: f64, wait: WaitSpec) -> Result<()> {
        if self.capabilities()?.supports("CanSlewAltAzAsync") {
            self.slew_to_alt_az_async(azimuth, altitude)?;
        } else {
            self.slew("SlewToAltAz", azimuth, altitude)?;
        }
        wait_flag_false(&self.actor, "Slewing", wait)
    }

    /// One `Invoke` of a two-argument slew member. Arguments are handed over in
    /// declaration order; [`crate::com::dispatch`] reverses them for `rgvarg`.
    fn slew(&self, member: &'static str, first: f64, second: f64) -> Result<()> {
        self.actor.call(move |device| {
            let a = Variant::from_f64(first);
            let b = Variant::from_f64(second);
            device.dispatch().call_void(member, &[&a, &b])
        })
    }

    /// Starts the slew to [`Self::target_right_ascension`]/[`Self::target_declination`].
    pub fn slew_to_target_async(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("SlewToTargetAsync", &[]))
    }

    /// [`Self::slew_to_target_async`] followed by waiting on `Slewing`.
    pub fn slew_to_target(&self, wait: WaitSpec) -> Result<()> {
        if self.capabilities()?.supports("CanSlewAsync") {
            self.slew_to_target_async()?;
        } else {
            self.actor.call(|device| device.dispatch().call_void("SlewToTarget", &[]))?;
        }
        wait_flag_false(&self.actor, "Slewing", wait)
    }

    /// Aborts an in-progress slew.
    pub fn abort_slew(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("AbortSlew", &[]))
    }

    /// Which pier a target would be reached on, without moving anything.
    pub fn destination_side_of_pier(&self, right_ascension: f64, declination: f64) -> Result<PierSide> {
        self.actor.call(move |device| {
            let ra = Variant::from_f64(right_ascension);
            let dec = Variant::from_f64(declination);
            match device.dispatch().call("DestinationSideOfPier", &[&ra, &dec])? {
                Some(v) => v.as_enum::<PierSide>("DestinationSideOfPier"),
                None => Err(AscomError::local(
                    AscomErrorKind::ValueNotSet,
                    "DestinationSideOfPier",
                    "the driver returned no value",
                )),
            }
        })
    }

    // Axis motion ------------------------------------------------------------

    /// Rate ranges [`Self::move_axis`] accepts for a mechanical axis.
    ///
    /// The spec requires this member to exist and to never raise
    /// `MethodNotImplementedException`; a mount without `MoveAxis` returns an empty
    /// list. Rates are always positive — the caller picks the direction.
    ///
    /// An empty `Vec` means the driver answered with an *empty collection*. A driver
    /// that answers with no value at all (`VT_EMPTY`) fails with
    /// [`AscomErrorKind::ValueNotSet`] like [`Self::can_move_axis`] does, and a driver
    /// that raises fails with its own exception class; neither can be mistaken for
    /// "this axis has no rates".
    ///
    /// TODO(v3-fallback): IFocuser/ITelescope V2/V3 reportedly exposed `Rates`
    /// (an `Array[Double]`) instead of `AxisRates`/`CanMoveAxis`. That is **not
    /// confirmed**: the local ASCOM documentation describes only V4 and carries no
    /// historical note about `Rates`, so no fallback is invented here. Check the
    /// ASCOM Platform 6 legacy `ITelescopeV2/V3` documentation before implementing
    /// one.
    pub fn axis_rates(&self, axis: TelescopeAxis) -> Result<Vec<RateRange>> {
        self.actor.call(move |device| read_axis_rates(device.dispatch(), axis))
    }

    /// Whether the mount can be moved about a mechanical axis. Mandatory member.
    pub fn can_move_axis(&self, axis: TelescopeAxis) -> Result<bool> {
        self.actor.call(move |device| {
            let axis_v = Variant::from_enum(axis);
            match device.dispatch().call("CanMoveAxis", &[&axis_v])? {
                Some(v) => v.as_bool(),
                None => Err(AscomError::local(
                    AscomErrorKind::ValueNotSet,
                    "CanMoveAxis",
                    "the driver returned no value",
                )),
            }
        })
    }

    /// Starts continuous motion about an axis at `rate` degrees per second.
    ///
    /// The sign of `rate` is the direction; magnitudes outside
    /// [`Self::axis_rates`] are rejected by the driver.
    pub fn move_axis(&self, axis: TelescopeAxis, rate: f64) -> Result<()> {
        self.actor.call(move |device| {
            let axis_v = Variant::from_enum(axis);
            let rate_v = Variant::from_f64(rate);
            device.dispatch().call_void("MoveAxis", &[&axis_v, &rate_v])
        })
    }

    // Guiding, parking, home -------------------------------------------------

    /// Fires a guide pulse. `duration_ms` is the pulse length in milliseconds.
    ///
    /// Completion is `IsPulseGuiding == false`; most drivers return before the pulse
    /// ends, so waiting on it is optional.
    pub fn pulse_guide(&self, direction: GuideDirection, duration_ms: i32) -> Result<()> {
        self.actor.call(move |device| {
            let dir = Variant::from_enum(direction);
            let dur = Variant::from_i32(duration_ms);
            device.dispatch().call_void("PulseGuide", &[&dir, &dur])
        })
    }

    /// [`Self::pulse_guide`] followed by waiting for `IsPulseGuiding == false`.
    pub fn pulse_guide_and_wait(
        &self,
        direction: GuideDirection,
        duration_ms: i32,
        wait: WaitSpec,
    ) -> Result<()> {
        self.pulse_guide(direction, duration_ms)?;
        wait_flag_false(&self.actor, "IsPulseGuiding", wait)
    }

    /// Starts a homing sequence.
    pub fn find_home_async(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("FindHome", &[]))
    }

    /// [`Self::find_home_async`] followed by waiting for `AtHome == true`.
    pub fn find_home(&self, wait: WaitSpec) -> Result<()> {
        self.find_home_async()?;
        crate::wait::wait_flag_true(&self.actor, "AtHome", wait)
    }

    /// Starts a park sequence.
    pub fn park_async(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("Park", &[]))
    }

    /// [`Self::park_async`] followed by waiting for `AtPark == true`.
    pub fn park(&self, wait: WaitSpec) -> Result<()> {
        self.park_async()?;
        crate::wait::wait_flag_true(&self.actor, "AtPark", wait)
    }

    /// Takes the mount out of the parked state. Unparking a mount that is not parked is
    /// harmless, but some drivers keep the park motion running after this returns, so
    /// `Slewing` — not `AtPark` — is what reports the end of the operation.
    pub fn unpark(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("Unpark", &[]))
    }

    /// Records the current position as the park position. This does not park the mount,
    /// and the spec wants `InvalidOperation` while `Slewing`. Nothing in V4 reads the
    /// recorded position back, so there is no way to restore the previous one.
    pub fn set_park(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("SetPark", &[]))
    }

    // Sync -------------------------------------------------------------------

    pub fn sync_to_coordinates(&self, right_ascension: f64, declination: f64) -> Result<()> {
        self.slew("SyncToCoordinates", right_ascension, declination)
    }

    pub fn sync_to_alt_az(&self, azimuth: f64, altitude: f64) -> Result<()> {
        self.slew("SyncToAltAz", azimuth, altitude)
    }

    pub fn sync_to_target(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("SyncToTarget", &[]))
    }

    // Collection-shaped members ---------------------------------------------

    /// `TrackingRates`: the rates the mount can track at.
    ///
    /// Arrives either as a SAFEARRAY of integers or as a dispatch collection.
    ///
    /// Every element must be a [`DriveRate`] variant: an unknown code fails the read
    /// with the same type error [`crate::com::variant::Variant::as_enum`] reports for an
    /// enum member, because dropping it would leave a shorter list that looks complete.
    /// An empty `Vec` means the driver does not implement the member; a member that
    /// answers with no value at all fails with [`AscomErrorKind::ValueNotSet`].
    pub fn tracking_rates(&self) -> Result<Vec<DriveRate>> {
        self.actor.call(|device| read_tracking_rates(device.dispatch()))
    }

    /// Aggregated driver state (`DeviceState`), empty for V2/V3 drivers.
    pub fn state(&self) -> Result<Vec<StateValue>> {
        self.device_state()
    }
}

/// One rate range from [`Telescope::axis_rates`].
///
/// `minimum == maximum` means a single discrete rate, which the spec allows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateRange {
    pub minimum: f64,
    pub maximum: f64,
}

impl RateRange {
    pub fn contains(&self, rate: f64) -> bool {
        let magnitude = f64::abs(rate);
        magnitude >= self.minimum && magnitude <= self.maximum
    }
}

/// Generates a read-only scalar property accessor.
macro_rules! read_property {
    ($method:ident, $member:literal, $ty:ty, $getter:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $method(&self) -> Result<$ty> {
            self.actor.call(|device| device.dispatch().$getter($member))
        }
    };
}

/// Generates a read/write scalar property pair, so the pair cannot drift apart.
macro_rules! scalar_pair {
    ($get:ident, $set:ident, $member:literal, $ty:ty, $getter:ident, $setter:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $get(&self) -> Result<$ty> {
            self.actor.call(|device| device.dispatch().$getter($member))
        }

        #[doc = concat!("Sets [`Self::", stringify!($get), "].")]
        pub fn $set(&self, value: $ty) -> Result<()> {
            self.actor.call(move |device| device.dispatch().$setter($member, value))
        }
    };
}

/// Generates a read-only enum property accessor.
macro_rules! read_enum {
    ($method:ident, $member:literal, $enum:ty, $doc:literal) => {
        #[doc = $doc]
        pub fn $method(&self) -> Result<$enum> {
            self.actor.call(|device| device.dispatch().get($member)?.as_enum::<$enum>($member))
        }
    };
}

/// Generates a read/write enum property pair.
macro_rules! enum_pair {
    ($get:ident, $set:ident, $member:literal, $enum:ty, $doc:literal) => {
        #[doc = $doc]
        pub fn $get(&self) -> Result<$enum> {
            self.actor.call(|device| device.dispatch().get($member)?.as_enum::<$enum>($member))
        }

        #[doc = concat!("Sets [`Self::", stringify!($get), "].")]
        pub fn $set(&self, value: $enum) -> Result<()> {
            self.actor.call(move |device| {
                let raw = Variant::from_enum(value);
                device.dispatch().put($member, &raw)
            })
        }
    };
}

impl Telescope {
    // Mount geometry and status (read-only) ----------------------------------

    read_property!(altitude, "Altitude", f64, get_f64, "Altitude of the optical axis (degrees, refracted).");
    read_property!(azimuth, "Azimuth", f64, get_f64, "Azimuth of the optical axis (degrees, east of north).");
    read_property!(aperture_area, "ApertureArea", f64, get_f64, "Effective collecting area (square metres).");
    read_property!(aperture_diameter, "ApertureDiameter", f64, get_f64, "Diameter of the objective (metres).");
    read_property!(focal_length, "FocalLength", f64, get_f64, "Effective focal length (millimetres).");
    read_property!(sidereal_time, "SiderealTime", f64, get_f64, "Local apparent sidereal time (hours).");
    read_property!(right_ascension, "RightAscension", f64, get_f64, "Current right ascension of the optical axis (hours).");
    read_property!(declination, "Declination", f64, get_f64, "Current declination of the optical axis (degrees).");
    read_property!(at_home, "AtHome", bool, get_bool, "The mount is at its home position.");
    read_property!(at_park, "AtPark", bool, get_bool, "The mount is parked.");
    read_property!(slewing, "Slewing", bool, get_bool, "The mount is currently moving.");
    read_property!(is_pulse_guiding, "IsPulseGuiding", bool, get_bool, "A guide pulse is in progress.");

    read_enum!(alignment_mode, "AlignmentMode", AlignmentMode, "How the mount is aligned.");
    read_enum!(
        equatorial_system,
        "EquatorialSystem",
        EquatorialCoordinateType,
        "Equatorial coordinate frame the mount reports right ascension and declination in."
    );

    enum_pair!(tracking_rate, set_tracking_rate, "TrackingRate", DriveRate, "Current tracking rate.");
    enum_pair!(side_of_pier, set_side_of_pier, "SideOfPier", PierSide, "Which pier the mount is on; writing needs `CanSetPierSide`.");

    scalar_pair!(site_elevation, set_site_elevation, "SiteElevation", f64, get_f64, set_f64, "Site height above sea level (metres).");
    scalar_pair!(site_latitude, set_site_latitude, "SiteLatitude", f64, get_f64, set_f64, "Site latitude (degrees, north positive).");
    scalar_pair!(site_longitude, set_site_longitude, "SiteLongitude", f64, get_f64, set_f64, "Site longitude (degrees, east positive).");
    scalar_pair!(slew_settle_time, set_slew_settle_time, "SlewSettleTime", i32, get_i32, set_i32, "Post-slew settling delay (seconds).");
    scalar_pair!(right_ascension_rate, set_right_ascension_rate, "RightAscensionRate", f64, get_f64, set_f64, "Superior drift rate in right ascension (arcsec/s).");
    scalar_pair!(declination_rate, set_declination_rate, "DeclinationRate", f64, get_f64, set_f64, "Superior drift rate in declination (arcsec/s).");
    scalar_pair!(guide_rate_right_ascension, set_guide_rate_right_ascension, "GuideRateRightAscension", f64, get_f64, set_f64, "Guide pulse rate in right ascension (multiple of sidereal).");
    scalar_pair!(guide_rate_declination, set_guide_rate_declination, "GuideRateDeclination", f64, get_f64, set_f64, "Guide pulse rate in declination (multiple of sidereal).");
    scalar_pair!(target_right_ascension, set_target_right_ascension, "TargetRightAscension", f64, get_f64, set_f64, "Right ascension of the `SlewToTarget` target (hours).");
    scalar_pair!(target_declination, set_target_declination, "TargetDeclination", f64, get_f64, set_f64, "Declination of the `SlewToTarget` target (degrees).");
    scalar_pair!(does_refraction, set_does_refraction, "DoesRefraction", bool, get_bool, set_bool, "Whether the driver applies atmospheric refraction.");
    scalar_pair!(tracking, set_tracking, "Tracking", bool, get_bool, set_bool, "Whether tracking is active.");

    /// The driver's clock, as a UTC instant (`VT_DATE` under the hood).
    pub fn utc_date(&self) -> Result<std::time::SystemTime> {
        self.actor.call(|device| device.dispatch().get("UTCDate")?.as_date())
    }

    /// Writes the driver's clock. Some drivers refuse this with
    /// `PropertyNotImplementedException`, which is a legal answer.
    pub fn set_utc_date(&self, value: std::time::SystemTime) -> Result<()> {
        self.actor.call(move |device| {
            let raw = Variant::from_date(value);
            device.dispatch().put("UTCDate", &raw)
        })
    }
}

impl AscomDevice for Telescope {
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

// ---------------------------------------------------------------------------
// Reading the collection-shaped members
//
// They take a `&Dispatch` rather than an `Actor`, so the in-process mocks of
// `com::mock` can drive them without a driver installed (as `device.rs` does).
// ---------------------------------------------------------------------------

/// Reads `AxisRates(axis)` off a driver object.
///
/// `Ok(vec![])` is honest only when the driver answered with an empty collection,
/// which is the spec's way of saying this axis cannot be moved. A `VT_EMPTY` reply is
/// the absence of an answer, so it is an error rather than a default.
fn read_axis_rates(dispatch: &Dispatch, axis: TelescopeAxis) -> Result<Vec<RateRange>> {
    let axis_v = Variant::from_enum(axis);
    let Some(rates) = dispatch.call("AxisRates", &[&axis_v])? else {
        return Err(AscomError::local(
            AscomErrorKind::ValueNotSet,
            "AxisRates",
            "the driver returned no value",
        ));
    };
    rate_ranges(&collections::objects(&rates)?)
}

/// Maps the `Rate` objects of an `AxisRates` answer onto their ranges.
fn rate_ranges(rates: &[Dispatch]) -> Result<Vec<RateRange>> {
    let mut out = Vec::with_capacity(rates.len());
    for rate in rates {
        out.push(RateRange {
            minimum: rate.get_f64("Minimum")?,
            maximum: rate.get_f64("Maximum")?,
        });
    }
    Ok(out)
}

/// Reads `TrackingRates` off a driver object.
///
/// `Ok(vec![])` means the driver does not implement the member, the normal answer of
/// a pre-V4 driver. A `VT_EMPTY` reply is already `ValueNotSet` inside
/// [`Dispatch::try_get`], which softens only `Unsupported`.
fn read_tracking_rates(dispatch: &Dispatch) -> Result<Vec<DriveRate>> {
    let Some(value) = dispatch.try_get("TrackingRates")? else {
        return Ok(Vec::new());
    };
    drive_rates(collections::ints(&value)?)
}

/// Maps the raw `TrackingRates` codes through the [`DriveRate`] mirror.
///
/// An unknown code fails the read instead of being dropped: the caller cannot tell a
/// list that lost an element from the complete set of rates the mount offers.
fn drive_rates(codes: Vec<i32>) -> Result<Vec<DriveRate>> {
    codes
        .iter()
        .map(|&code| {
            DriveRate::from_raw(code).ok_or_else(|| {
                AscomError::type_mismatch("TrackingRates", format!("unknown enum value {code}"))
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Enumerations, with the numbers fixed by the specification
// ---------------------------------------------------------------------------

/// `Telescope.AlignmentModes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum AlignmentMode {
    /// Alt-azimuth fork.
    AltAz = 0,
    /// Polar mount, north of the pier.
    Polar = 1,
    /// German equatorial mount.
    GermanPolar = 2,
}

impl AscomEnum for AlignmentMode {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::AltAz),
            1 => Some(Self::Polar),
            2 => Some(Self::GermanPolar),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

/// `Telescope.DriveRates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum DriveRate {
    Sidereal = 0,
    Lunar = 1,
    Solar = 2,
    King = 3,
}

impl AscomEnum for DriveRate {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Sidereal),
            1 => Some(Self::Lunar),
            2 => Some(Self::Solar),
            3 => Some(Self::King),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

/// `Telescope.EquatorialCoordinateType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum EquatorialCoordinateType {
    Other = 0,
    Topocentric = 1,
    J2000 = 2,
    J2050 = 3,
    B1950 = 4,
}

impl AscomEnum for EquatorialCoordinateType {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Other),
            1 => Some(Self::Topocentric),
            2 => Some(Self::J2000),
            3 => Some(Self::J2050),
            4 => Some(Self::B1950),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

/// `Telescope.PierSide` (called `PointingState` in the .NET library).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum PierSide {
    East = 0,
    West = 1,
    Unknown = -1,
}

impl AscomEnum for PierSide {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::East),
            1 => Some(Self::West),
            -1 => Some(Self::Unknown),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

/// `Telescope.TelescopeAxes` — *mechanical* axes.
///
/// The spec deliberately does not define which sign of `MoveAxis` rate corresponds to
/// which direction about an axis; a client must determine that empirically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum TelescopeAxis {
    Primary = 0,
    Secondary = 1,
    Tertiary = 2,
}

impl TelescopeAxis {
    /// All axes the specification defines, for probing a mount.
    pub const ALL: [TelescopeAxis; 3] = [Self::Primary, Self::Secondary, Self::Tertiary];
}

impl AscomEnum for TelescopeAxis {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Primary),
            1 => Some(Self::Secondary),
            2 => Some(Self::Tertiary),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::mock::{Element, Member, MockCollection, MockDevice};
    use windows::core::HRESULT;

    /// An ASCOM exception HRESULT, e.g. code `0x40B` = InvalidOperationException.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    /// A driver object answering exactly the configured members. The returned
    /// `Variant` keeps the mock alive.
    fn mock_dispatch(members: Vec<(&'static str, Member)>) -> (Variant, Dispatch) {
        let variant = MockDevice::new(members).into_variant();
        let dispatch = Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        (variant, dispatch)
    }

    #[test]
    fn telescope_enums_use_the_spec_numbers() {
        assert_eq!(AlignmentMode::to_raw(AlignmentMode::GermanPolar), 2);
        assert_eq!(AlignmentMode::from_raw(1), Some(AlignmentMode::Polar));
        assert_eq!(AlignmentMode::from_raw(3), None);

        assert_eq!(DriveRate::to_raw(DriveRate::King), 3);
        assert_eq!(DriveRate::from_raw(0), Some(DriveRate::Sidereal));
        assert_eq!(DriveRate::from_raw(-1), None);

        assert_eq!(EquatorialCoordinateType::to_raw(EquatorialCoordinateType::J2000), 2);
        assert_eq!(EquatorialCoordinateType::from_raw(4), Some(EquatorialCoordinateType::B1950));
        assert_eq!(EquatorialCoordinateType::from_raw(5), None);

        assert_eq!(PierSide::to_raw(PierSide::Unknown), -1);
        assert_eq!(PierSide::from_raw(0), Some(PierSide::East));
        assert_eq!(PierSide::from_raw(1), Some(PierSide::West));
        assert_eq!(PierSide::from_raw(2), None);

        assert_eq!(TelescopeAxis::to_raw(TelescopeAxis::Tertiary), 2);
        assert_eq!(TelescopeAxis::from_raw(0), Some(TelescopeAxis::Primary));
        assert_eq!(TelescopeAxis::ALL.len(), 3);
    }

    #[test]
    fn rate_ranges_are_magnitude_checks() {
        let range = RateRange { minimum: 0.5, maximum: 4.0 };
        assert!(range.contains(2.0));
        // The sign is the caller's direction choice, so both signs are in range.
        assert!(range.contains(-2.0));
        assert!(!range.contains(0.4));
        assert!(!range.contains(4.1));
        // A discrete rate is allowed to have equal bounds.
        assert!(RateRange { minimum: 1.0, maximum: 1.0 }.contains(1.0));
    }

    #[test]
    fn flag_members_match_the_interface() {
        // Every CanXxx of ITelescopeV4 must be cached, none invented.
        for expected in [
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
        ] {
            assert!(FLAG_MEMBERS.contains(&expected), "missing {expected}");
        }
        assert_eq!(FLAG_MEMBERS.len(), 16);
    }

    #[test]
    fn the_measured_drive_rate_codes_map_through() {
        // Exactly what OmniSim answers, in its own order: every code is known, so a
        // cooperative driver is unaffected by how unknown codes are treated.
        let rates = drive_rates(vec![0, 3, 1, 2]).expect("the spec codes are all known");
        assert_eq!(
            rates,
            [DriveRate::Sidereal, DriveRate::King, DriveRate::Lunar, DriveRate::Solar]
        );
    }

    #[test]
    fn an_unknown_tracking_rate_code_fails_the_read() {
        // Dropping the unknown code would leave a 3-element Vec that reads as the
        // mount's complete set of rates.
        let error = drive_rates(vec![0, 1, 2, 99])
            .expect_err("a code outside DriveRate must not be dropped quietly");
        assert_eq!(error.member, "TrackingRates");
        assert!(error.message.contains("99"), "the offending code is lost: {error}");
        // It fails the way every other enum member fails, i.e. like `as_enum`.
        let as_enum =
            Variant::from_i32(99).as_enum::<DriveRate>("TrackingRates").expect_err("same case");
        assert_eq!(error.kind, as_enum.kind, "misclassified against `as_enum`");
        assert_eq!(error.message, as_enum.message, "message differs from `as_enum`");
    }

    #[test]
    fn an_axis_rates_answer_of_no_value_is_value_not_set() {
        // `VT_EMPTY` is the absence of an answer; the spec's "this axis has no rates"
        // is a real empty collection, which the next test covers.
        let (_keep, dispatch) = mock_dispatch(vec![("AxisRates", Member::Value(Element::Empty))]);
        let error = read_axis_rates(&dispatch, TelescopeAxis::Primary)
            .expect_err("no value at all is not an empty rate list");
        assert_eq!(error.kind, AscomErrorKind::ValueNotSet);
        assert_eq!(error.member, "AxisRates");
    }

    #[test]
    fn an_empty_axis_rates_collection_is_a_valid_answer() {
        // The spec's answer for a mount without `MoveAxis`: an empty list, not an error.
        let empty = MockCollection::new(Vec::new()).into_variant();
        let rates = collections::objects(&empty).expect("an empty collection");
        assert!(rate_ranges(&rates).expect("an empty list is an answer").is_empty());
    }

    #[test]
    fn a_refused_axis_rates_is_not_read_as_no_rates() {
        // The member must not raise, but if the driver does raise, that is the driver
        // talking about the device and must not look like "no rates here".
        let (_keep, dispatch) = mock_dispatch(vec![("AxisRates", Member::Refuses(ascom_hr(0x40B)))]);
        let error = read_axis_rates(&dispatch, TelescopeAxis::Primary)
            .expect_err("a refusal is not an empty list");
        assert_eq!(error.kind, AscomErrorKind::InvalidOperation, "misclassified: {error}");
    }

    #[test]
    fn a_tracking_rates_member_the_driver_lacks_is_an_empty_list() {
        // `try_get` softens only `Unsupported`, which is a normal driver state: a driver
        // whose type has no `TrackingRates` at all has no rates to report.
        let (_keep, dispatch) = mock_dispatch(Vec::new());
        assert!(
            read_tracking_rates(&dispatch)
                .expect("a member the driver does not expose is not an error")
                .is_empty()
        );
    }

    #[test]
    fn a_tracking_rates_answer_of_no_value_is_value_not_set() {
        // `Dispatch::get` already reports `VT_EMPTY` as `ValueNotSet`, and `try_get`
        // softens only `Unsupported`, so an empty `Vec` here is never a missing answer.
        let (_keep, dispatch) =
            mock_dispatch(vec![("TrackingRates", Member::Value(Element::Empty))]);
        let error = read_tracking_rates(&dispatch)
            .expect_err("no value at all is not an empty list of rates");
        assert_eq!(error.kind, AscomErrorKind::ValueNotSet);
        assert_eq!(error.member, "TrackingRates");
    }
}
