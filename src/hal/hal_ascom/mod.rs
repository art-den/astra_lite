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

use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use ascom::camera::Camera as AcCamera;
use ascom::chooser::DeviceType as AcDeviceType;
use ascom::drivers::installed_drivers;
use ascom::filterwheel::FilterWheel as AcFilterWheel;
use ascom::focuser::Focuser as AcFocuser;
use ascom::telescope::Telescope as AcTelescope;

use crate::hal::events::HalEventHandlers;
use crate::hal::events::HalEvent;
use crate::hal::*;

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
}

impl HalCtx {
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
    pub fn disconnect_all(&self) -> eyre::Result<()> {
        let data = self.data_read();
        for camera in &data.cameras {
            log_if_error_pub(&camera.deactivate_impl(), "Deactivate ASCOM camera");
        }
        for telescope in &data.telescopes {
            log_if_error_pub(&telescope.deactivate_impl(), "Deactivate ASCOM telescope");
        }
        for focuser in &data.focusers {
            log_if_error_pub(&focuser.deactivate_impl(), "Deactivate ASCOM focuser");
        }
        for filter_wheel in &data.filter_wheels {
            log_if_error_pub(&filter_wheel.deactivate_impl(), "Deactivate ASCOM filter wheel");
        }
        Ok(())
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
// Camera

pub struct AscomCamera {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
}

impl AscomCamera {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
        }
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
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

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(false)
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
        Err(not_connected(&self.device_id))
    }

    fn start_exposure(&self, _duration: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn abort_exposure(&self) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn remaining_time(&self) -> Option<f64> {
        None
    }

    // Frame type

    fn set_frame_type(&self, _frame_type: FrameType) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    // Frame

    fn pixel_size_um(&self) -> eyre::Result<(f64, f64)> {
        Err(not_connected(&self.device_id))
    }

    fn is_frame_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn ccd_size(&self) -> eyre::Result<(usize, usize)> {
        Err(not_connected(&self.device_id))
    }

    fn set_frame(&self, _x: usize, _y: usize, _width: usize, _height: usize) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    // Gain

    fn is_gain_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn gain_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Err(not_connected(&self.device_id))
    }

    fn set_gain(&self, _value: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    // Offset

    fn is_offset_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn offset_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Err(not_connected(&self.device_id))
    }

    fn set_offset(&self, _value: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    // Bin

    fn is_binning_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn max_binning(&self) -> eyre::Result<(usize, usize)> {
        Err(not_connected(&self.device_id))
    }

    fn set_binning(&self, _bin_x: usize, _bin_y: usize) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    // Cooler

    fn is_cooler_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn temperature(&self) -> eyre::Result<f64> {
        Err(not_connected(&self.device_id))
    }

    fn temperature_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Err(not_connected(&self.device_id))
    }

    fn set_temperature(&self, _temperature: Option<f64>) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
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

pub struct AscomTelescope {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
}

impl AscomTelescope {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
        }
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
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

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(false)
    }
}

impl Telescope for AscomTelescope {
    fn state(&self) -> eyre::Result<TelescopeState> {
        Err(not_connected(&self.device_id))
    }

    fn site(&self) -> eyre::Result<TelescopeSite> {
        Err(not_connected(&self.device_id))
    }

    fn is_abort_motion_supported(&self) -> bool {
        true
    }

    fn abort_motion(&self) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn is_parked(&self) -> eyre::Result<bool> {
        Err(not_connected(&self.device_id))
    }

    fn park(&self) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn unpark(&self) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn is_tracking(&self) -> eyre::Result<bool> {
        Err(not_connected(&self.device_id))
    }

    fn track(&self, _enabled: bool) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn revert_motion(&self, _reverse_ns: bool, _reverse_we: bool) -> eyre::Result<()> {
        Ok(())
    }

    fn move_(&self, _direction: TelescopeMoveDir) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn slew_speed_list(&self) -> eyre::Result<Vec<(String, String)>> {
        Err(not_connected(&self.device_id))
    }

    fn set_slew_speed(&self, _speed_id: &str) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn eq_coord(&self) -> eyre::Result<(f64, f64)> {
        Err(not_connected(&self.device_id))
    }

    fn goto_and_track(&self, _ra: f64, _dec: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn is_slewing(&self) -> eyre::Result<bool> {
        Err(not_connected(&self.device_id))
    }

    fn sync(&self, _ra: f64, _dec: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn is_guide_rate_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn guide_rate(&self) -> eyre::Result<(f64, f64)> {
        Err(not_connected(&self.device_id))
    }

    fn pulse_max_duration(&self) -> eyre::Result<(f64, f64)> {
        Err(not_connected(&self.device_id))
    }

    fn can_set_guide_rate(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn set_guide_rate(&self, _rate_ns: f64, _rate_we: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn pulse_guide(&self, _duration_ns: f64, _duration_we: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn is_pulse_guiding(&self) -> eyre::Result<bool> {
        Err(not_connected(&self.device_id))
    }
}

///////////////////////////////////////////////////////////////////////////////
// Focuser

pub struct AscomFocuser {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
}

impl AscomFocuser {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
        }
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
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

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(false)
    }
}

impl Focuser for AscomFocuser {
    fn state(&self) -> eyre::Result<FocuserState> {
        Err(not_connected(&self.device_id))
    }

    fn abs_position_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Err(not_connected(&self.device_id))
    }

    fn abs_position(&self) -> eyre::Result<f64> {
        Err(not_connected(&self.device_id))
    }

    fn set_abs_position(&self, _value: f64) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }

    fn temperature(&self) -> eyre::Result<f64> {
        Err(not_connected(&self.device_id))
    }
}

///////////////////////////////////////////////////////////////////////////////
// Filter wheel

pub struct AscomFilterWheel {
    ctx:         Arc<HalCtx>,
    device_id:   Arc<String>,
    device_name: String,
}

impl AscomFilterWheel {
    fn new(ctx: Arc<HalCtx>, info: DeviceInfo) -> Self {
        Self {
            ctx,
            device_id:   Arc::new(info.id),
            device_name: info.name,
        }
    }

    fn notify_periodic_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        Ok(())
    }

    fn deactivate_impl(&self) -> eyre::Result<()> {
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

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(false)
    }
}

impl FilterWheel for AscomFilterWheel {
    fn list_and_active(&self) -> eyre::Result<(Vec<String>, usize)> {
        Err(not_connected(&self.device_id))
    }

    fn set_active(&self, _active_elem: usize) -> eyre::Result<()> {
        Err(not_connected(&self.device_id))
    }
}
