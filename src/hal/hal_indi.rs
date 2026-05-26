use std::{ops::RangeInclusive, sync::{Arc, Mutex}};

use crate::hal::{Device, Focuser, FrameType, HalState, Telescope, events::{HalEvent, HalEventSubscribers}, indi::Subscription};

use super::{indi, HalImpl, Camera, DeviceInfo, DeviceType};

pub const CAM_CCD2_POSTFIX: &str = "_CCD2";
pub const SET_PROP_TIME_OUT: u64 = 2000; // ms

///////////////////////////////////////////////////////////////////////////////
// IndiHalImpl

pub struct IndiHalImpl {
    indi:              Arc<indi::Connection>,
    event_subscribers: Arc<HalEventSubscribers>,
    indi_evt_subscr:   Mutex<Option<Subscription>>,
}

impl IndiHalImpl {
    pub fn new(
        indi:              &Arc<indi::Connection>,
        event_subscribers: &Arc<HalEventSubscribers>
    ) -> Arc<Self> {
        let result = Arc::new(Self {
            indi:              Arc::clone(indi),
            event_subscribers: Arc::clone(event_subscribers),
            indi_evt_subscr:   Mutex::new(None),
        });

        let self_ = Arc::clone(&result);
        let indi_evt_subscr = indi.subscribe_events(move |event| {
            self_.indi_event_handler(event);
        });

        *result.indi_evt_subscr.lock().unwrap() = Some(indi_evt_subscr);
        result
    }

    fn indi_event_handler(&self, event: indi::Event) {
        match event {
            indi::Event::NewDevice(evt) => if evt.connected {
                self.process_dev_conn_evt(&evt.device_name, evt.interface, evt.connected);
            }
            indi::Event::DeviceConnected(evt) => {
                self.process_dev_conn_evt(&evt.device_name, evt.interface, evt.connected);
            }
            indi::Event::DeviceDelete(evt) => {
                self.process_dev_conn_evt(&evt.device_name, evt.interface, false);
            }
            _ => {}
        }
    }

    fn process_dev_conn_evt(&self, device_name: &str, interface: indi::DriverInterface, connected: bool) {
        let device_type = Self::driver_interface_to_dev_type(interface);
        if device_type.is_empty() {
            return;
        }
        let device_info = DeviceInfo {
            id:    device_name.to_string(),
            name:  device_name.to_string(),
            type_: device_type,
        };
        let event_to_send = if connected {
            HalEvent::DeviceConnected(Arc::new(device_info))
        } else {
            HalEvent::DeviceDisconnected(Arc::new(device_info))
        };
        self.event_subscribers.send_event(event_to_send);
    }

    fn driver_interface_to_dev_type(drv_interface: indi::DriverInterface) -> DeviceType {
        let is_ccd       = drv_interface.contains(indi::DriverInterface::CCD);
        let is_telescope = drv_interface.contains(indi::DriverInterface::TELESCOPE);
        let is_focuser   = drv_interface.contains(indi::DriverInterface::FOCUSER);

        let mut device_type = DeviceType::empty();
        device_type.set(DeviceType::CAMERA,    is_ccd);
        device_type.set(DeviceType::TELESCOPE, is_telescope);
        device_type.set(DeviceType::FOCUSER,   is_focuser);

        device_type
    }

    fn create_indi_device(&self, id: &str) -> IndiDevice {
        IndiDevice {
            id:   id.to_string(),
            name: id.to_string(),
            indi: Arc::clone(&self.indi),
        }
    }
}

impl Drop for IndiHalImpl {
    fn drop(&mut self) {
        let mut indi_evt_subscr = self.indi_evt_subscr.lock().unwrap();
        if let Some(indi_evt_subscr) = indi_evt_subscr.take() {
            self.indi.unsubscribe(indi_evt_subscr);
        }
        log::info!("IndiHalImpl dropped");
    }
}

impl HalImpl for IndiHalImpl {
    fn state(&self) -> HalState {
        let indi_state = self.indi.state();
        match indi_state {
            indi::ConnState::Connecting    => HalState::Connecting,
            indi::ConnState::Connected     => HalState::Connected,
            indi::ConnState::Disconnecting => HalState::Disconnecting,
            indi::ConnState::Disconnected  => HalState::Disconnected,
            indi::ConnState::Error(err)    => HalState::Error(err),
        }
    }

    fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>> {
        let mut result = Vec::new();
        let indi_devices = self.indi.get_devices_list();
        for device in indi_devices {
            let device_type = Self::driver_interface_to_dev_type(device.interface);

            if device_type.contains(type_filter) {
                result.push(DeviceInfo{
                    id: device.name.to_string(),
                    name: device.name.to_string(),
                    type_: device_type,
                });
                let ccd2_prop_exists = self.indi.property_exists(&device.name, "CCD2", None).unwrap_or(false);
                if device_type.contains(DeviceType::CAMERA) && ccd2_prop_exists {
                    result.push(DeviceInfo{
                        id: device.name.to_string() + CAM_CCD2_POSTFIX,
                        name: device.name.to_string() + CAM_CCD2_POSTFIX,
                        type_: device_type,
                    });
                }
            };
        }
        Ok(result)
    }

    fn camera(&self, id: &str) -> eyre::Result<Arc<dyn Camera + Send + Sync>> {
        let mut ccd = indi::CamCcd::Primary;
        let mut name = id;
        if id.ends_with(CAM_CCD2_POSTFIX) {
            let new_len = id.len() - CAM_CCD2_POSTFIX.len();
            name = &id[..new_len];
            ccd = indi::CamCcd::Secondary;
        }
        let device = IndiDevice {
            id:   id.to_string(),
            name: name.to_string(),
            indi: Arc::clone(&self.indi),
        };
        let camera = IndiCamera { device, ccd };
        Ok(Arc::new(camera))
    }

    fn telescope(&self, id: &str) -> eyre::Result<Arc<dyn Telescope + Send + Sync>> {
        Ok(Arc::new(self.create_indi_device(id)))
    }

    fn focuser(&self, id: &str) -> eyre::Result<Arc<dyn Focuser + Send + Sync>> {
        Ok(Arc::new(self.create_indi_device(id)))
    }
}

///////////////////////////////////////////////////////////////////////////////
// Device

struct IndiDevice {
    id:   String,
    name: String,
    indi: Arc<indi::Connection>,
}

impl Device for IndiDevice {
    fn id(&self) -> &str {
        &self.id
    }

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.indi.is_device_enabled(&self.name)?)
    }
}

///////////////////////////////////////////////////////////////////////////////
// Camera

struct IndiCamera {
    device: IndiDevice,
    ccd:    indi::CamCcd,
}

impl Device for IndiCamera {
    fn id(&self) -> &str {
        self.device.id()
    }

    fn is_active(&self) -> eyre::Result<bool> {
        self.device.is_active()
    }
}

impl Camera for IndiCamera {
    // Common

    fn init_before_shot(&self) -> eyre::Result<()> {
        // Disable fast toggle

        if self.device.indi.camera_is_fast_toggle_supported(&self.device.name).unwrap_or(false) {
            self.device.indi.camera_enable_fast_toggle(&self.device.name, false, false, Some(SET_PROP_TIME_OUT))?;
        }

        // Polling period

        if self.device.indi.device_is_polling_period_supported(&self.device.name)? {
            self.device.indi.device_set_polling_period(&self.device.name, 500, false, None)?;
        }

        // Make binning mode is alwais AVG (if camera supports it)

        if self.device.indi.camera_is_binning_mode_supported(&self.device.name, self.ccd)? {
            _ = self.device.indi.camera_set_binning_mode(
                &self.device.name,
                indi::BinningMode::Avg,
                false, Some(SET_PROP_TIME_OUT)
            );
        }

        // Capture format = RAW

        if self.device.indi.camera_is_capture_format_supported(&self.device.name)? {
            self.device.indi.camera_set_capture_format(
                &self.device.name,
                indi::CaptureFormat::Raw,
                false, None
            )?;
        }

        Ok(())
    }

    // Exposure

    fn exposure_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        let exp_prop_value = self.device.indi.camera_get_exposure_prop_value(&self.device.name, self.ccd)?;
        Ok(exp_prop_value.min..=exp_prop_value.max)
    }

    fn start_exposure(&self, duration: f64) -> eyre::Result<()> {
        self.device.indi.camera_start_exposure(&self.device.name, self.ccd, duration)?;
        Ok(())
    }

    fn abort_exposure(&self) -> eyre::Result<()> {
        self.device.indi.camera_abort_exposure(&self.device.name, self.ccd)?;
        Ok(())
    }

    fn remaining_time(&self) -> eyre::Result<f64> {
        Ok(self.device.indi.camera_get_exposure(&self.device.name, self.ccd)?)
    }

    // Frame type

    fn set_frame_type(&self, frame_type: FrameType) -> eyre::Result<()> {
        let frame_type = match frame_type {
            FrameType::Lights => indi::FrameType::Light,
            FrameType::Flats  => indi::FrameType::Flat,
            FrameType::Darks  => indi::FrameType::Dark,
            FrameType::Biases => indi::FrameType::Bias,
        };

        self.device.indi.camera_set_frame_type(
            &self.device.name, self.ccd,
            frame_type,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Frame

    fn pixel_size_um(&self) -> eyre::Result<(f64, f64)> {
        Ok(self.device.indi.camera_get_pixel_size_um(&self.device.name, self.ccd)?)
    }

    fn is_frame_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_frame_supported(&self.device.name, self.ccd)?)
    }

    fn ccd_size(&self) -> eyre::Result<(usize, usize)> {
        Ok(self.device.indi.camera_get_max_frame_size(&self.device.name, self.ccd)?)
    }

    fn set_frame(&self, x: usize, y: usize, width: usize, height: usize) -> eyre::Result<()> {
        self.device.indi.camera_set_frame(
            &self.device.name, self.ccd,
            x, y, width, height,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Gain

    fn is_gain_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_gain_supported(&self.device.name)?)
    }

    fn gain_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        let gain_prop = self.device.indi.camera_get_gain_prop_value(&self.device.name)?;
        Ok(gain_prop.min..=gain_prop.max)
    }

    fn set_gain(&self, value: f64) -> eyre::Result<()> {
        self.device.indi.camera_set_gain(&self.device.name, value, true, Some(SET_PROP_TIME_OUT))?;
        Ok(())
    }

    // Offset

    fn is_offset_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_offset_supported(&self.device.name)?)
    }

    fn offset_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        let offset_prop = self.device.indi.camera_get_offset_prop_value(&self.device.name)?;
        Ok(offset_prop.min..=offset_prop.max)
    }

    fn set_offset(&self, value: f64) -> eyre::Result<()> {
        let offset_prop = self.device.indi.camera_get_offset_prop_value(&self.device.name)?;
        let mut next_offset = value + 1.0;
        if next_offset > offset_prop.max {
            next_offset = value - 1.0;
        }

        // Due to a bug in INDI
        self.device.indi.camera_set_offset(&self.device.name, next_offset, false, None)?;

        self.device.indi.camera_set_offset(&self.device.name, value, true, Some(SET_PROP_TIME_OUT))?;
        Ok(())
    }

    // Bin

    fn is_binning_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_binning_supported(&self.device.name, self.ccd)?)
    }

    fn max_binning(&self) -> eyre::Result<(usize, usize)> {
        let (max_bin_x, max_bin_y) = self.device.indi.camera_get_max_binning(&self.device.name, self.ccd)?;
        Ok((max_bin_x, max_bin_y))
    }

    fn set_binning(&self, bin_x: usize, bin_y: usize) -> eyre::Result<()> {
        self.device.indi.camera_set_binning(
            &self.device.name, self.ccd,
            bin_x, bin_y,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Cooler

    fn is_cooler_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_cooler_supported(&self.device.name)?)
    }

    fn temperature(&self) -> eyre::Result<f64> {
        Ok(self.device.indi.camera_get_temperature_prop_value(&self.device.name)?.value)
    }

    fn temperature_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        let temp_prop = self.device.indi.camera_get_temperature_prop_value(&self.device.name)?;
        Ok(temp_prop.min..=temp_prop.max)
    }

    fn set_temperature(&self, temperature: Option<f64>) -> eyre::Result<()> {
        if let Some(temperature) = temperature {
            self.device.indi.camera_set_temperature(&self.device.name, temperature)?;
        }
        self.device.indi.camera_enable_cooler(
            &self.device.name,
            temperature.is_some(),
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Heater

    fn is_heater_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_heater_str_supported(&self.device.name)?)
    }

    fn heater_ctrl_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        let list = self.device.indi.camera_get_heater_items(&self.device.name)?;
        let result: Vec<_> = list.iter().map(|(id, text)| (id.to_string(), text.to_string())).collect();
        Ok(result)
    }

    fn control_heater(&self, id: &str) -> eyre::Result<()> {
        self.device.indi.camera_set_heater_str(
            &self.device.name,
            id,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Fan

    fn is_fan_ctrl_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_fan_supported(&self.device.name)?)
    }

    // Low noise mode

    fn is_low_noise_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_low_noise_supported(&self.device.name)?)
    }

    fn enable_low_noise_mode(&self, enable: bool) -> eyre::Result<()> {
        self.device.indi.camera_set_low_noise(
            &self.device.name,
            enable,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // High fullwell mode

    fn is_high_fullwell_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_high_fullwell_supported(&self.device.name)?)
    }

    fn enable_high_fullwell_mode(&self, enable: bool) -> eyre::Result<()> {
        self.device.indi.camera_set_high_fullwell(
            &self.device.name,
            enable,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Conversion gain

    fn is_conversion_gain_supported(&self) -> eyre::Result<bool> {
        Ok(self.device.indi.camera_is_conversion_gain_str_supported(&self.device.name)?)
    }

    fn conversion_gain_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        let list = self.device.indi.camera_get_conversion_gain_items(&self.device.name)?;
        let result: Vec<_> = list.iter().map(|(id, text)| (id.to_string(), text.to_string())).collect();
        Ok(result)
    }

    fn set_conversion_gain(&self, id: &str) -> eyre::Result<()> {
        self.device.indi.camera_set_conversion_gain_str(
            &self.device.name,
            id,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// Telescope (mount)

impl Telescope for IndiDevice {
    fn is_abort_motion_supported(&self) -> bool {
        true
    }

    fn abort_motion(&self) -> eyre::Result<()> {
        self.indi.mount_abort_motion(&self.name)?;
        Ok(())
    }

    fn eq_coord(&self) -> eyre::Result<(f64/*ra*/, f64/*dec*/)> {
        Ok(self.indi.mount_get_eq_ra_and_dec(&self.name)?)
    }

    fn goto_and_track(&self, ra: f64, dec: f64) -> eyre::Result<()> {
        self.indi.mount_set_after_coord_action(
            &self.name,
            indi::AfterCoordSetAction::Track,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        self.indi.mount_set_eq_coord(&self.name, ra, dec, true, None)?;
        Ok(())
    }

    fn slewing(&self) -> eyre::Result<bool> {
        let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.name)?;
        Ok(crd_prop_state == indi::PropState::Busy)
    }

    fn is_guide_rate_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.mount_is_guide_rate_supported(&self.name)?)
    }

    fn guide_rate(&self) -> eyre::Result<(f64/*ra*/, f64/*dec*/)> {
        Ok(self.indi.mount_get_guide_rate(&self.name)?)
    }

    fn can_set_guide_rate(&self) -> eyre::Result<bool> {
        let prop_data = self.indi.mount_get_guide_rate_prop_data(&self.name)?;
        Ok(prop_data.permition != indi::PropPermition::RO)
    }

    fn set_guide_rate(&self, rate_ns: f64, rate_we: f64) -> eyre::Result<()> {
        self.indi.mount_set_guide_rate(
            &self.name,
            rate_ns, rate_we,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    fn pulse_guide(&self, duration_ns: f64, duration_we: f64) -> eyre::Result<()> {
        Ok(self.indi.mount_timed_guide(&self.name, duration_ns, duration_we)?)
    }

    fn is_pulse_guiding(&self) -> eyre::Result<bool> {
        Ok(self.indi.mount_is_timed_guiding(&self.name)?)
    }
}


///////////////////////////////////////////////////////////////////////////////
// Focuser

impl Focuser for IndiDevice {
    fn abs_position_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        let prop = self.indi.focuser_get_abs_value_prop_elem(&self.name)?;
        Ok(prop.min..=prop.max)
    }

    fn abs_position(&self) -> eyre::Result<f64> {
        Ok(self.indi.focuser_get_abs_value_prop_elem(&self.name)?.value)
    }

    fn set_abs_position(&self, value: f64) -> eyre::Result<()> {
        self.indi.focuser_set_abs_value(&self.name, value, true, None)?;
        Ok(())
    }

    fn temperature(&self) -> eyre::Result<f64> {
        Ok(self.indi.focuser_get_temperature(&self.name)?)
    }
}
