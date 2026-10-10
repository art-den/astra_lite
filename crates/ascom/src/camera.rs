//! `ICameraV4`.
//!
//! The exposure cycle is fixed by the spec and by the fact that `ImageArray` is a
//! *synchronous* member that may take seconds:
//!
//! ```text
//! StartExposure(duration, light)   // async: only initiates
//!   -> wait for ImageReady == true // completion property
//!   -> read ImageArray             // blocking, on our COM thread
//! ```
//!
//! Reading `ImageArray` before `ImageReady` is `true` raises
//! `InvalidOperationException`, so [`Camera::read_image`] checks it first and reports
//! the situation as our own error rather than passing the driver's through.
//! `ImageArrayVariant` is never used.

use crate::actor::Actor;
use crate::com::collections;
use crate::com::variant::{AscomEnum, Variant};
use crate::device::{AscomDevice, DeviceSpec, GuideDirection, StateValue};
use crate::error::{AscomError, AscomErrorKind, Result};
use crate::image::{FrameGeometry, Image};
use crate::wait::{WaitSpec, wait_flag_true};

/// Capability members captured in [`AscomDevice::capabilities`].
pub const FLAG_MEMBERS: &[&str] = &[
    "CanAbortExposure",
    "CanAsymmetricBin",
    "CanFastReadout",
    "CanGetCoolerPower",
    "CanPulseGuide",
    "CanSetCCDTemperature",
    "CanStopExposure",
    "HasShutter",
];

/// A camera living on its own STA COM thread. `Send + Clone`.
#[derive(Clone)]
pub struct Camera {
    actor: Actor,
    spec: DeviceSpec,
}

impl Camera {
    /// Instantiates the driver named by `spec`; does not connect.
    pub fn open(spec: &DeviceSpec) -> Result<Self> {
        Ok(Self { actor: Actor::spawn(&spec.prog_id)?, spec: spec.clone() })
    }

    /// Builds the camera around an in-process mock driver answering `members`.
    /// Test seam only (`feature = "mock"`); it replaces instantiation alone.
    #[cfg(feature = "mock")]
    pub fn open_mock(
        members: Vec<(&'static str, crate::com::mock::Member)>,
        prog_id: &str,
    ) -> Result<Self> {
        Ok(Self { actor: Actor::spawn_mock(members)?, spec: DeviceSpec::new(prog_id) })
    }

    pub fn prog_id(&self) -> &str {
        &self.spec.prog_id
    }

    // Exposure ---------------------------------------------------------------

    /// Starts an exposure. Returns as soon as the camera has accepted it.
    ///
    /// `light = true` is a light frame, `false` a dark frame. A dark frame may ask for
    /// exactly `0` seconds (a bias/dark request) even though `ExposureMin` is non-zero:
    /// the lower bound describes light frames only.
    pub fn start_exposure(&self, duration: f64, light: bool) -> Result<()> {
        self.actor.call(move |device| {
            let (min, max) = exposure_bounds(device.dispatch())?;
            if let Err(detail) = exposure_allowed(duration, light, min, max) {
                return Err(AscomError::local(
                    AscomErrorKind::InvalidValue,
                    "StartExposure",
                    detail,
                )
                .with_source("ascom::camera"));
            }
            let duration_v = Variant::from_f64(duration);
            let light_v = Variant::from_bool(light);
            device.dispatch().call_void("StartExposure", &[&duration_v, &light_v])
        })
    }

    /// Stops an exposure in progress; needs `CanStopExposure`.
    pub fn stop_exposure(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("StopExposure", &[]))
    }

    /// Aborts an exposure, discarding the data; needs `CanAbortExposure`.
    pub fn abort_exposure(&self) -> Result<()> {
        self.actor.call(|device| device.dispatch().call_void("AbortExposure", &[]))
    }

    pub fn image_ready(&self) -> Result<bool> {
        self.actor.call(|device| device.dispatch().get_bool("ImageReady"))
    }

    /// Waits for `ImageReady == true`.
    pub fn wait_image_ready(&self, wait: WaitSpec) -> Result<()> {
        wait_flag_true(&self.actor, "ImageReady", wait)
    }

    /// Reads the finished frame.
    ///
    /// `ImageReady` is checked first: the spec says reading too early raises
    /// `InvalidOperationException`, and reporting it here keeps the failure
    /// attributable. Geometry comes from the same connection, in one round trip.
    pub fn read_image(&self) -> Result<Image> {
        self.actor.call(|device| {
            if !device.dispatch().get_bool("ImageReady")? {
                return Err(AscomError::local(
                    AscomErrorKind::InvalidOperation,
                    "ImageArray",
                    "ImageReady is false: there is no frame to read",
                )
                .with_source("ascom::camera"));
            }
            let geometry = geometry_of(device.dispatch())?;
            let array = device.dispatch().get("ImageArray")?;
            Image::from_variant(&array, geometry)
        })
    }

    /// The full cycle: initiate, wait for `ImageReady`, read the frame.
    ///
    /// `read_timeout` bounds only the wait; `ImageArray` itself is allowed to take a
    /// long time, which is exactly why the camera owns a thread of its own.
    pub fn expose(&self, duration: f64, light: bool, read_timeout: WaitSpec) -> Result<Image> {
        self.start_exposure(duration, light)?;
        self.wait_image_ready(read_timeout)?;
        self.read_image()
    }

    /// Fires a guide pulse through the camera's shutter or the mount.
    pub fn pulse_guide(&self, direction: GuideDirection, duration_ms: i32) -> Result<()> {
        self.actor.call(move |device| {
            let dir = Variant::from_enum(direction);
            let dur = Variant::from_i32(duration_ms);
            device.dispatch().call_void("PulseGuide", &[&dir, &dur])
        })
    }

    // Binning and sub-frames -------------------------------------------------

    /// Sets symmetric or asymmetric binning.
    ///
    /// Rejected before the driver sees it when the values exceed `MaxBinX`/`MaxBinY`,
    /// or when `bin_x != bin_y` on a camera without `CanAsymmetricBin`.
    ///
    /// A larger binning can leave the current `StartX`/`NumX` beyond the (now smaller)
    /// binned sensor. The spec deliberately puts that check in `StartExposure` rather
    /// than in the property writes, so this method neither rejects nor silently
    /// rewrites the frame.
    pub fn set_binning(&self, bin_x: i32, bin_y: i32) -> Result<()> {
        self.actor.call(move |device| {
            let max_x = device.dispatch().get_i32("MaxBinX")?;
            let max_y = device.dispatch().get_i32("MaxBinY")?;
            if bin_x < 1 || bin_y < 1 {
                return Err(binning_error(format!("binning must be >= 1, got {bin_x}x{bin_y}")));
            }
            if bin_x > max_x || bin_y > max_y {
                return Err(binning_error(format!(
                    "{bin_x}x{bin_y} exceeds MaxBinX={max_x}, MaxBinY={max_y}"
                )));
            }
            if bin_x != bin_y
                && !or_if_absent(device.dispatch().get_bool("CanAsymmetricBin"), false)?
            {
                return Err(binning_error(format!(
                    "{bin_x}x{bin_y} is asymmetric and CanAsymmetricBin is false"
                )));
            }
            device.dispatch().set_i32("BinX", bin_x)?;
            device.dispatch().set_i32("BinY", bin_y)
        })
    }

    /// Sub-frame, in the **binned** pixels the spec defines `StartX`/`NumX` with.
    ///
    /// Checked against `CameraXSize / BinX` and `CameraYSize / BinY`, so a frame that
    /// would run off the sensor never reaches the driver. The spec leaves this check to
    /// `StartExposure`; doing it here does not change what a valid frame does, it only
    /// makes an impossible one fail with a message that names the limit.
    pub fn set_sub_frame(&self, start_x: i32, start_y: i32, num_x: i32, num_y: i32) -> Result<()> {
        self.actor.call(move |device| {
            if num_x < 1 || num_y < 1 || start_x < 0 || start_y < 0 {
                return Err(binning_error(format!(
                    "sub-frame {num_x}x{num_y}+{start_x}+{start_y} is not a positive region"
                )));
            }
            let sensor_x = device.dispatch().get_i32("CameraXSize")?;
            let sensor_y = device.dispatch().get_i32("CameraYSize")?;
            // A driver that does not implement BinX/BinY keeps the unbinned limit; a
            // failure to answer propagates, because guessing bin 1 would loosen the
            // limit and let through a frame the binned sensor cannot produce.
            let bin_x = or_if_absent(device.dispatch().get_i32("BinX"), 1)?;
            let bin_y = or_if_absent(device.dispatch().get_i32("BinY"), 1)?;
            let limit_x = frame_limit(sensor_x, bin_x);
            let limit_y = frame_limit(sensor_y, bin_y);
            if sub_frame_overruns(start_x, num_x, limit_x)
                || sub_frame_overruns(start_y, num_y, limit_y)
            {
                return Err(binning_error(format!(
                    "sub-frame {num_x}x{num_y}+{start_x}+{start_y} runs past the \
                     {limit_x}x{limit_y} frame the {sensor_x}x{sensor_y} sensor leaves at \
                     binning {bin_x}x{bin_y}"
                )));
            }
            device.dispatch().set_i32("StartX", start_x)?;
            device.dispatch().set_i32("StartY", start_y)?;
            device.dispatch().set_i32("NumX", num_x)?;
            device.dispatch().set_i32("NumY", num_y)
        })
    }

    // Multi-valued members ---------------------------------------------------

    /// `Gains`: the discrete gain names the camera exposes (empty for continuous).
    pub fn gains(&self) -> Result<Vec<String>> {
        self.string_list("Gains")
    }

    /// `Offsets`: the discrete offset names the camera exposes.
    pub fn offsets(&self) -> Result<Vec<String>> {
        self.string_list("Offsets")
    }

    /// `ReadoutModes`: the discrete readout-mode names the camera exposes.
    pub fn readout_modes(&self) -> Result<Vec<String>> {
        self.string_list("ReadoutModes")
    }

    fn string_list(&self, member: &'static str) -> Result<Vec<String>> {
        self.actor.call(move |device| {
            let Some(value) = device.dispatch().try_get(member)? else {
                return Ok(Vec::new());
            };
            collections::strings(&value)
        })
    }

    /// Aggregated driver state (`DeviceState`), empty for V2/V3 drivers.
    pub fn state(&self) -> Result<Vec<StateValue>> {
        self.device_state()
    }
}

/// The bounds a light frame must fall inside, read off the driver.
///
/// An ancient driver may not implement the limits; then there is nothing to check
/// against and the driver itself decides. A read that merely failed is not that
/// answer: defaulting a `Com`/`Disconnected`/`Timeout` away would disable the local
/// pre-check and send a duration the camera cannot honour, so it propagates.
fn exposure_bounds(dispatch: &crate::com::Dispatch) -> Result<(f64, f64)> {
    let min = or_if_absent(dispatch.get_f64("ExposureMin"), f64::MIN)?;
    let max = or_if_absent(dispatch.get_f64("ExposureMax"), f64::MAX)?;
    Ok((min, max))
}

/// Whether `duration` may be sent to the driver for a frame of this kind.
///
/// `ExposureMin`..`ExposureMax` bound a **light** frame. The spec obliges the driver to
/// accept `Duration = 0` when `Light` is false — that is how an application asks for a
/// dark or bias frame — while requiring `ExposureMin` itself to be non-zero, so applying
/// the lower bound to a dark frame would forbid the request the spec mandates.
fn exposure_allowed(
    duration: f64,
    light: bool,
    min: f64,
    max: f64,
) -> std::result::Result<(), String> {
    if duration < 0.0 {
        return Err(format!("{duration} s is negative; a dark frame asks for exactly 0"));
    }
    if !light && duration == 0.0 {
        return Ok(());
    }
    if (min..=max).contains(&duration) {
        Ok(())
    } else {
        Err(format!(
            "{duration} s is outside [{min}, {max}] (ExposureMin..ExposureMax); only a dark \
             frame (Light = false) may ask for 0"
        ))
    }
}

fn binning_error(detail: String) -> AscomError {
    AscomError::local(AscomErrorKind::InvalidValue, "Binning", detail).with_source("ascom::camera")
}

/// How many pixels of a `sensor`-pixel detector fit in one axis at `bin`.
///
/// `StartX`/`NumX` count **binned** pixels, so the limit is the sensor size divided by
/// the binning, floored — which is what drivers enforce (an 800-pixel detector at Bin 2
/// accepts `NumX` up to 400 and answers `InvalidValue` above that).
fn frame_limit(sensor: i32, bin: i32) -> i32 {
    i32::max(sensor / i32::max(bin, 1), 1)
}

/// Whether the sub-frame span `[start, start + num)` runs past `limit`.
///
/// Compared in `i64`: `start` and `num` are caller data, and in `i32` a sum like
/// `i32::MAX + 1` overflows — a panic on the actor thread in debug builds, a wrap
/// to negative in release that lets the impossible frame reach the driver.
fn sub_frame_overruns(start: i32, num: i32, limit: i32) -> bool {
    i64::from(start) + i64::from(num) > i64::from(limit)
}

/// Substitutes `default` only when the driver genuinely does not expose the member.
///
/// `is_unsupported()` is the crate's classification of absence: the ASCOM 0x400
/// code, a raw `E_NOTIMPL`, and the late-binding name failures
/// (`DISP_E_UNKNOWNNAME`/`DISP_E_MEMBERNOTFOUND`) all map to it. Every other error
/// propagates — `Com`, `Disconnected`, `Timeout` are binding-layer failures, and
/// `ValueNotSet` is "no value", which is an error and never a default.
fn or_if_absent<T>(read: Result<T>, default: T) -> Result<T> {
    match read {
        Ok(value) => Ok(value),
        Err(err) if err.is_unsupported() => Ok(default),
        Err(err) => Err(err),
    }
}

/// Reads the members that describe the frame about to be transferred.
fn geometry_of(dispatch: &crate::com::Dispatch) -> Result<FrameGeometry> {
    // Only NumX/NumY are mandatory here; the rest degrades to a documented default,
    // but only when the driver does not implement the member. A read failure
    // propagates: this geometry ends up in the FITS header (ASCCDSEN, MAXADU,
    // BITDEPTH), and a binding-layer failure must not stamp values the driver
    // never reported.
    let sensor = or_if_absent(
        dispatch.get("SensorType").and_then(|v| v.as_enum::<SensorType>("SensorType")),
        SensorType::Monochrome,
    )?;
    Ok(FrameGeometry {
        num_x: dispatch.get_i32("NumX")?,
        num_y: dispatch.get_i32("NumY")?,
        adu_max: or_if_absent(dispatch.get_i32("MaxADU"), 0)?,
        sensor,
        bayer_offset_x: or_if_absent(dispatch.get_i32("BayerOffsetX"), 0)?,
        bayer_offset_y: or_if_absent(dispatch.get_i32("BayerOffsetY"), 0)?,
        bin_x: or_if_absent(dispatch.get_i32("BinX"), 1)?,
        bin_y: or_if_absent(dispatch.get_i32("BinY"), 1)?,
    })
}

macro_rules! read_property {
    ($method:ident, $member:literal, $ty:ty, $getter:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $method(&self) -> Result<$ty> {
            self.actor.call(|device| device.dispatch().$getter($member))
        }
    };
}

macro_rules! scalar_pair {
    ($get:ident, $set:ident, $member:literal, $ty:ty, $getter:ident, $setter:ident, $doc:literal) => {
        #[doc = $doc]
        pub fn $get(&self) -> Result<$ty> {
            self.actor.call(|device| device.dispatch().$getter($member))
        }

        #[doc = concat!("Sets [`Self::", stringify!($get), "`].")]
        pub fn $set(&self, value: $ty) -> Result<()> {
            self.actor.call(move |device| device.dispatch().$setter($member, value))
        }
    };
}

macro_rules! read_enum {
    ($method:ident, $member:literal, $enum:ty, $doc:literal) => {
        #[doc = $doc]
        pub fn $method(&self) -> Result<$enum> {
            self.actor.call(|device| device.dispatch().get($member)?.as_enum::<$enum>($member))
        }
    };
}

impl Camera {
    // Sensor and frame geometry (read-only) ----------------------------------

    read_property!(camera_x_size, "CameraXSize", i32, get_i32, "Detector width in unbinned pixels.");
    read_property!(camera_y_size, "CameraYSize", i32, get_i32, "Detector height in unbinned pixels.");
    read_property!(max_bin_x, "MaxBinX", i32, get_i32, "Largest usable binning factor in x.");
    read_property!(max_bin_y, "MaxBinY", i32, get_i32, "Largest usable binning factor in y.");
    read_property!(num_x, "NumX", i32, get_i32, "Width of the frame being transferred, in binned pixels.");
    read_property!(num_y, "NumY", i32, get_i32, "Height of the frame being transferred, in binned pixels.");
    read_property!(start_x, "StartX", i32, get_i32, "Sub-frame origin in x, in binned pixels (zero based).");
    read_property!(start_y, "StartY", i32, get_i32, "Sub-frame origin in y, in binned pixels (zero based).");
    read_property!(bin_x, "BinX", i32, get_i32, "Current binning factor in x.");
    read_property!(bin_y, "BinY", i32, get_i32, "Current binning factor in y.");
    read_property!(bayer_offset_x, "BayerOffsetX", i32, get_i32, "Bayer matrix offset in x (not affected by sub-frame settings).");
    read_property!(bayer_offset_y, "BayerOffsetY", i32, get_i32, "Bayer matrix offset in y (not affected by sub-frame settings).");
    read_property!(max_adu, "MaxADU", i32, get_i32, "Maximum ADU value the sensor can produce.");
    read_property!(pixel_size_x, "PixelSizeX", f64, get_f64, "Pixel pitch in x (micrometres).");
    read_property!(pixel_size_y, "PixelSizeY", f64, get_f64, "Pixel pitch in y (micrometres).");
    read_property!(sensor_name, "SensorName", String, get_string, "Detector name.");
    read_property!(electrons_per_adu, "ElectronsPerADU", f64, get_f64, "Conversion factor from ADU to electrons.");
    read_property!(full_well_capacity, "FullWellCapacity", f64, get_f64, "Full-well capacity (electrons).");
    read_property!(heat_sink_temperature, "HeatSinkTemperature", f64, get_f64, "Heat-sink temperature (degrees C), when reported.");

    // Exposure characteristics ----------------------------------------------

    read_property!(exposure_min, "ExposureMin", f64, get_f64, "Shortest exposure the camera accepts (seconds).");
    read_property!(exposure_max, "ExposureMax", f64, get_f64, "Longest exposure the camera accepts (seconds).");
    read_property!(exposure_resolution, "ExposureResolution", f64, get_f64, "Smallest exposure increment (seconds).");
    read_property!(last_exposure_duration, "LastExposureDuration", f64, get_f64, "Duration of the last completed exposure (seconds).");
    read_property!(last_exposure_start_time, "LastExposureStartTime", String, get_string, "Start time of the last exposure, as a string.");
    read_property!(percent_completed, "PercentCompleted", i32, get_i32, "Progress of the current operation, 0-100.");
    read_property!(is_pulse_guiding, "IsPulseGuiding", bool, get_bool, "A guide pulse is in progress.");

    // Thermal ----------------------------------------------------------------

    read_property!(ccd_temperature, "CCDTemperature", f64, get_f64, "Current detector temperature (degrees C).");
    read_property!(cooler_power, "CoolerPower", f64, get_f64, "Cooler power as a fraction of maximum, 0-1.");

    // Readout ----------------------------------------------------------------

    read_property!(readout_mode, "ReadoutMode", i32, get_i32, "Index into `ReadoutModes`.");

    read_enum!(camera_state, "CameraState", CameraState, "Where the camera is in its exposure cycle.");
    read_enum!(sensor_type, "SensorType", SensorType, "Sensor kind, which decides how to interpret the pixels.");

    scalar_pair!(cooler_on, set_cooler_on, "CoolerOn", bool, get_bool, set_bool, "Whether the cooler is engaged.");
    scalar_pair!(fast_readout, set_fast_readout, "FastReadout", bool, get_bool, set_bool, "Fast readout mode (trades dynamic range for speed); needs `CanFastReadout`.");
    scalar_pair!(gain, set_gain, "Gain", i32, get_i32, set_i32, "Current gain (index into `Gains`, or an absolute value).");
    scalar_pair!(offset, set_offset, "Offset", i32, get_i32, set_i32, "Current offset (index into `Offsets`, or an absolute value).");

    read_property!(gain_min, "GainMin", i32, get_i32, "Lowest usable gain.");
    read_property!(gain_max, "GainMax", i32, get_i32, "Highest usable gain.");
    read_property!(offset_min, "OffsetMin", i32, get_i32, "Lowest usable offset.");
    read_property!(offset_max, "OffsetMax", i32, get_i32, "Highest usable offset.");
    read_property!(sub_exposure_duration, "SubExposureDuration", f64, get_f64, "Exposure time actually used by the last or current exposure (seconds).");

    /// Target detector temperature.
    pub fn target_ccd_temperature(&self) -> Result<f64> {
        self.actor.call(|device| device.dispatch().get_f64("SetCCDTemperature"))
    }

    /// Sets the target detector temperature; needs `CanSetCCDTemperature`.
    pub fn set_target_ccd_temperature(&self, value: f64) -> Result<()> {
        self.actor.call(move |device| device.dispatch().set_f64("SetCCDTemperature", value))
    }
}

impl AscomDevice for Camera {
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
// Enumerations
// ---------------------------------------------------------------------------

/// `Camera.CameraStates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum CameraState {
    Idle = 0,
    /// Waiting for an exposure to start.
    Waiting = 1,
    Exposing = 2,
    Reading = 3,
    Downloading = 4,
    Error = 5,
}

impl AscomEnum for CameraState {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Idle),
            1 => Some(Self::Waiting),
            2 => Some(Self::Exposing),
            3 => Some(Self::Reading),
            4 => Some(Self::Downloading),
            5 => Some(Self::Error),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

/// `Camera.SensorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum SensorType {
    Monochrome = 0,
    Color = 1,
    RGGB = 2,
    CMYG = 3,
    CMYG2 = 4,
    LRGB = 5,
}

impl SensorType {
    /// Name for the FITS header.
    pub fn name(self) -> &'static str {
        match self {
            Self::Monochrome => "Mono",
            Self::Color => "Color",
            Self::RGGB => "RGGB",
            Self::CMYG => "CMYG",
            Self::CMYG2 => "CMYG2",
            Self::LRGB => "LRGB",
        }
    }

    /// Number of pixel planes a full-colour sensor transfers.
    ///
    /// Bayer sensors (`RGGB`, `CMYG`, `CMYG2`) and monochrome ones transfer a single
    /// plane; `LRGB` transfers four, `Color` three.
    pub fn planes(self) -> usize {
        match self {
            Self::Monochrome | Self::RGGB | Self::CMYG | Self::CMYG2 => 1,
            Self::Color => 3,
            Self::LRGB => 4,
        }
    }
}

impl AscomEnum for SensorType {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::Monochrome),
            1 => Some(Self::Color),
            2 => Some(Self::RGGB),
            3 => Some(Self::CMYG),
            4 => Some(Self::CMYG2),
            5 => Some(Self::LRGB),
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
    use crate::com::mock::{Element, Member, MockDevice};
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{
        DISP_E_MEMBERNOTFOUND, DISP_E_UNKNOWNNAME, E_FAIL, E_NOTIMPL, RPC_E_DISCONNECTED,
    };

    /// An ASCOM exception HRESULT, e.g. code `0x402` = ValueNotSet.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    /// A driver object answering exactly the configured members. A name the device
    /// does not carry is refused with `DISP_E_UNKNOWNNAME`, i.e. classified as
    /// "this driver does not implement the member". The returned `Variant` keeps
    /// the mock alive.
    fn mock_dispatch(members: Vec<(&'static str, Member)>) -> (Variant, crate::com::Dispatch) {
        let variant = MockDevice::new(members).into_variant();
        let dispatch =
            crate::com::Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        (variant, dispatch)
    }

    #[test]
    fn camera_state_numbers_match_the_spec() {
        assert_eq!(CameraState::to_raw(CameraState::Idle), 0);
        assert_eq!(CameraState::to_raw(CameraState::Error), 5);
        assert_eq!(CameraState::from_raw(2), Some(CameraState::Exposing));
        assert_eq!(CameraState::from_raw(6), None);
        assert_eq!(CameraState::from_raw(-1), None);
    }

    #[test]
    fn sensor_type_numbers_and_plane_counts() {
        assert_eq!(SensorType::to_raw(SensorType::Monochrome), 0);
        assert_eq!(SensorType::to_raw(SensorType::LRGB), 5);
        assert_eq!(SensorType::from_raw(3), Some(SensorType::CMYG));
        assert_eq!(SensorType::from_raw(9), None);

        assert_eq!(SensorType::Monochrome.planes(), 1);
        assert_eq!(SensorType::RGGB.planes(), 1, "Bayer is a single plane");
        assert_eq!(SensorType::Color.planes(), 3);
        assert_eq!(SensorType::LRGB.planes(), 4);
        assert_eq!(SensorType::CMYG2.name(), "CMYG2");
    }

    #[test]
    fn capability_list_matches_the_interface() {
        for expected in [
            "CanAbortExposure",
            "CanAsymmetricBin",
            "CanFastReadout",
            "CanGetCoolerPower",
            "CanPulseGuide",
            "CanSetCCDTemperature",
            "CanStopExposure",
            "HasShutter",
        ] {
            assert!(FLAG_MEMBERS.contains(&expected), "missing {expected}");
        }
        assert_eq!(FLAG_MEMBERS.len(), 8);
    }

    #[test]
    fn binning_errors_are_attributable_to_the_caller() {
        let err = binning_error("nope".to_string());
        assert_eq!(err.kind, AscomErrorKind::InvalidValue);
        assert_eq!(err.source, "ascom::camera");
        assert_eq!(err.member, "Binning");
    }

    #[test]
    fn sub_frame_limits_count_binned_pixels() {
        assert_eq!(frame_limit(800, 1), 800);
        assert_eq!(frame_limit(800, 2), 400, "what OmniSim answers at Bin 2");
        assert_eq!(frame_limit(800, 4), 200);
        assert_eq!(frame_limit(801, 2), 400, "floored, as drivers enforce it");
        assert_eq!(frame_limit(800, 0), 800, "a driver answering 0 must not divide by zero");
        assert_eq!(frame_limit(2, 4), 1, "one pixel is always addressable");
    }

    /// `ExposureMin` is required to be non-zero, so it cannot be the bound for a dark
    /// frame: 0 s there is the request the spec tells drivers to honour.
    #[test]
    fn only_a_dark_frame_may_ask_for_zero_seconds() {
        assert!(exposure_allowed(0.0, false, 0.001, 3600.0).is_ok(), "bias/dark request");
        assert!(exposure_allowed(0.0, true, 0.001, 3600.0).is_err(), "a light frame obeys ExposureMin");
        assert!(exposure_allowed(0.001, true, 0.001, 3600.0).is_ok());
        assert!(exposure_allowed(0.0005, true, 0.001, 3600.0).is_err());
        assert!(exposure_allowed(3600.0, true, 0.001, 3600.0).is_ok());
        assert!(exposure_allowed(3600.1, true, 0.001, 3600.0).is_err());
        // Negative is nonsense in both directions, however small.
        assert!(exposure_allowed(-0.0001, false, 0.001, 3600.0).is_err());
        // A driver that reports no limits leaves the decision to itself.
        assert!(exposure_allowed(0.5, true, f64::MIN, f64::MAX).is_ok());
    }

    /// The pre-check exists so a duration the sensor cannot produce never reaches the
    /// driver, so a limit read that only *failed* must not disable it.
    #[test]
    fn a_failed_exposure_limit_read_does_not_disable_the_precheck() {
        // A driver that does not expose the members is the documented absence: no
        // limits, and the driver judges the duration itself.
        let (_keep, dispatch) = mock_dispatch(Vec::new());
        let bounds = exposure_bounds(&dispatch).expect("absent limits are a normal answer");
        assert_eq!(bounds, (f64::MIN, f64::MAX));

        // A driver that answers is read verbatim, so the bounds really are its own.
        let (_keep, dispatch) = mock_dispatch(vec![
            ("ExposureMin", Member::Value(Element::Int(1))),
            ("ExposureMax", Member::Value(Element::Int(3600))),
        ]);
        assert_eq!(exposure_bounds(&dispatch).expect("a cooperative driver"), (1.0, 3600.0));

        for (member, hresult, kind) in [
            ("ExposureMin", ascom_hr(0x407), AscomErrorKind::NotConnected),
            ("ExposureMax", ascom_hr(0x40B), AscomErrorKind::InvalidOperation),
            ("ExposureMin", E_FAIL, AscomErrorKind::Com),
            ("ExposureMax", RPC_E_DISCONNECTED, AscomErrorKind::Disconnected),
        ] {
            let (_keep, dispatch) = mock_dispatch(vec![(member, Member::Refuses(hresult))]);
            let error = exposure_bounds(&dispatch)
                .expect_err("a failed limit read is not an absence of limits");
            assert_eq!(error.kind, kind, "misclassified through {member}: {error}");
        }
    }

    #[test]
    fn sub_frame_check_survives_i32_overflow() {
        // `start + num` is unchecked `i32` math over caller data: `set_sub_frame
        // (i32::MAX, 0, 1, 1)` passed the positivity guard and overflowed here — a
        // panic on the actor thread in debug, a wrap to negative in release that let
        // the impossible frame reach the driver.
        assert!(sub_frame_overruns(i32::MAX, 1, 800));
        assert!(sub_frame_overruns(0, i32::MAX, 800));
        // The boundary itself is exact: [start, start + num) must fit in the limit.
        assert!(!sub_frame_overruns(700, 100, 800));
        assert!(sub_frame_overruns(701, 100, 800));
        assert!(!sub_frame_overruns(0, i32::MAX, i32::MAX));
        assert!(sub_frame_overruns(i32::MAX, 1, i32::MAX));
    }

    /// The whole contract of the helper: default on genuine absence only, every
    /// other answer propagates unchanged.
    #[test]
    fn only_an_absent_member_may_default() {
        assert_eq!(or_if_absent(Ok(7), -1).unwrap(), 7);
        // Every shape of "this driver does not implement the member".
        for hresult in [
            ascom_hr(0x400),
            HRESULT(E_NOTIMPL.0),
            HRESULT(DISP_E_UNKNOWNNAME.0),
            HRESULT(DISP_E_MEMBERNOTFOUND.0),
        ] {
            let error = AscomError::from_hresult(hresult.0, "MaxADU");
            assert!(error.is_unsupported(), "hr 0x{:08X} is not absence", hresult.0 as u32);
            assert_eq!(or_if_absent(Err(error), -1).unwrap(), -1);
        }
        // Everything else is the driver or the binding layer talking, not absence.
        for (hresult, kind) in [
            (ascom_hr(0x402), AscomErrorKind::ValueNotSet),
            (ascom_hr(0x407), AscomErrorKind::NotConnected),
            (HRESULT(RPC_E_DISCONNECTED.0), AscomErrorKind::Disconnected),
            (HRESULT(E_FAIL.0), AscomErrorKind::Com),
        ] {
            let error = AscomError::from_hresult(hresult.0, "MaxADU");
            assert_eq!(error.kind, kind, "hr 0x{:08X}", hresult.0 as u32);
            let propagated = or_if_absent(Err(error), -1).expect_err("must not default");
            assert_eq!(propagated.kind, kind);
        }
        // A crate-generated timeout is likewise not an absent member.
        let timeout = or_if_absent(Err(AscomError::timeout("MaxADU", "gone")), -1)
            .expect_err("a timeout is not absence");
        assert_eq!(timeout.kind, AscomErrorKind::Timeout);
    }

    /// A driver that implements only the mandatory members still yields a geometry:
    /// absence of an optional member is the driver's normal answer, not a failure.
    #[test]
    fn a_sparse_driver_geometry_falls_back_to_documented_defaults() {
        let (_keep, dispatch) = mock_dispatch(vec![
            ("NumX", Member::Value(Element::Int(400))),
            ("NumY", Member::Value(Element::Int(300))),
        ]);
        let geometry = geometry_of(&dispatch).expect("absent members are a normal answer");
        assert_eq!(geometry.num_x, 400);
        assert_eq!(geometry.num_y, 300);
        assert_eq!(geometry.adu_max, 0);
        assert_eq!(geometry.sensor, SensorType::Monochrome);
        assert_eq!(geometry.bayer_offset_x, 0);
        assert_eq!(geometry.bayer_offset_y, 0);
        assert_eq!((geometry.bin_x, geometry.bin_y), (1, 1));
    }

    /// What a cooperative driver reports must survive untouched.
    #[test]
    fn a_full_geometry_answer_is_read_verbatim() {
        let (_keep, dispatch) = mock_dispatch(vec![
            ("NumX", Member::Value(Element::Int(200))),
            ("NumY", Member::Value(Element::Int(150))),
            ("MaxADU", Member::Value(Element::Int(65535))),
            ("SensorType", Member::Value(Element::Int(SensorType::RGGB.to_raw()))),
            ("BayerOffsetX", Member::Value(Element::Int(1))),
            ("BayerOffsetY", Member::Value(Element::Int(0))),
            ("BinX", Member::Value(Element::Int(2))),
            ("BinY", Member::Value(Element::Int(1))),
        ]);
        let geometry = geometry_of(&dispatch).expect("the driver answered everything");
        assert_eq!(geometry.adu_max, 65535);
        assert_eq!(geometry.sensor, SensorType::RGGB);
        assert_eq!((geometry.bayer_offset_x, geometry.bayer_offset_y), (1, 0));
        assert_eq!((geometry.bin_x, geometry.bin_y), (2, 1));
    }

    /// A binding-layer failure is not absence: swallowing it into a default makes
    /// `write_fits` stamp MAXADU=0, BITDEPTH=0 or ASCCDSEN=Mono — assertions the
    /// driver never made.
    #[test]
    fn a_binding_failure_is_not_an_absent_geometry_member() {
        for (member, hresult, kind) in [
            ("MaxADU", HRESULT(RPC_E_DISCONNECTED.0), AscomErrorKind::Disconnected),
            ("SensorType", HRESULT(RPC_E_DISCONNECTED.0), AscomErrorKind::Disconnected),
            ("BinX", HRESULT(RPC_E_DISCONNECTED.0), AscomErrorKind::Disconnected),
        ] {
            let (_keep, dispatch) = mock_dispatch(vec![
                ("NumX", Member::Value(Element::Int(8))),
                ("NumY", Member::Value(Element::Int(8))),
                (member, Member::Refuses(hresult)),
            ]);
            let error = geometry_of(&dispatch)
                .expect_err("a binding-layer failure must not become a default");
            assert_eq!(error.kind, kind, "misclassified through {member}: {error}");
        }
    }
}
