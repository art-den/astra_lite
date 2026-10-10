//! ASCOM Classic (COM) HAL implementation, backed by the `ascom` crate.
//!
//! Drivers are enumerated once at startup from the registry (no COM calls) and
//! exposed as LAZY wrappers: they appear in device lists immediately, hold no
//! COM handle, and answer `Err("... not connected")` to everything but
//! `id()/name()/is_active()`. Selecting a device (`CurDevices::change_*`) calls
//! `Device::activate()`, which opens the COM object, connects, and caches the
//! capabilities/static data. There are no Connect/Disconnect buttons for ASCOM:
//! selection in the UI == connection.
//!
//! All calls are synchronous (the `ascom` crate owns one STA COM thread per
//! device); `activate()`/`deactivate()` and every COM read must run with no
//! `options`/`data` locks held because HAL events execute synchronously on the
//! sender thread.
//!
//! A driver may register several ProgIDs for one COM class (e.g. OmniSim is
//! both `ASCOM.OmniSim.Camera` and `ASCOM.Simulator.Camera`). Such wrappers
//! share the class instance and its `Connected` flag, so connect/disconnect is
//! refcounted per CLSID: only the first wrapper connects, only the last
//! disconnects.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ascom::camera::Camera as AcCamera;
use ascom::camera::SensorType as AcSensorType;
use ascom::chooser::DeviceType as AcDeviceType;
use ascom::device::{AscomDevice, DeviceSpec as AcDeviceSpec, GuideDirection as AcGuideDirection};
use ascom::drivers::installed_drivers;
use ascom::error::AscomErrorKind;
use ascom::filterwheel::FilterWheel as AcFilterWheel;
use ascom::filterwheel::slot_of as ac_slot_of;
use ascom::focuser::Focuser as AcFocuser;
use ascom::image::Image as AcImage;
use ascom::telescope::Telescope as AcTelescope;
use ascom::telescope::TelescopeAxis as AcTelescopeAxis;
use bitflags::bitflags;

use crate::hal::events::HalEventHandlers;
use crate::hal::events::HalEvent;
use crate::hal::*;
use crate::image::io::save_raw_image_to_fits_file;
use crate::image::raw::{CfaType, RawImage, RawImageInfo};

const SIDERAL_RATE_DEG_PER_SEC: f64 = 360.0 / (23.0 * 60.0 * 60.0 + 56.0 * 60.0 + 4.09);

///////////////////////////////////////////////////////////////////////////////
// Common machinery

/// Handles must be `Send + Sync`: one STA COM thread per device behind them.
fn assert_send_sync<T: Send + Sync>() {}

/// Compile-time guarantees from the `ascom` crate threading contract.
fn check_ascom_handle_contracts() {
    assert_send_sync::<AcCamera>();
    assert_send_sync::<AcTelescope>();
    assert_send_sync::<AcFocuser>();
    assert_send_sync::<AcFilterWheel>();
}

/// Shared between the impl and all device wrappers.
struct HalCtx {
    event_handlers: Arc<HalEventHandlers>,
    active_count:   AtomicUsize,
    /// Connect refcounts per COM class (`conn_key`): ProgID aliases of one
    /// driver share a single connected class instance.
    conn_refs:      Mutex<HashMap<String, usize>>,
}

impl HalCtx {
    /// Registers a connection on a COM class; `false` means another wrapper
    /// of the same class already connects the driver.
    fn conn_acquire(&self, key: &str) -> bool {
        let mut refs = self.conn_refs.lock().unwrap();
        let count = refs.entry(key.to_string()).or_insert(0);
        *count += 1;
        *count == 1
    }

    /// Unregisters a connection; `true` means this was the last wrapper of the
    /// class, so the caller must disconnect the shared driver.
    fn conn_release(&self, key: &str) -> bool {
        let mut refs = self.conn_refs.lock().unwrap();
        if let Some(count) = refs.get_mut(key) {
            *count -= 1;
            if *count == 0 {
                refs.remove(key);
                return true;
            }
        }
        false
    }

    fn device_activated(&self) {
        self.active_count.fetch_add(1, Ordering::Relaxed);
        self.send_state_event();
    }

    fn device_deactivated(&self) {
        self.active_count.fetch_sub(1, Ordering::Relaxed);
        self.send_state_event();
    }

    fn send_state_event(&self) {
        let state = if self.active_count.load(Ordering::Relaxed) > 0 {
            HalState::Connected
        } else {
            HalState::Disconnected
        };
        self.event_handlers.send(HalEvent::StateChanged(state));
    }
}

fn not_connected(device_id: &str) -> eyre::Report {
    eyre::eyre!("ASCOM device {device_id} is not connected")
}

/// Maps an `ascom` error into an `eyre` report with context (panic = "abort":
/// nothing may unwrap a COM result)
fn ac_err<T>(ctx: &str, res: ascom::error::Result<T>) -> eyre::Result<T> {
    res.map_err(|err| eyre::eyre!("{ctx}: {err}"))
}

/// Activation state of a lazy device wrapper.
enum ActState<T> {
    Idle,
    Busy,
    Active(Arc<T>),
}

/// Takes the active payload out of a state, leaving `Busy` untouched
/// (an activation in flight must not be clobbered by deactivation).
fn take_active<T>(state: &mut ActState<T>) -> Option<Arc<T>> {
    match std::mem::replace(state, ActState::Idle) {
        ActState::Active(data) => Some(data),
        other => {
            *state = other;
            None
        }
    }
}

/// Begins activation: `Ok(true)` when the caller must activate,
/// `Ok(false)` when the device is already active (idempotent).
fn begin_activation<T>(state: &mut ActState<T>) -> eyre::Result<bool> {
    match &*state {
        ActState::Active(_) => Ok(false),
        ActState::Busy => eyre::bail!("ASCOM device is busy (activation in progress)"),
        ActState::Idle => {
            *state = ActState::Busy;
            Ok(true)
        }
    }
}

/// Rolls back a failed activation while the COM handle is still alive: releases
/// the connection ref and, when it was the class's last wrapper, disconnects the
/// shared driver (drop never disconnects). Mirrors `deactivate_impl`: disconnect
/// failures are logged, never propagated, so the original error is not masked.
fn rollback_activation<D: AscomDevice>(ctx: &HalCtx, device: &D, conn_key: &str, device_id: &str) {
    if ctx.conn_release(conn_key) && let Err(err) = device.set_connected(false) {
        log::error!("cannot disconnect ASCOM device {device_id}: {err}");
    }
}

///////////////////////////////////////////////////////////////////////////////
// AscomHalImpl

struct AscomHalData {
    devices:       Vec<DeviceInfo>,
    cameras:       Vec<Arc<AscomCamera>>,
    telescopes:    Vec<Arc<AscomTelescope>>,
    focusers:      Vec<Arc<AscomFocuser>>,
    filter_wheels: Vec<Arc<AscomFilterWheel>>,
}

pub struct AscomHalImpl {
    data:           RwLock<Arc<AscomHalData>>,
    ctx:            Arc<HalCtx>,
}

impl AscomHalImpl {
    pub fn new(event_handlers: &Arc<HalEventHandlers>) -> Arc<Self> {
        check_ascom_handle_contracts();

        let ctx = Arc::new(HalCtx {
            event_handlers: Arc::clone(event_handlers),
            active_count:   AtomicUsize::new(0),
            conn_refs:      Mutex::new(HashMap::new()),
        });

        let mut data = AscomHalData {
            devices:       Vec::new(),
            cameras:       Vec::new(),
            telescopes:    Vec::new(),
            focusers:      Vec::new(),
            filter_wheels: Vec::new(),
        };

        for driver in drivers_of(AcDeviceType::Camera) {
            let info = device_info(&driver.prog_id, &driver.description, DeviceType::CAMERA);
            data.cameras.push(Arc::new(AscomCamera::new(Arc::clone(&ctx), info.clone())));
            data.devices.push(info);
        }
        for driver in drivers_of(AcDeviceType::Telescope) {
            let info = device_info(&driver.prog_id, &driver.description, DeviceType::TELESCOPE);
            data.telescopes.push(Arc::new(AscomTelescope::new(Arc::clone(&ctx), info.clone())));
            data.devices.push(info);
        }
        for driver in drivers_of(AcDeviceType::Focuser) {
            let info = device_info(&driver.prog_id, &driver.description, DeviceType::FOCUSER);
            data.focusers.push(Arc::new(AscomFocuser::new(Arc::clone(&ctx), info.clone())));
            data.devices.push(info);
        }
        for driver in drivers_of(AcDeviceType::FilterWheel) {
            let info = device_info(&driver.prog_id, &driver.description, DeviceType::FLT_WHEEL);
            data.filter_wheels.push(Arc::new(AscomFilterWheel::new(Arc::clone(&ctx), info.clone())));
            data.devices.push(info);
        }

        log::info!(
            "ASCOM Classic: {} camera(s), {} telescope(s), {} focuser(s), {} filter wheel(s)",
            data.cameras.len(), data.telescopes.len(),
            data.focusers.len(), data.filter_wheels.len(),
        );

        Arc::new(Self {
            data: RwLock::new(Arc::new(data)),
            ctx,
        })
    }

    /// Disconnects every activated device. Called on engine stop/shutdown:
    /// dropping the last handle does NOT disconnect a driver.
    /// All deactivations are attempted; if any failed, the first error is
    /// returned (the rest are only logged).
    pub fn disconnect_all(&self) -> eyre::Result<()> {
        let data = self.data_read();
        let mut first_err: Option<eyre::Report> = None;
        let mut check = |res: eyre::Result<()>, context: &str| {
            log_if_error_pub(&res, context);
            if first_err.is_none() {
                first_err = res.err();
            }
        };
        for camera in &data.cameras {
            check(camera.deactivate_impl(), "Deactivate ASCOM camera");
        }
        for telescope in &data.telescopes {
            check(telescope.deactivate_impl(), "Deactivate ASCOM telescope");
        }
        for focuser in &data.focusers {
            check(focuser.deactivate_impl(), "Deactivate ASCOM focuser");
        }
        for filter_wheel in &data.filter_wheels {
            check(filter_wheel.deactivate_impl(), "Deactivate ASCOM filter wheel");
        }
        first_err.map_or(Ok(()), Err)
    }

    pub fn find_camera(&self, id: &str) -> Option<Arc<AscomCamera>> {
        self.data_read().cameras.iter().find(|d| *d.device_id == id).map(Arc::clone)
    }

    pub fn find_telescope(&self, id: &str) -> Option<Arc<AscomTelescope>> {
        self.data_read().telescopes.iter().find(|d| *d.device_id == id).map(Arc::clone)
    }

    pub fn find_focuser(&self, id: &str) -> Option<Arc<AscomFocuser>> {
        self.data_read().focusers.iter().find(|d| *d.device_id == id).map(Arc::clone)
    }

    pub fn find_filter_wheel(&self, id: &str) -> Option<Arc<AscomFilterWheel>> {
        self.data_read().filter_wheels.iter().find(|d| *d.device_id == id).map(Arc::clone)
    }

    fn data_read(&self) -> Arc<AscomHalData> {
        Arc::clone(&self.data.read().unwrap())
    }
}

/// Registry enumeration never touches COM. No ASCOM platform -> empty list,
/// not an error; a registry read failure is logged and treated as empty.
fn drivers_of(ac_type: AcDeviceType) -> Vec<ascom::drivers::DriverInfo> {
    match installed_drivers(ac_type) {
        Ok(list) => list,
        Err(err) => {
            log::error!("Cannot list ASCOM {} drivers: {err}", ac_type.as_str());
            Vec::new()
        }
    }
}

/// COM class id (CLSID) of a ProgID, or the ProgID itself when unresolvable.
/// A driver may register several ProgIDs for one class (e.g. OmniSim exposes
/// `ASCOM.OmniSim.Camera` also as `ASCOM.Simulator.Camera`): the class
/// instance and its `Connected` flag are shared, so wrappers with the same
/// CLSID must coordinate connect/disconnect via this key. Registry only, no
/// COM calls.
fn conn_key_of(prog_id: &str) -> String {
    use ascom::com::registry::{default_string, Hive, View};
    for (hive, view) in [
        (Hive::Machine, View::SixtyFour),
        (Hive::Machine, View::ThirtyTwo),
        (Hive::User,    View::SixtyFour),
        (Hive::User,    View::ThirtyTwo),
    ] {
        let path = format!(r"SOFTWARE\Classes\{prog_id}\CLSID");
        if let Ok(Some(clsid)) = default_string(hive, &path, view) {
            return clsid.trim().to_lowercase();
        }
    }
    prog_id.to_lowercase()
}

fn device_info(prog_id: &str, description: &Option<String>, type_: DeviceType) -> DeviceInfo {
    DeviceInfo {
        id:    prog_id.to_string(),
        name:  description.clone().unwrap_or_else(|| prog_id.to_string()),
        type_,
    }
}

fn log_if_error_pub(res: &eyre::Result<()>, context: &str) {
    if let Err(err) = res {
        log::error!("Error {err}, context: {context}");
    }
}

impl HalImpl for AscomHalImpl {
    fn state(&self) -> HalState {
        if self.ctx.active_count.load(Ordering::Relaxed) > 0 {
            HalState::Connected
        } else {
            HalState::Disconnected
        }
    }

    fn disconnect(&self) -> eyre::Result<()> {
        self.disconnect_all()
    }

    fn notify_periodic_timer_tick(&self, timer_period_ms: usize) -> eyre::Result<()> {
        let data = self.data_read();

        // Contract: never return Err here. An Err from the tick aborts the active
        // mode; single COM read failures are logged and reported as HalEvent::Error.
        for camera in &data.cameras {
            if let Err(err) = camera.notify_periodic_timer_tick(timer_period_ms) {
                log::error!("ASCOM camera tick error: {err}");
                self.ctx.event_handlers.send(HalEvent::Error(Arc::new(err.to_string())));
            }
        }
        for telescope in &data.telescopes {
            if let Err(err) = telescope.notify_periodic_timer_tick(timer_period_ms) {
                log::error!("ASCOM telescope tick error: {err}");
                self.ctx.event_handlers.send(HalEvent::Error(Arc::new(err.to_string())));
            }
        }
        for focuser in &data.focusers {
            if let Err(err) = focuser.notify_periodic_timer_tick(timer_period_ms) {
                log::error!("ASCOM focuser tick error: {err}");
                self.ctx.event_handlers.send(HalEvent::Error(Arc::new(err.to_string())));
            }
        }
        for filter_wheel in &data.filter_wheels {
            if let Err(err) = filter_wheel.notify_periodic_timer_tick(timer_period_ms) {
                log::error!("ASCOM filter wheel tick error: {err}");
                self.ctx.event_handlers.send(HalEvent::Error(Arc::new(err.to_string())));
            }
        }

        Ok(())
    }

    fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>> {
        let result = self.data_read().devices
            .iter()
            .filter(|dev| dev.type_.contains(type_filter))
            .cloned()
            .collect();
        Ok(result)
    }

    fn cameras(&self) -> eyre::Result<Vec<CameraInfo>> {
        let result = self.data_read().cameras
            .iter()
            .map(|cam| CameraInfo {
                id:   cam.device_id.to_string(),
                name: cam.device_name.clone(),
                ccd:  CcdPurpose::Unknown,
            })
            .collect();
        Ok(result)
    }

    fn camera(&self, id: &str) -> Option<Arc<dyn Camera + Send + Sync>> {
        self.find_camera(id).map(|d| d as Arc<dyn Camera + Send + Sync>)
    }

    fn telescope(&self, id: &str) -> Option<Arc<dyn Telescope + Send + Sync>> {
        self.find_telescope(id).map(|d| d as Arc<dyn Telescope + Send + Sync>)
    }

    fn focuser(&self, id: &str) -> Option<Arc<dyn Focuser + Send + Sync>> {
        self.find_focuser(id).map(|d| d as Arc<dyn Focuser + Send + Sync>)
    }

    fn filter_wheel(&self, id: &str) -> Option<Arc<dyn FilterWheel + Send + Sync>> {
        self.find_filter_wheel(id).map(|d| d as Arc<dyn FilterWheel + Send + Sync>)
    }
}

///////////////////////////////////////////////////////////////////////////////
// CameraShot

struct AscomCameraShot {
    image:          AcImage,
    // Subframe origin, needed to derive the full-frame Bayer pattern
    start_x:        usize,
    start_y:        usize,
    sensor_type:    AcSensorType,
    bayer_offset:   Option<[u8; 2]>,
    dl_time:        f64,
    raw_image_info: RawImageInfo,
}

impl AscomCameraShot {
    fn new(
        image:          AcImage,
        start_x:        usize,
        start_y:        usize,
        sensor_type:    AcSensorType,
        bayer_offset:   Option<[u8; 2]>,
        dl_time:        f64,
        raw_image_info: RawImageInfo,
    ) -> Self {
        Self { image, start_x, start_y, sensor_type, bayer_offset, dl_time, raw_image_info }
    }

    fn pixel(&self, x: usize, y: usize) -> u16 {
        let value = self.image.pixel(x, y, 0).unwrap_or(0.0);
        value.round().clamp(0.0, u16::MAX as f64) as u16
    }

    /// Bayer pattern for the subframe position
    fn cfa_type(&self) -> eyre::Result<CfaType> {
        if self.image.planes != 1 {
            eyre::bail!("Image has {} planes", self.image.planes);
        }
        match self.sensor_type {
            AcSensorType::Monochrome =>
                Ok(CfaType::None),
            AcSensorType::RGGB if let Some(bayer_offset) = self.bayer_offset => {
                let x = (self.start_x + bayer_offset[0] as usize) % 2;
                let y = (self.start_y + bayer_offset[1] as usize) % 2;
                Ok(match (x, y) {
                    (0, 0) => CfaType::RGGB,
                    (1, 0) => CfaType::GRBG,
                    (0, 1) => CfaType::GBRG,
                    _ => CfaType::BGGR,
                })
            }
            // Defensive: activation defaults the offset, so this should not happen
            AcSensorType::RGGB =>
                eyre::bail!("RGGB sensor missing BayerOffset"),
            _ => eyre::bail!("Sensor type {:?} not supported", self.sensor_type),
        }
    }

    fn save_raw_file(&self, file_name: &Path) -> eyre::Result<()> {
        let raw_image = self.get_raw()?;
        save_raw_image_to_fits_file(&raw_image, file_name)
    }
}

impl CameraShot for AscomCameraShot {
    fn get_type(&self) -> CameraShotType {
        if self.image.planes == 1 {
            CameraShotType::RawCcdData
        } else {
            CameraShotType::ReadyImage
        }
    }

    fn get_raw(&self) -> eyre::Result<RawImage> {
        let mut info = self.raw_image_info.clone();
        info.cfa = self.cfa_type()?;

        let mut data = Vec::with_capacity(info.width * info.height);
        for y in 0..info.height {
            for x in 0..info.width {
                data.push(self.pixel(x, y));
            }
        }

        let cfa_array = info.cfa.get_array();
        Ok(RawImage::new(info, data, cfa_array))
    }

    fn get_image(&self, _image: &mut crate::image::image::Image) -> eyre::Result<()> {
        eyre::bail!("Color image is unimplemented for ASCOM drivers");
    }

    fn download_time(&self) -> f64 {
        self.dl_time
    }

    fn file_ext(&self) -> &str {
        match self.get_type() {
            CameraShotType::RawCcdData => "fits",
            CameraShotType::ReadyImage => "tif",
        }
    }

    fn save_to_file(&self, file_name: &Path) -> eyre::Result<()> {
        match self.get_type() {
            CameraShotType::RawCcdData =>
                self.save_raw_file(file_name),
            CameraShotType::ReadyImage =>
                eyre::bail!("Saving ReadyImage is not supported for ASCOM drivers"),
        }
    }
}

///////////////////////////////////////////////////////////////////////////////
// Camera

bitflags! {
    struct CameraFlags: u32 {
        const FRAME_SUPPORTED   = (1 << 0);
        const GAIN_SUPPORTED    = (1 << 1);
        const OFFSET_SUPPORTED  = (1 << 2);
        const BIN_SUPPORTED     = (1 << 3);
        const COOLER_SUPPORTED  = (1 << 4);
        const CAN_STOP_EXP      = (1 << 5);
        const CAN_ABORT_EXP     = (1 << 6);
        const CAN_GET_COOL_PWR  = (1 << 7);
        const CAN_GET_CCD_TEMP  = (1 << 8);
    }
}

/// ASCOM `ICamera` reports no CCD-temperature setpoint bounds (only
/// `SetCCDTemperature`, which rejects out-of-range values with
/// `InvalidValueException`). Conservative fallback range.
const DEFAULT_CCD_TEMP_RANGE: RangeInclusive<f64> = -50.0 ..= 50.0;

/// COM handle plus cached static data. Exists only while the device is active.
struct CameraStatic {
    device:       AcCamera,
    flags:        CameraFlags,
    exp_range:    RangeInclusive<f64>,
    gain_range:   RangeInclusive<f64>,
    offset_range: RangeInclusive<f64>,
    pixel_size_x: f64,
    pixel_size_y: f64,
    ccd_size_x:   usize,
    ccd_size_y:   usize,
    max_bin_x:    usize,
    max_bin_y:    usize,
    sensor_type:  AcSensorType,
    bayer_offset: Option<[u8; 2]>,
    max_value:    u16,
    cam_name:     String,
}

struct ExposureData {
    duration:   f64,
    start_time: std::time::Instant,
    // `CameraBeginDownloadData` was already sent for this exposure
    download_begun: bool,
}

#[derive(Default)]
struct CameraDynData {
    prev_temperature: Option<f64>,
    prev_cool_pwr:    Option<f64>,
    frame_type:       Option<FrameType>,
    exposure:         f64,
}

pub struct AscomCamera {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
    conn_key:    Arc<String>,
    state:       Mutex<ActState<CameraStatic>>,
    exp_data:    Mutex<Option<ExposureData>>,
    dyn_data:    Mutex<CameraDynData>,
    /// Replaces driver instantiation in tests; production always opens the ProgID.
    #[cfg(test)]
    open_override: Option<Box<dyn Fn() -> ascom::error::Result<AcCamera> + Send + Sync>>,
}

impl AscomCamera {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            conn_key:    Arc::new(conn_key_of(&info.id)),
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
            state:       Mutex::new(ActState::Idle),
            exp_data:    Mutex::new(None),
            dyn_data:    Mutex::new(CameraDynData::default()),
            #[cfg(test)]
            open_override: None,
        }
    }

    /// Instantiates the COM driver object for this wrapper's ProgID.
    fn open_driver(&self) -> ascom::error::Result<AcCamera> {
        #[cfg(test)]
        if let Some(open) = &self.open_override {
            return open();
        }
        AcCamera::open(&AcDeviceSpec::new(self.device_id.as_str()))
    }

    /// Wrapper whose driver comes from `open` instead of the ProgID: how the tests
    /// inject an in-process mock driver.
    #[cfg(test)]
    fn with_open_override(
        ctx: Arc<HalCtx>,
        info: DeviceInfo,
        open: impl Fn() -> ascom::error::Result<AcCamera> + Send + Sync + 'static,
    ) -> Self {
        let mut camera = Self::new(ctx, info);
        camera.open_override = Some(Box::new(open));
        camera
    }

    fn active_data(&self) -> eyre::Result<Arc<CameraStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Ok(Arc::clone(data)),
            _ => Err(not_connected(&self.device_id)),
        }
    }

    fn active_data_opt(&self) -> Option<Arc<CameraStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Some(Arc::clone(data)),
            _ => None,
        }
    }

    fn is_activated(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), ActState::Active(_))
    }

    /// Opens the COM object, connects (unless a ProgID alias of the same class
    /// is already connected), and caches capabilities and static data.
    /// Blocking: runs on the caller thread with no external locks held.
    fn activate_impl(&self, must_connect: bool) -> eyre::Result<CameraStatic> {
        let device = match ac_err(
            &format!("cannot open ASCOM camera {}", self.device_id),
            self.open_driver(),
        ) {
            Ok(device) => device,
            Err(err) => {
                // No handle exists to disconnect: only drop the acquired ref
                self.ctx.conn_release(&self.conn_key);
                return Err(err);
            }
        };

        if must_connect && let Err(err) = device.set_connected(true) {
            // The driver may have set `Connected` before failing: roll back
            rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
            eyre::bail!("cannot connect ASCOM camera {}: {err}", self.device_id);
        }

        // Capabilities are cached by the crate after connect. A failed snapshot
        // is not fatal: every unimplemented member then reads as `false`.
        let caps = match device.capabilities() {
            Ok(caps) => caps,
            Err(err) => {
                log::warn!("ASCOM camera {}: cannot read capabilities: {err}", self.device_id);
                Default::default()
            }
        };

        // Mandatory reads: on failure roll back the connection while the
        // handle is still alive (dropping it never disconnects)
        let (exp_min, exp_max, pixel_size_x, pixel_size_y, ccd_size_x, ccd_size_y) =
            (|| -> eyre::Result<(f64, f64, f64, f64, usize, usize)> {
                let exp_min = ac_err("ExposureMin", device.exposure_min())?;
                let exp_max = ac_err("ExposureMax", device.exposure_max())?;
                let pixel_size_x = ac_err("PixelSizeX", device.pixel_size_x())?;
                let pixel_size_y = ac_err("PixelSizeY", device.pixel_size_y())?;
                let ccd_size_x = i32::max(ac_err("CameraXSize", device.camera_x_size())?, 0) as usize;
                let ccd_size_y = i32::max(ac_err("CameraYSize", device.camera_y_size())?, 0) as usize;
                Ok((exp_min, exp_max, pixel_size_x, pixel_size_y, ccd_size_x, ccd_size_y))
            })()
            .map_err(|err| {
                rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
                err
            })?;

        let max_bin_x = usize::max(i32::max(device.max_bin_x().unwrap_or(1), 0) as usize, 1);
        let max_bin_y = usize::max(i32::max(device.max_bin_y().unwrap_or(1), 0) as usize, 1);
        let gain_supported = device.gain().is_ok();
        let gain_min = device.gain_min().unwrap_or(0) as f64;
        let gain_max = device.gain_max().unwrap_or(100_000) as f64;
        let offset_supported = device.offset().is_ok();
        let offset_min = device.offset_min().unwrap_or(0) as f64;
        let offset_max = device.offset_max().unwrap_or(65535) as f64;

        let sensor_type = device.sensor_type().unwrap_or(AcSensorType::Monochrome);
        // A pre-ASCOM-6 driver may leave BayerOffsetX/Y unimplemented: only that case
        // may be defaulted to 0. A real read failure (timeout, RPC) must not be masked.
        #[derive(Clone, Copy)]
        enum MemberRead { Value(i32), Absent, Fault }
        let read_offset = |member: &str, read: ascom::error::Result<i32>| -> MemberRead {
            match read {
                Ok(value) => MemberRead::Value(value),
                Err(err) if err.is_unsupported() => MemberRead::Absent,
                Err(err) => {
                    log::error!("ASCOM camera {}: cannot read {member}: {err}", self.device_id);
                    MemberRead::Fault
                }
            }
        };
        let offset_x = read_offset("BayerOffsetX", device.bayer_offset_x());
        let offset_y = read_offset("BayerOffsetY", device.bayer_offset_y());
        let value = |read: MemberRead| match read {
            MemberRead::Value(value) => Some(value),
            _ => None,
        };
        let rem2 = |value: i32| i32::rem_euclid(value, 2) as u8;
        let bayer_offset = match (offset_x, offset_y) {
            (MemberRead::Value(x), MemberRead::Value(y)) => Some([rem2(x), rem2(y)]),
            (MemberRead::Absent, MemberRead::Absent)
            | (MemberRead::Absent, MemberRead::Value(_))
            | (MemberRead::Value(_), MemberRead::Absent)
                if sensor_type == AcSensorType::RGGB =>
            {
                let offset = [
                    rem2(value(offset_x).unwrap_or(0)),
                    rem2(value(offset_y).unwrap_or(0)),
                ];
                let missing = match (offset_x, offset_y) {
                    (MemberRead::Absent, MemberRead::Absent) => "BayerOffsetX/Y",
                    (MemberRead::Absent, _) => "BayerOffsetX",
                    _ => "BayerOffsetY",
                };
                log::warn!(
                    "ASCOM camera {}: {missing} not reported by driver, assuming ({}, {})",
                    self.device_id, offset[0], offset[1]
                );
                Some(offset)
            }
            // A real read failure (or a non-Bayer sensor): leave the offset unknown
            _ => None,
        };

        let max_adu = device.max_adu().unwrap_or(-1);
        let max_value: u16 = if max_adu <= 0 {
            u16::MAX
        } else {
            usize::min(max_adu as usize, u16::MAX as usize) as u16
        };

        let cam_name = device.name().unwrap_or_else(|_| self.device_name.clone());
        let can_read_ccd_temp = device.ccd_temperature().is_ok();

        let mut flags = CameraFlags::empty();
        flags.set(CameraFlags::FRAME_SUPPORTED, true);
        flags.set(CameraFlags::GAIN_SUPPORTED, gain_supported);
        flags.set(CameraFlags::OFFSET_SUPPORTED, offset_supported);
        flags.set(CameraFlags::BIN_SUPPORTED, usize::min(max_bin_x, max_bin_y) > 1);
        flags.set(CameraFlags::COOLER_SUPPORTED, caps.supports("CanSetCCDTemperature"));
        flags.set(CameraFlags::CAN_STOP_EXP, caps.supports("CanStopExposure"));
        flags.set(CameraFlags::CAN_ABORT_EXP, caps.supports("CanAbortExposure"));
        flags.set(CameraFlags::CAN_GET_COOL_PWR, caps.supports("CanGetCoolerPower"));
        flags.set(CameraFlags::CAN_GET_CCD_TEMP, can_read_ccd_temp);

        if caps.supports("CanSetCCDTemperature") {
            log::warn!(
                "ASCOM camera {}: CCD temperature setpoint bounds not reported by driver \
                 (absent from the ASCOM ICamera interface), assuming {DEFAULT_CCD_TEMP_RANGE:?}",
                self.device_id
            );
        }

        Ok(CameraStatic {
            device,
            flags,
            exp_range: exp_min ..= exp_max,
            gain_range: gain_min ..= gain_max,
            offset_range: offset_min ..= offset_max,
            pixel_size_x,
            pixel_size_y,
            ccd_size_x,
            ccd_size_y,
            max_bin_x,
            max_bin_y,
            sensor_type,
            bayer_offset,
            max_value,
            cam_name,
        })
    }

    /// Ready events: exactly once per successful activation, outside any lock.
    fn send_ready_events(&self) {
        let device_id = Arc::clone(&self.device_id);
        self.ctx.event_handlers.send(HalEvent::CameraIsReadyToWork(Arc::clone(&device_id)));
        self.ctx.event_handlers.send(HalEvent::CameraIsReadyForCooling(Arc::clone(&device_id)));
        self.ctx.event_handlers.send(HalEvent::CameraOffsetCanBeControlled(Arc::clone(&device_id)));
        self.ctx.event_handlers.send(HalEvent::CameraGainCanBeControlled(device_id));
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let Some(st) = self.active_data_opt() else { return Ok(()); };

        let exp_status = {
            let exp_data = self.exp_data.lock().unwrap();
            exp_data.as_ref().map(|e| (e.duration, e.start_time))
        };

        if let Some((duration, start_time)) = exp_status {
            let elapsed = start_time.elapsed().as_secs_f64();
            let remaining = duration - elapsed;
            self.ctx.event_handlers.send(HalEvent::CameraTimeUntilEndOfExposure {
                device_id: Arc::clone(&self.device_id),
                time:      remaining.clamp(0.0, duration),
            });

            // One `ImageReady` read per tick
            match st.device.image_ready() {
                Ok(true) => {
                    if let Err(err) = self.get_image_and_send_event(&st) {
                        log::error!("ASCOM camera {}: cannot finish exposure: {err}", self.device_id);
                        *self.exp_data.lock().unwrap() = None;
                        return Err(err);
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    *self.exp_data.lock().unwrap() = None;
                    eyre::bail!("error waiting for end of exposure on ASCOM camera {}: {err}", self.device_id);
                }
            }
        } else {
            let mut data = self.dyn_data.lock().unwrap();

            if st.flags.contains(CameraFlags::CAN_GET_CCD_TEMP) {
                let temperature = st.device.ccd_temperature().ok();
                if data.prev_temperature != temperature && let Some(temperature) = temperature {
                    self.ctx.event_handlers.send(HalEvent::CameraCcdTempChanged {
                        device_id:   Arc::clone(&self.device_id),
                        temperature,
                    });
                }
                data.prev_temperature = temperature;
            }

            if st.flags.contains(CameraFlags::CAN_GET_COOL_PWR) {
                let cool_pwr = st.device.cooler_power().ok();
                if data.prev_cool_pwr != cool_pwr && let Some(cool_pwr) = cool_pwr {
                    self.ctx.event_handlers.send(HalEvent::CameraCoolerPwrChanged {
                        device_id: Arc::clone(&self.device_id),
                        // Classic reports a 0..1 fraction; the UI shows percents
                        // (as INDI does)
                        power:     cool_pwr * 100.0,
                    });
                }
                data.prev_cool_pwr = cool_pwr;
            }
        }

        Ok(())
    }

    fn get_image_and_send_event(&self, st: &CameraStatic) -> eyre::Result<()> {
        let (frame_type, exposure) = {
            let data = self.dyn_data.lock().unwrap();
            (data.frame_type, data.exposure)
        };

        // Send Begin exactly once per exposure, right before the read:
        // the `InvalidOperation` retry below re-enters this function on
        // the next tick and must not re-send it
        let first_read_attempt = {
            let mut exp_data = self.exp_data.lock().unwrap();
            match exp_data.as_mut() {
                Some(exp) if !exp.download_begun => {
                    exp.download_begun = true;
                    true
                }
                _ => false,
            }
        };
        if first_read_attempt {
            self.ctx.event_handlers.send(HalEvent::CameraBeginDownloadData(
                Arc::clone(&self.device_id)
            ));
        }

        let timer = std::time::Instant::now();
        let image = match st.device.read_image() {
            Ok(image) => image,
            Err(err) if err.kind == AscomErrorKind::InvalidOperation => {
                // `ImageReady` fell again before the read: retry next tick
                log::debug!("ASCOM camera {}: read_image not ready, will retry: {err}", self.device_id);
                return Ok(());
            }
            Err(err) => {
                eyre::bail!("ASCOM camera {}: ImageArray read failed: {err}", self.device_id);
            }
        };
        let dl_time = timer.elapsed().as_secs_f64();

        if image.wide_elements() {
            log::warn!("ASCOM camera {}: slow wide (VT_VARIANT) pixel path", self.device_id);
        }

        // `Image` carries no sub-frame origin: read it off the handle right away
        let start_x = i32::max(st.device.start_x().unwrap_or(0), 0) as usize;
        let start_y = i32::max(st.device.start_y().unwrap_or(0), 0) as usize;

        let mut info = RawImageInfo::default();
        info.width     = image.width;
        info.height    = image.height;
        info.max_value = st.max_value;
        info.frame_type = frame_type.unwrap_or(FrameType::Lights);
        info.camera    = st.cam_name.clone();
        info.gain      = st.device.gain().unwrap_or(0);
        info.offset    = st.device.offset().unwrap_or(0);
        info.bin       = st.device.bin_x().unwrap_or(1).clamp(1, u8::MAX as i32) as u8;
        info.ccd_temp  = st.device.ccd_temperature().ok();
        info.exposure  = exposure;

        let shot = AscomCameraShot::new(
            image,
            start_x,
            start_y,
            st.sensor_type,
            st.bayer_offset,
            dl_time,
            info,
        );

        *self.exp_data.lock().unwrap() = None;

        self.ctx.event_handlers.send(HalEvent::CameraShotResult {
            device_id: Arc::clone(&self.device_id),
            shot:      Arc::new(shot),
        });

        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
        let active = take_active(&mut self.state.lock().unwrap());
        if let Some(st) = active {
            *self.exp_data.lock().unwrap() = None;
            // Disconnect the shared class only when this was its last wrapper
            if self.ctx.conn_release(&self.conn_key) && let Err(err) = st.device.set_connected(false) {
                log::error!("cannot disconnect ASCOM camera {}: {err}", self.device_id);
            }
            // Drops the driver COM thread outside any lock (may block up to 5 s)
            drop(st);
            self.ctx.device_deactivated();
        }
        Ok(())
    }
}

impl Device for AscomCamera {
    fn id(&self) -> &str {
        &self.device_id
    }

    fn name(&self) -> &str {
        &self.device_name
    }

    // No COM reads here: called on every widget-state correction
    fn is_active(&self) -> eyre::Result<bool> {
        if self.exp_data.lock().unwrap().is_some() {
            return Ok(true);
        }
        Ok(self.is_activated())
    }

    // Blocks the caller thread while the driver connects (as INDI/Alpaca
    // Connect buttons do). Must be called with no `options`/`data` locks held.
    fn activate(&self) -> eyre::Result<()> {
        if !begin_activation(&mut self.state.lock().unwrap())? {
            return Ok(()); // already active: no duplicate Ready events
        }

        // Several ProgIDs may name one driver class: connect only as its first user
        let must_connect = self.ctx.conn_acquire(&self.conn_key);

        match self.activate_impl(must_connect) {
            Ok(st) => {
                *self.state.lock().unwrap() = ActState::Active(Arc::new(st));
                self.ctx.device_activated();
                self.send_ready_events();
                Ok(())
            }
            Err(err) => {
                // The connection ref was already released by `activate_impl`
                *self.state.lock().unwrap() = ActState::Idle;
                Err(err)
            }
        }
    }

    fn deactivate(&self) -> eyre::Result<()> {
        self.deactivate_impl()
    }
}

impl Camera for AscomCamera {
    fn features(&self) -> CameraFeatures {
        CameraFeatures::empty()
    }

    fn init_before_shot(&self) -> eyre::Result<()> {
        Ok(())
    }

    // Exposure

    fn exposure_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(self.active_data()?.exp_range.clone())
    }

    fn start_exposure(&self, duration: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        let light = self.dyn_data.lock().unwrap().frame_type
            .unwrap_or(FrameType::Lights) == FrameType::Lights;

        *self.exp_data.lock().unwrap() = Some(ExposureData {
            duration,
            start_time:     std::time::Instant::now(),
            download_begun: false,
        });

        let result = ac_err(
            "StartExposure",
            st.device.start_exposure(duration, light)
        );
        if result.is_err() {
            *self.exp_data.lock().unwrap() = None;
        } else {
            // Only remember the exposure of a successfully started shot
            self.dyn_data.lock().unwrap().exposure = duration;
        }
        result
    }

    fn abort_exposure(&self) -> eyre::Result<()> {
        let st = self.active_data()?;
        let mut result = eyre::Ok(());
        if st.flags.contains(CameraFlags::CAN_STOP_EXP) {
            result = ac_err("StopExposure", st.device.stop_exposure());
        } else if st.flags.contains(CameraFlags::CAN_ABORT_EXP) {
            result = ac_err("AbortExposure", st.device.abort_exposure());
        }
        *self.exp_data.lock().unwrap() = None;
        result
    }

    fn remaining_time(&self) -> Option<f64> {
        let exp_data = self.exp_data.lock().unwrap();
        exp_data.as_ref().map(|exp_data| {
            let elapsed = exp_data.start_time.elapsed().as_secs_f64();
            (exp_data.duration - elapsed).clamp(0.0, exp_data.duration)
        })
    }

    // Frame type

    fn set_frame_type(&self, frame_type: FrameType) -> eyre::Result<()> {
        self.dyn_data.lock().unwrap().frame_type = Some(frame_type);
        Ok(())
    }

    // Frame

    fn pixel_size_um(&self) -> eyre::Result<(f64, f64)> {
        let st = self.active_data()?;
        Ok((st.pixel_size_x, st.pixel_size_y))
    }

    fn is_frame_supported(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(CameraFlags::FRAME_SUPPORTED))
            .unwrap_or(false))
    }

    fn ccd_size(&self) -> eyre::Result<(usize, usize)> {
        let st = self.active_data()?;
        Ok((st.ccd_size_x, st.ccd_size_y))
    }

    fn set_frame(&self, x: usize, y: usize, width: usize, height: usize) -> eyre::Result<()> {
        let st = self.active_data()?;
        // The trait passes unbinned sensor pixels; ASCOM SubFrame counts binned
        // pixels, so convert using the binning the driver currently reports.
        let bin_x = st.device.bin_x().unwrap_or(1).max(1) as usize;
        let bin_y = st.device.bin_y().unwrap_or(1).max(1) as usize;
        let (bx, by, bw, bh) = super::unbinned_rect_to_binned(
            x, y, width, height, bin_x, bin_y, st.ccd_size_x, st.ccd_size_y
        );
        ac_err(
            "SetSubFrame",
            st.device.set_sub_frame(bx as i32, by as i32, bw as i32, bh as i32)
        )
    }

    // Gain

    fn is_gain_supported(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(CameraFlags::GAIN_SUPPORTED))
            .unwrap_or(false))
    }

    fn gain_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(self.active_data()?.gain_range.clone())
    }

    fn set_gain(&self, value: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SetGain", st.device.set_gain(value.round() as i32))
    }

    // Offset

    fn is_offset_supported(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(CameraFlags::OFFSET_SUPPORTED))
            .unwrap_or(false))
    }

    fn offset_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(self.active_data()?.offset_range.clone())
    }

    fn set_offset(&self, value: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SetOffset", st.device.set_offset(value.round() as i32))
    }

    // Bin

    fn is_binning_supported(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(CameraFlags::BIN_SUPPORTED))
            .unwrap_or(false))
    }

    fn max_binning(&self) -> eyre::Result<(usize, usize)> {
        let st = self.active_data()?;
        Ok((st.max_bin_x, st.max_bin_y))
    }

    fn set_binning(&self, bin_x: usize, bin_y: usize) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SetBinning", st.device.set_binning(bin_x as i32, bin_y as i32))
    }

    // Cooler

    fn is_cooler_supported(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(CameraFlags::COOLER_SUPPORTED))
            .unwrap_or(false))
    }

    fn temperature(&self) -> eyre::Result<f64> {
        let st = self.active_data()?;
        ac_err("CCDTemperature", st.device.ccd_temperature())
    }

    fn temperature_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        // `Err` before activation (IMPL_ASCOM 3.2). The ASCOM ICamera interface
        // exposes no Min/MaxCcdTemperature, so serve the documented fallback.
        let _st = self.active_data()?;
        Ok(DEFAULT_CCD_TEMP_RANGE)
    }

    fn set_temperature(&self, temperature: Option<f64>) -> eyre::Result<()> {
        let st = self.active_data()?;
        if let Some(temperature) = temperature {
            ac_err("SetCCDTemperature", st.device.set_target_ccd_temperature(temperature))?;
            ac_err("SetCoolerOn", st.device.set_cooler_on(true))?;
        } else {
            ac_err("SetCoolerOn", st.device.set_cooler_on(false))?;
        }
        Ok(())
    }

    // Heater

    fn is_heater_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn heater_ctrl_list(&self) -> eyre::Result<Vec<(String, String)>> {
        unimplemented!();
    }

    fn control_heater(&self, _id: &str) -> eyre::Result<()> {
        unimplemented!();
    }

    // Fan

    fn is_fan_ctrl_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn enable_fan(&self, _enable: bool) -> eyre::Result<()> {
        unimplemented!();
    }

    // Low noise mode

    fn is_low_noise_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn enable_low_noise_mode(&self, _enable: bool) -> eyre::Result<()> {
        unimplemented!();
    }

    // High fullwell mode

    fn is_high_fullwell_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn enable_high_fullwell_mode(&self, _enable: bool) -> eyre::Result<()> {
        unimplemented!();
    }

    // Conversion gain

    fn is_conversion_gain_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn conversion_gain_list(&self) -> eyre::Result<Vec<(String, String)>> {
        unimplemented!();
    }

    fn set_conversion_gain(&self, _id: &str) -> eyre::Result<()> {
        unimplemented!();
    }

    // Telescope

    fn set_telescope_focal_len(&self, _focal_len: f64) -> eyre::Result<()> {
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// Telescope (mount)

bitflags! {
    struct TelescopeFlags: u32 {
        const GUIDE_RATE_SUPPORTED = (1 << 0);
        const CAN_SET_GUIDE_RATE   = (1 << 1);
    }
}

/// COM handle plus cached data. Exists only while the device is active.
struct TelescopeStatic {
    device:     AcTelescope,
    move_rates: Vec<(String, f64)>,
    flags:      TelescopeFlags,
}

struct TelescopeData {
    ns_reverted:   bool,
    we_reverted:   bool,
    axis_rate:     f64,
    prev_state:    Option<TelescopeState>,
    prev_tracking: Option<bool>,
    prev_parked:   Option<bool>,
}

/// Flags are `None` when the corresponding COM read failed (unknown)
struct StateInternal {
    state:       TelescopeState,
    is_tracking: Option<bool>,
    is_parked:   Option<bool>,
}

pub struct AscomTelescope {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
    conn_key:    Arc<String>,
    state:       Mutex<ActState<TelescopeStatic>>,
    data:        Mutex<TelescopeData>,
}

impl Default for TelescopeData {
    fn default() -> Self {
        Self {
            ns_reverted:   false,
            we_reverted:   false,
            axis_rate:     1.0,
            prev_state:    None,
            prev_tracking: None,
            prev_parked:   None,
        }
    }
}

impl AscomTelescope {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            conn_key:    Arc::new(conn_key_of(&info.id)),
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
            state:       Mutex::new(ActState::Idle),
            data:        Mutex::new(TelescopeData::default()),
        }
    }

    fn active_data(&self) -> eyre::Result<Arc<TelescopeStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Ok(Arc::clone(data)),
            _ => Err(not_connected(&self.device_id)),
        }
    }

    fn active_data_opt(&self) -> Option<Arc<TelescopeStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Some(Arc::clone(data)),
            _ => None,
        }
    }

    fn is_activated(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), ActState::Active(_))
    }

    /// Opens the COM object, connects (unless a ProgID alias of the same class
    /// is already connected), and builds the move rate list.
    /// Blocking: runs on the caller thread with no external locks held.
    fn activate_impl(&self, must_connect: bool) -> eyre::Result<TelescopeStatic> {
        let device = match ac_err(
            &format!("cannot open ASCOM telescope {}", self.device_id),
            AcTelescope::open(&AcDeviceSpec::new(self.device_id.as_str()))
        ) {
            Ok(device) => device,
            Err(err) => {
                // No handle exists to disconnect: only drop the acquired ref
                self.ctx.conn_release(&self.conn_key);
                return Err(err);
            }
        };

        if must_connect && let Err(err) = device.set_connected(true) {
            // The driver may have set `Connected` before failing: roll back
            rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
            eyre::bail!("cannot connect ASCOM telescope {}: {err}", self.device_id);
        }

        let caps = match device.capabilities() {
            Ok(caps) => caps,
            Err(err) => {
                log::warn!("ASCOM telescope {}: cannot read capabilities: {err}", self.device_id);
                Default::default()
            }
        };

        let prim_axis_rates = device.axis_rates(AcTelescopeAxis::Primary).unwrap_or_default();
        let sec_axis_rates = device.axis_rates(AcTelescopeAxis::Secondary).unwrap_or_default();

        let prim_max_rate = prim_axis_rates.iter()
            .map(|range| range.maximum)
            .max_by(|x, y| f64::partial_cmp(x, y).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(SIDERAL_RATE_DEG_PER_SEC);
        let sec_max_rate = sec_axis_rates.iter()
            .map(|range| range.maximum)
            .max_by(|x, y| f64::partial_cmp(x, y).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(SIDERAL_RATE_DEG_PER_SEC);
        let max_rate = f64::min(prim_max_rate, sec_max_rate);

        let mut move_rates = Vec::new();
        for rate in [1, 5, 10, 25, 50, 100, 250, 500, 1000] {
            let rate_is_deg_in_sec = SIDERAL_RATE_DEG_PER_SEC * rate as f64;
            if rate_is_deg_in_sec >= 0.5 * max_rate {
                break;
            }
            move_rates.push((format!("x{rate}"), rate_is_deg_in_sec));
        }
        move_rates.push(("1/2 Max".to_string(), 0.5 * max_rate));
        move_rates.push(("Max".to_string(), max_rate));

        // Classic has no `CanPulseGuide`-like flag for reading guide rates:
        // probing the property is the only way to know
        let guide_rate_supported = device.guide_rate_right_ascension().is_ok();
        let can_set_guide_rate = caps.supports("CanSetGuideRates");

        let mut flags = TelescopeFlags::empty();
        flags.set(TelescopeFlags::GUIDE_RATE_SUPPORTED, guide_rate_supported);
        flags.set(TelescopeFlags::CAN_SET_GUIDE_RATE, can_set_guide_rate);

        Ok(TelescopeStatic { device, move_rates, flags })
    }

    /// Ready events: exactly once per successful activation, outside any lock.
    fn send_ready_events(&self) {
        self.ctx.event_handlers.send(HalEvent::TelescopeSlewRateListReady(
            Arc::clone(&self.device_id)
        ));
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let Some(st) = self.active_data_opt() else { return Ok(()); };

        let state = self.read_state(&st);

        let mut data = self.data.lock().unwrap();
        let state_changed = data.prev_state != Some(state.state);
        let tracking_changed = state.is_tracking.is_some_and(|v| data.prev_tracking != Some(v));
        let parked_changed = state.is_parked.is_some_and(|v| data.prev_parked != Some(v));
        data.prev_state = Some(state.state);
        // On a failed read keep the last real value: the next good read compares to it
        data.prev_tracking = state.is_tracking.or(data.prev_tracking);
        data.prev_parked = state.is_parked.or(data.prev_parked);
        drop(data);

        if state_changed {
            self.ctx.event_handlers.send(HalEvent::TelescopeStateChanged {
                device_id: Arc::clone(&self.device_id),
                state:     state.state,
            });
        }
        if tracking_changed && let Some(tracking) = state.is_tracking {
            self.ctx.event_handlers.send(HalEvent::TelescopeTrackingChanged {
                device_id: Arc::clone(&self.device_id),
                tracking,
            });
        }
        if parked_changed && let Some(is_parked) = state.is_parked {
            self.ctx.event_handlers.send(
                if is_parked {
                    HalEvent::TelescopeParked(Arc::clone(&self.device_id))
                } else {
                    HalEvent::TelescopeUnparked(Arc::clone(&self.device_id))
                }
            );
        }
        Ok(())
    }

    /// Reads every property on its own: a failed read yields `None`, and any
    /// unknown property makes the whole state `Error`.
    fn read_state(&self, st: &TelescopeStatic) -> StateInternal {
        let read_flag = |ctx: &str, res: ascom::error::Result<bool>| -> Option<bool> {
            res.map_err(|err| {
                log::debug!("ASCOM telescope {}: {ctx}: {err}", self.device_id);
            })
            .ok()
        };
        let is_tracking = read_flag("Tracking", st.device.tracking());
        let is_parked = read_flag("AtPark", st.device.at_park());
        let is_slewing = read_flag("Slewing", st.device.slewing());
        let is_pulse_guiding = read_flag("IsPulseGuiding", st.device.is_pulse_guiding());

        let state = if is_parked.is_none()
            || is_pulse_guiding.is_none()
            || is_slewing.is_none()
            || is_tracking.is_none()
        {
            TelescopeState::Error
        } else if is_parked == Some(true) {
            TelescopeState::Parked
        } else if is_pulse_guiding == Some(true) {
            TelescopeState::Correction
        } else if is_slewing == Some(true) {
            TelescopeState::Slewing
        } else if is_tracking == Some(true) {
            TelescopeState::Tracking
        } else {
            TelescopeState::Stopped
        };
        StateInternal { state, is_tracking, is_parked }
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
        let active = take_active(&mut self.state.lock().unwrap());
        if let Some(st) = active {
            // Disconnect the shared class only when this was its last wrapper
            if self.ctx.conn_release(&self.conn_key) && let Err(err) = st.device.set_connected(false) {
                log::error!("cannot disconnect ASCOM telescope {}: {err}", self.device_id);
            }
            // Drops the driver COM thread outside any lock (may block up to 5 s)
            drop(st);
            self.ctx.device_deactivated();
        }
        Ok(())
    }
}

impl Device for AscomTelescope {
    fn id(&self) -> &str {
        &self.device_id
    }

    fn name(&self) -> &str {
        &self.device_name
    }

    // No COM reads here: called on every widget-state correction
    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.is_activated())
    }

    fn activate(&self) -> eyre::Result<()> {
        if !begin_activation(&mut self.state.lock().unwrap())? {
            return Ok(()); // already active: no duplicate Ready events
        }

        // Several ProgIDs may name one driver class: connect only as its first user
        let must_connect = self.ctx.conn_acquire(&self.conn_key);

        match self.activate_impl(must_connect) {
            Ok(st) => {
                *self.state.lock().unwrap() = ActState::Active(Arc::new(st));
                self.ctx.device_activated();
                self.send_ready_events();
                Ok(())
            }
            Err(err) => {
                // The connection ref was already released by `activate_impl`
                *self.state.lock().unwrap() = ActState::Idle;
                Err(err)
            }
        }
    }

    fn deactivate(&self) -> eyre::Result<()> {
        self.deactivate_impl()
    }
}

impl Telescope for AscomTelescope {
    fn state(&self) -> eyre::Result<TelescopeState> {
        let st = self.active_data()?;
        let state = self.read_state(&st);
        if state.state == TelescopeState::Error {
            eyre::bail!("ASCOM telescope {}: cannot read state", self.device_id);
        }
        Ok(state.state)
    }

    fn site(&self) -> eyre::Result<TelescopeSite> {
        let st = self.active_data()?;
        let latitude = ac_err("SiteLatitude", st.device.site_latitude())?;
        let longitude = ac_err("SiteLongitude", st.device.site_longitude())?;
        let elevation = ac_err("SiteElevation", st.device.site_elevation())?;
        Ok(TelescopeSite { latitude, longitude, elevation })
    }

    fn is_abort_motion_supported(&self) -> bool {
        true
    }

    fn abort_motion(&self) -> eyre::Result<()> {
        let st = self.active_data()?;
        // Drivers report NotImplemented for axes they do not have
        _ = st.device.abort_slew();
        _ = st.device.move_axis(AcTelescopeAxis::Primary, 0.0);
        _ = st.device.move_axis(AcTelescopeAxis::Secondary, 0.0);
        _ = st.device.move_axis(AcTelescopeAxis::Tertiary, 0.0);
        Ok(())
    }

    fn is_parked(&self) -> eyre::Result<bool> {
        let st = self.active_data()?;
        ac_err("AtPark", st.device.at_park())
    }

    fn park(&self) -> eyre::Result<()> {
        let st = self.active_data()?;
        // Async: the park motion completes on its own (state comes from the tick)
        ac_err("Park", st.device.park_async())
    }

    fn unpark(&self) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("Unpark", st.device.unpark())
    }

    fn is_tracking(&self) -> eyre::Result<bool> {
        let st = self.active_data()?;
        ac_err("Tracking", st.device.tracking())
    }

    fn track(&self, enabled: bool) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SetTracking", st.device.set_tracking(enabled))
    }

    fn revert_motion(&self, reverse_ns: bool, reverse_we: bool) -> eyre::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.ns_reverted = reverse_ns;
        data.we_reverted = reverse_we;
        Ok(())
    }

    fn move_(&self, direction: TelescopeMoveDir) -> eyre::Result<()> {
        let st = self.active_data()?;

        let (axis_rate, ns_reverted, we_reverted) = {
            let data = self.data.lock().unwrap();
            (data.axis_rate, data.ns_reverted, data.we_reverted)
        };

        let move_prim_axis = |rate: f64| -> eyre::Result<()> {
            let rate = if we_reverted { -rate } else { rate };
            ac_err("MoveAxis(Primary)", st.device.move_axis(AcTelescopeAxis::Primary, rate))
        };

        let move_sec_axis = |rate: f64| -> eyre::Result<()> {
            let rate = if ns_reverted { -rate } else { rate };
            ac_err("MoveAxis(Secondary)", st.device.move_axis(AcTelescopeAxis::Secondary, rate))
        };

        match direction {
            TelescopeMoveDir::North => move_sec_axis(axis_rate)?,
            TelescopeMoveDir::South => move_sec_axis(-axis_rate)?,
            TelescopeMoveDir::West => move_prim_axis(axis_rate)?,
            TelescopeMoveDir::East => move_prim_axis(-axis_rate)?,
            TelescopeMoveDir::NorthWest => {
                move_sec_axis(axis_rate)?;
                move_prim_axis(axis_rate)?;
            }
            TelescopeMoveDir::NorthEast => {
                move_sec_axis(axis_rate)?;
                move_prim_axis(-axis_rate)?;
            }
            TelescopeMoveDir::SouthWest => {
                move_sec_axis(-axis_rate)?;
                move_prim_axis(axis_rate)?;
            }
            TelescopeMoveDir::SouthEast => {
                move_sec_axis(-axis_rate)?;
                move_prim_axis(-axis_rate)?;
            }
        }
        Ok(())
    }

    fn slew_speed_list(&self) -> eyre::Result<Vec<(String, String)>> {
        let st = self.active_data()?;
        let list = st.move_rates
            .iter()
            .map(|(name, _)| (name.to_string(), name.to_string()))
            .collect();
        Ok(list)
    }

    fn set_slew_speed(&self, speed_id: &str) -> eyre::Result<()> {
        let st = self.active_data()?;
        let Some((_, rate)) = st.move_rates.iter().find(|(name, _)| name == speed_id) else {
            eyre::bail!("unknown slew speed id {speed_id}");
        };
        let rate = *rate;
        self.data.lock().unwrap().axis_rate = rate;
        Ok(())
    }

    fn eq_coord(&self) -> eyre::Result<(f64, f64)> {
        let st = self.active_data()?;
        let ra = ac_err("RightAscension", st.device.right_ascension())?;
        let dec = ac_err("Declination", st.device.declination())?;
        Ok((ra, dec))
    }

    fn goto_and_track(&self, ra: f64, dec: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SlewToCoordinatesAsync", st.device.slew_to_coordinates_async(ra, dec))
    }

    fn is_slewing(&self) -> eyre::Result<bool> {
        let st = self.active_data()?;
        ac_err("Slewing", st.device.slewing())
    }

    fn sync(&self, ra: f64, dec: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SyncToCoordinates", st.device.sync_to_coordinates(ra, dec))
    }

    fn is_guide_rate_supported(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(TelescopeFlags::GUIDE_RATE_SUPPORTED))
            .unwrap_or(false))
    }

    // Classic guide rates are fractions of sidereal rate already
    fn guide_rate(&self) -> eyre::Result<(f64, f64)> {
        let st = self.active_data()?;
        let ns = ac_err("GuideRateDeclination", st.device.guide_rate_declination())?;
        let we = ac_err("GuideRateRightAscension", st.device.guide_rate_right_ascension())?;
        Ok((ns, we))
    }

    fn pulse_max_duration(&self) -> eyre::Result<(f64, f64)> {
        Ok((3000.0, 3000.0))
    }

    fn can_set_guide_rate(&self) -> eyre::Result<bool> {
        Ok(self.active_data().map(|st| st.flags.contains(TelescopeFlags::CAN_SET_GUIDE_RATE))
            .unwrap_or(false))
    }

    fn set_guide_rate(&self, rate_ns: f64, rate_we: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        ac_err("SetGuideRateDeclination", st.device.set_guide_rate_declination(rate_ns))?;
        ac_err("SetGuideRateRightAscension", st.device.set_guide_rate_right_ascension(rate_we))
    }

    fn pulse_guide(&self, duration_ns: f64, duration_we: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        if duration_ns != 0.0 {
            let dir = if duration_ns < 0.0 {
                AcGuideDirection::North
            } else {
                AcGuideDirection::South
            };
            let duration = f64::abs(duration_ns) as i32;
            ac_err("PulseGuide", st.device.pulse_guide(dir, duration))?;
        }
        if duration_we != 0.0 {
            let dir = if duration_we < 0.0 {
                AcGuideDirection::West
            } else {
                AcGuideDirection::East
            };
            let duration = f64::abs(duration_we) as i32;
            ac_err("PulseGuide", st.device.pulse_guide(dir, duration))?;
        }
        Ok(())
    }

    fn is_pulse_guiding(&self) -> eyre::Result<bool> {
        let st = self.active_data()?;
        ac_err("IsPulseGuiding", st.device.is_pulse_guiding())
    }
}

///////////////////////////////////////////////////////////////////////////////
// Focuser

/// COM handle plus cached data. Exists only while the device is active.
struct FocuserStatic {
    device:   AcFocuser,
    absolute: bool,
    max_step: i32,
}

#[derive(Default)]
struct FocuserData {
    prev_state: Option<FocuserState>,
    prev_pos:   Option<i32>,
    prev_temp:  Option<f64>,
}

pub struct AscomFocuser {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
    conn_key:    Arc<String>,
    state:       Mutex<ActState<FocuserStatic>>,
    data:        Mutex<FocuserData>,
}

impl AscomFocuser {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            conn_key:    Arc::new(conn_key_of(&info.id)),
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
            state:       Mutex::new(ActState::Idle),
            data:        Mutex::new(FocuserData::default()),
        }
    }

    fn active_data(&self) -> eyre::Result<Arc<FocuserStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Ok(Arc::clone(data)),
            _ => Err(not_connected(&self.device_id)),
        }
    }

    fn active_data_opt(&self) -> Option<Arc<FocuserStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Some(Arc::clone(data)),
            _ => None,
        }
    }

    fn is_activated(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), ActState::Active(_))
    }

    /// Blocking: runs on the caller thread with no external locks held.
    fn activate_impl(&self, must_connect: bool) -> eyre::Result<FocuserStatic> {
        let device = match ac_err(
            &format!("cannot open ASCOM focuser {}", self.device_id),
            AcFocuser::open(&AcDeviceSpec::new(self.device_id.as_str()))
        ) {
            Ok(device) => device,
            Err(err) => {
                // No handle exists to disconnect: only drop the acquired ref
                self.ctx.conn_release(&self.conn_key);
                return Err(err);
            }
        };

        if must_connect && let Err(err) = device.set_connected(true) {
            // The driver may have set `Connected` before failing: roll back
            rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
            eyre::bail!("cannot connect ASCOM focuser {}: {err}", self.device_id);
        }

        let absolute = ac_err("Absolute", device.absolute())
            .map_err(|err| {
                // Roll back the connection while the handle is still alive
                rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
                err
            })?;
        if !absolute {
            log::warn!(
                "ASCOM focuser {}: Absolute == false, absolute positions emulated as deltas",
                self.device_id
            );
        }
        let max_step = device.max_step().unwrap_or(0);

        Ok(FocuserStatic { device, absolute, max_step })
    }

    /// Ready events: exactly once per successful activation, outside any lock.
    fn send_ready_events(&self) {
        let Some(st) = self.active_data_opt() else { return };
        if let Ok(abs_value) = st.device.position() {
            self.ctx.event_handlers.send(HalEvent::FocuserAbsValueCanBeControlled {
                device_id: Arc::clone(&self.device_id),
                abs_value: abs_value as f64,
            });
        }
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let Some(st) = self.active_data_opt() else { return Ok(()); };

        let state = match st.device.is_moving() {
            Ok(true)  => FocuserState::Moving,
            Ok(false) => FocuserState::Stopped,
            Err(_)    => FocuserState::Error,
        };
        // A failed read stays `None`: never fabricate a value for the UI
        let pos = st.device.position().ok();
        let temperature = st.device.temperature().ok();

        let mut data = self.data.lock().unwrap();
        let state_changed = data.prev_state != Some(state);
        let pos_changed = pos.is_some_and(|v| data.prev_pos != Some(v));
        let temp_changed = temperature.is_some_and(|v| data.prev_temp != Some(v));
        data.prev_state = Some(state);
        // On a failed read keep the last real value: the next good read compares to it
        data.prev_pos = pos.or(data.prev_pos);
        data.prev_temp = temperature.or(data.prev_temp);
        drop(data);

        if state_changed {
            self.ctx.event_handlers.send(HalEvent::FocuserStateChanged {
                device_id: Arc::clone(&self.device_id),
                state,
            });
        }
        if pos_changed && let Some(pos) = pos {
            self.ctx.event_handlers.send(HalEvent::FocuserAbsValueChanged {
                device_id: Arc::clone(&self.device_id),
                abs_value: pos as f64,
            });
        }
        if temp_changed && let Some(temperature) = temperature {
            self.ctx.event_handlers.send(HalEvent::FocuserTemperatureChanged {
                device_id:   Arc::clone(&self.device_id),
                temperature,
            });
        }

        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
        let active = take_active(&mut self.state.lock().unwrap());
        if let Some(st) = active {
            *self.data.lock().unwrap() = FocuserData::default();
            // Disconnect the shared class only when this was its last wrapper
            if self.ctx.conn_release(&self.conn_key) && let Err(err) = st.device.set_connected(false) {
                log::error!("cannot disconnect ASCOM focuser {}: {err}", self.device_id);
            }
            // Drops the driver COM thread outside any lock (may block up to 5 s)
            drop(st);
            self.ctx.device_deactivated();
        }
        Ok(())
    }
}

impl Device for AscomFocuser {
    fn id(&self) -> &str {
        &self.device_id
    }

    fn name(&self) -> &str {
        &self.device_name
    }

    // No COM reads here: called on every widget-state correction
    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.is_activated())
    }

    fn activate(&self) -> eyre::Result<()> {
        if !begin_activation(&mut self.state.lock().unwrap())? {
            return Ok(()); // already active: no duplicate Ready events
        }

        // Several ProgIDs may name one driver class: connect only as its first user
        let must_connect = self.ctx.conn_acquire(&self.conn_key);

        match self.activate_impl(must_connect) {
            Ok(st) => {
                *self.state.lock().unwrap() = ActState::Active(Arc::new(st));
                self.ctx.device_activated();
                self.send_ready_events();
                Ok(())
            }
            Err(err) => {
                // The connection ref was already released by `activate_impl`
                *self.state.lock().unwrap() = ActState::Idle;
                Err(err)
            }
        }
    }

    fn deactivate(&self) -> eyre::Result<()> {
        self.deactivate_impl()
    }
}

impl Focuser for AscomFocuser {
    fn state(&self) -> eyre::Result<FocuserState> {
        let st = self.active_data()?;
        Ok(match st.device.is_moving() {
            Ok(true)  => FocuserState::Moving,
            Ok(false) => FocuserState::Stopped,
            Err(_)    => FocuserState::Error,
        })
    }

    fn abs_position_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        let st = self.active_data()?;
        Ok(0.0 ..= st.max_step as f64)
    }

    fn abs_position(&self) -> eyre::Result<f64> {
        let st = self.active_data()?;
        Ok(ac_err("Position", st.device.position())? as f64)
    }

    fn set_abs_position(&self, value: f64) -> eyre::Result<()> {
        let st = self.active_data()?;
        let target = value.round() as i32;

        if st.absolute {
            return ac_err("Move", st.device.move_to(target));
        }

        // Relative focuser: `Move` argument is a delta. Delta computed in i64 to
        // avoid i32 overflow; the crate validates it against live MaxIncrement.
        let pos = ac_err("Position", st.device.position())?;
        let Some(delta) = i64::from(target)
            .checked_sub(i64::from(pos))
            .and_then(|delta| i32::try_from(delta).ok())
        else {
            eyre::bail!(
                "ASCOM focuser {}: cannot move {pos} -> {target}: delta is out of range",
                self.device_id
            );
        };
        ac_err("Move", st.device.move_to(delta))
    }

    fn temperature(&self) -> eyre::Result<f64> {
        let st = self.active_data()?;
        ac_err("Temperature", st.device.temperature())
    }
}

///////////////////////////////////////////////////////////////////////////////
// Filter wheel

/// COM handle plus cached filter names. Exists only while the device is active.
struct FilterWheelStatic {
    device: AcFilterWheel,
    names:  Vec<String>,
}

pub struct AscomFilterWheel {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
    conn_key:    Arc<String>,
    state:       Mutex<ActState<FilterWheelStatic>>,
    // Previously reported slot (None == moving/unknown)
    data:        Mutex<Option<usize>>,
}

impl AscomFilterWheel {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            conn_key:    Arc::new(conn_key_of(&info.id)),
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
            state:       Mutex::new(ActState::Idle),
            data:        Mutex::new(None),
        }
    }

    fn active_data(&self) -> eyre::Result<Arc<FilterWheelStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Ok(Arc::clone(data)),
            _ => Err(not_connected(&self.device_id)),
        }
    }

    fn active_data_opt(&self) -> Option<Arc<FilterWheelStatic>> {
        let state = self.state.lock().unwrap();
        match &*state {
            ActState::Active(data) => Some(Arc::clone(data)),
            _ => None,
        }
    }

    fn is_activated(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), ActState::Active(_))
    }

    /// Blocking: runs on the caller thread with no external locks held.
    fn activate_impl(&self, must_connect: bool) -> eyre::Result<FilterWheelStatic> {
        let device = match ac_err(
            &format!("cannot open ASCOM filter wheel {}", self.device_id),
            AcFilterWheel::open(&AcDeviceSpec::new(self.device_id.as_str()))
        ) {
            Ok(device) => device,
            Err(err) => {
                // No handle exists to disconnect: only drop the acquired ref
                self.ctx.conn_release(&self.conn_key);
                return Err(err);
            }
        };

        if must_connect && let Err(err) = device.set_connected(true) {
            // The driver may have set `Connected` before failing: roll back
            rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
            eyre::bail!("cannot connect ASCOM filter wheel {}: {err}", self.device_id);
        }

        let names = ac_err("Names", device.names())
            .map_err(|err| {
                // Roll back the connection while the handle is still alive
                rollback_activation(&self.ctx, &device, &self.conn_key, &self.device_id);
                err
            })?;

        Ok(FilterWheelStatic { device, names })
    }

    /// Ready events: exactly once per successful activation, outside any lock.
    fn send_ready_events(&self) {
        self.ctx.event_handlers.send(HalEvent::FilterWheelNameChanged(
            Arc::clone(&self.device_id)
        ));
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let Some(st) = self.active_data_opt() else { return Ok(()); };

        // `MOVING` (-1) reads as `None` (the wheel is turning)
        let pos = ac_slot_of(st.device.position().unwrap_or(-1)).map(|slot| slot as usize);

        let mut prev = self.data.lock().unwrap();
        let pos_changed = *prev != pos;
        *prev = pos;
        drop(prev);

        if pos_changed {
            self.ctx.event_handlers.send(HalEvent::FilterWheelSlotChange {
                device_id: Arc::clone(&self.device_id),
                slot:      pos.map(|slot| slot as i32),
            });
        }

        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
        let active = take_active(&mut self.state.lock().unwrap());
        if let Some(st) = active {
            *self.data.lock().unwrap() = None;
            // Disconnect the shared class only when this was its last wrapper
            if self.ctx.conn_release(&self.conn_key) && let Err(err) = st.device.set_connected(false) {
                log::error!("cannot disconnect ASCOM filter wheel {}: {err}", self.device_id);
            }
            // Drops the driver COM thread outside any lock (may block up to 5 s)
            drop(st);
            self.ctx.device_deactivated();
        }
        Ok(())
    }
}

impl Device for AscomFilterWheel {
    fn id(&self) -> &str {
        &self.device_id
    }

    fn name(&self) -> &str {
        &self.device_name
    }

    // No COM reads here: called on every widget-state correction
    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.is_activated())
    }

    fn activate(&self) -> eyre::Result<()> {
        if !begin_activation(&mut self.state.lock().unwrap())? {
            return Ok(()); // already active: no duplicate Ready events
        }

        // Several ProgIDs may name one driver class: connect only as its first user
        let must_connect = self.ctx.conn_acquire(&self.conn_key);

        match self.activate_impl(must_connect) {
            Ok(st) => {
                *self.state.lock().unwrap() = ActState::Active(Arc::new(st));
                self.ctx.device_activated();
                self.send_ready_events();
                Ok(())
            }
            Err(err) => {
                // The connection ref was already released by `activate_impl`
                *self.state.lock().unwrap() = ActState::Idle;
                Err(err)
            }
        }
    }

    fn deactivate(&self) -> eyre::Result<()> {
        self.deactivate_impl()
    }
}

impl FilterWheel for AscomFilterWheel {
    fn list_and_active(&self) -> eyre::Result<(Vec<String>, usize)> {
        let st = self.active_data()?;
        let pos = ac_err("Position", st.device.position())?;
        let pos = ac_slot_of(pos)
            .ok_or_else(|| eyre::eyre!("Position is not accessible now"))? as usize;
        Ok((st.names.clone(), pos))
    }

    fn set_active(&self, active_elem: usize) -> eyre::Result<()> {
        let st = self.active_data()?;

        if ac_slot_of(st.device.position().unwrap_or(-1)) == Some(active_elem as i32) {
            return Ok(());
        }

        ac_err("SetPosition", st.device.set_position(active_elem as i32))?;

        self.ctx.event_handlers.send(HalEvent::FilterWheelSlotChange {
            device_id: Arc::clone(&self.device_id),
            slot:      None,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> HalCtx {
        HalCtx {
            event_handlers: Arc::new(HalEventHandlers::new()),
            active_count:   AtomicUsize::new(0),
            conn_refs:      Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn only_the_first_wrapper_of_a_class_must_connect() {
        let ctx = test_ctx();
        assert!(ctx.conn_acquire("clsid"));    // first wrapper: must connect
        assert!(!ctx.conn_acquire("clsid"));   // alias wrapper: already connected
        assert!(!ctx.conn_release("clsid"));   // first leaves: alias still active
        assert!(ctx.conn_release("clsid"));    // last leaves: must disconnect
    }

    #[test]
    fn releasing_an_unregistered_class_does_nothing() {
        let ctx = test_ctx();
        assert!(!ctx.conn_release("unknown"));
    }

    #[test]
    fn different_classes_are_independent() {
        let ctx = test_ctx();
        assert!(ctx.conn_acquire("clsid_a"));
        assert!(ctx.conn_acquire("clsid_b"));
        assert!(ctx.conn_release("clsid_a"));
        assert!(ctx.conn_release("clsid_b"));
    }

    #[test]
    fn idle_activation_begins_and_latches_busy() {
        let mut state: ActState<u32> = ActState::Idle;
        assert!(begin_activation(&mut state).expect("Idle must accept an activation"));
        assert!(matches!(state, ActState::Busy), "beginning must latch Busy");
    }

    #[test]
    fn concurrent_activation_is_refused_while_busy() {
        let mut state: ActState<u32> = ActState::Busy;
        let err = begin_activation(&mut state).expect_err("Busy must refuse an activation");
        assert!(
            err.to_string().contains("activation in progress"),
            "the error does not explain the busy state: {err}"
        );
        assert!(matches!(state, ActState::Busy), "a refused begin changed the state");
    }

    #[test]
    fn activating_an_active_device_is_a_noop() {
        // `activate()` uses `Ok(false)` to skip both the work and the Ready events
        let mut state: ActState<u32> = ActState::Active(Arc::new(7));
        assert!(!begin_activation(&mut state).expect("Active must be reported as already done"));
        assert!(matches!(&state, ActState::Active(data) if **data == 7), "the cache was lost");
    }

    #[test]
    fn take_active_returns_data_and_resets_to_idle() {
        let data = Arc::new(7u32);
        let mut state: ActState<u32> = ActState::Active(Arc::clone(&data));
        let taken = take_active(&mut state).expect("Active must hand out its payload");
        assert!(Arc::ptr_eq(&taken, &data), "a different payload came out");
        assert!(matches!(state, ActState::Idle), "taking the payload must leave Idle");
    }

    #[test]
    fn take_active_preserves_busy_and_idle() {
        // Deactivating while an activation is in flight must not clobber Busy
        let mut busy: ActState<u32> = ActState::Busy;
        assert!(take_active(&mut busy).is_none(), "Busy has no payload to hand out");
        assert!(matches!(busy, ActState::Busy), "Busy was clobbered");

        let mut idle: ActState<u32> = ActState::Idle;
        assert!(take_active(&mut idle).is_none());
        assert!(matches!(idle, ActState::Idle), "Idle was changed");
    }

    #[test]
    fn failed_activation_returns_to_idle() {
        // The transition pair `activate()` performs when `activate_impl` fails
        let mut state: ActState<u32> = ActState::Idle;
        assert!(begin_activation(&mut state).expect("the first attempt must be accepted"));
        state = ActState::Idle;
        assert!(
            begin_activation(&mut state).expect("a failed activation must be retryable"),
            "the second attempt was refused"
        );
    }

    // --- Activation against an injected in-process mock driver -------------------

    use ascom::mock::{Element, HRESULT, Member};
    use std::sync::atomic::AtomicBool;

    /// An ASCOM exception HRESULT, e.g. code `0x402` = ValueNotSet.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    /// A mock camera answering everything `activate_impl` needs. A member left out of
    /// the table answers `DISP_E_UNKNOWNNAME` ("not implemented"), which is exactly
    /// what the tolerant HAL reads expect.
    fn mock_camera(connected: Arc<AtomicBool>) -> Vec<(&'static str, Member)> {
        vec![
            ("Connected", Member::Flag(connected)),
            // Identity members: mandatory for the capability snapshot
            ("InterfaceVersion", Member::Value(Element::Int(3))),
            ("Name", Member::Value(Element::Str("Mock Camera"))),
            ("Description", Member::Value(Element::Str("In-process mock camera"))),
            ("DriverInfo", Member::Value(Element::Str("astra_lite tests"))),
            ("DriverVersion", Member::Value(Element::Str("1.0"))),
            // Mandatory activation reads
            ("ExposureMin", Member::Value(Element::Int(1))),
            ("ExposureMax", Member::Value(Element::Int(600))),
            ("PixelSizeX", Member::Value(Element::Int(4))),
            ("PixelSizeY", Member::Value(Element::Int(4))),
            ("CameraXSize", Member::Value(Element::Int(4000))),
            ("CameraYSize", Member::Value(Element::Int(3000))),
            // Reads the HAL can do without
            ("MaxBinX", Member::Value(Element::Int(4))),
            ("MaxBinY", Member::Value(Element::Int(4))),
            ("SensorType", Member::Value(Element::Int(0))),
            ("MaxADU", Member::Value(Element::Int(65535))),
        ]
    }

    /// Turns one member of a fixture into a driver exception.
    fn refuse(members: &mut [(&'static str, Member)], name: &str, scode: HRESULT) {
        let (_, answer) = members
            .iter_mut()
            .find(|(member, _)| *member == name)
            .expect("the fixture carries that member");
        *answer = Member::Refuses(scode);
    }

    /// Wrapper whose driver is a fresh mock built on every open. One fake ProgID keeps
    /// both aliases on one `conn_key`, because no CLSID resolves for it.
    fn mock_camera_wrapper(ctx: Arc<HalCtx>, members: Vec<(&'static str, Member)>) -> AscomCamera {
        let info = DeviceInfo {
            id:    "Astra.Mock.Camera".to_string(),
            name:  "Mock Camera".to_string(),
            type_: DeviceType::CAMERA,
        };
        AscomCamera::with_open_override(ctx, info, move || {
            AcCamera::open_mock(members.clone(), "Astra.Mock.Camera")
        })
    }

    /// Total live connection refs over every COM class.
    fn conn_total(ctx: &HalCtx) -> usize {
        ctx.conn_refs.lock().unwrap().values().sum()
    }

    #[test]
    fn failed_activation_disconnects_the_driver() {
        let ctx = Arc::new(test_ctx());
        let connected = Arc::new(AtomicBool::new(false));
        let mut members = mock_camera(Arc::clone(&connected));
        refuse(&mut members, "CameraXSize", ascom_hr(0x402));
        let camera = mock_camera_wrapper(Arc::clone(&ctx), members);

        let err =
            camera.activate().expect_err("a refused mandatory member must fail activation");
        assert!(err.to_string().contains("CameraXSize"), "the failing member is lost: {err}");

        // The connect that already happened has to be rolled back
        assert!(!connected.load(Ordering::SeqCst), "the driver was left connected");
        assert_eq!(ctx.active_count.load(Ordering::Relaxed), 0, "a failure activated a device");
        assert_eq!(conn_total(&ctx), 0, "the connection ref was not released");
        assert!(!camera.is_activated(), "the wrapper claims to be active");
    }

    #[test]
    fn activation_is_retryable_after_failure() {
        let ctx = Arc::new(test_ctx());
        let connected = Arc::new(AtomicBool::new(false));
        let mut members = mock_camera(Arc::clone(&connected));
        refuse(&mut members, "CameraYSize", ascom_hr(0x402));
        let camera = mock_camera_wrapper(Arc::clone(&ctx), members);

        let err = camera.activate().expect_err("the first attempt must fail");
        assert!(err.to_string().contains("CameraYSize"), "the failing member is lost: {err}");
        let err = camera.activate().expect_err("the second attempt must fail the same way");
        assert!(err.to_string().contains("CameraYSize"), "the retry lost the member: {err}");

        assert!(!connected.load(Ordering::SeqCst), "a retry left the driver connected");
        assert_eq!(conn_total(&ctx), 0, "a retry leaked a connection ref");
        assert_eq!(ctx.active_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn successful_activation_connects_and_deactivate_disconnects() {
        let ctx = Arc::new(test_ctx());
        let connected = Arc::new(AtomicBool::new(false));
        let camera = mock_camera_wrapper(Arc::clone(&ctx), mock_camera(Arc::clone(&connected)));

        camera.activate().expect("a cooperative mock camera must activate");
        assert!(connected.load(Ordering::SeqCst), "activation never connected the driver");
        assert_eq!(ctx.active_count.load(Ordering::Relaxed), 1);
        assert!(camera.is_activated());
        assert_eq!(conn_total(&ctx), 1, "exactly one connection ref must be live");

        camera.deactivate_impl().expect("deactivation must succeed");
        assert!(!connected.load(Ordering::SeqCst), "deactivation left the driver connected");
        assert_eq!(ctx.active_count.load(Ordering::Relaxed), 0);
        assert!(!camera.is_activated());
        assert_eq!(conn_total(&ctx), 0);
    }

    #[test]
    fn alias_failure_does_not_disconnect() {
        // Two wrappers of one COM class: a failing alias must not disconnect the
        // driver the first wrapper still uses (`conn_release` answers `false` for it).
        let ctx = Arc::new(test_ctx());
        let connected = Arc::new(AtomicBool::new(false));
        let first = mock_camera_wrapper(Arc::clone(&ctx), mock_camera(Arc::clone(&connected)));
        first.activate().expect("the first wrapper must activate");
        assert_eq!(ctx.active_count.load(Ordering::Relaxed), 1);

        let mut members = mock_camera(Arc::clone(&connected));
        refuse(&mut members, "PixelSizeX", ascom_hr(0x402));
        let alias = mock_camera_wrapper(Arc::clone(&ctx), members);
        let err = alias.activate().expect_err("the alias must fail on its refusing member");
        assert!(err.to_string().contains("PixelSizeX"), "the failing member is lost: {err}");

        assert!(connected.load(Ordering::SeqCst), "the alias disconnected the shared driver");
        assert_eq!(ctx.active_count.load(Ordering::Relaxed), 1, "the alias changed the count");
        assert_eq!(conn_total(&ctx), 1, "the alias left its ref behind");
        assert!(!alias.is_activated());
        assert!(first.is_activated());
    }
}
