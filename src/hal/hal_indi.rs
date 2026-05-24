use std::{ops::Range, sync::{Arc, Mutex}};

use crate::hal::{Device, FrameType, HalState, events::{HalEvent, HalEventSubscribers}, indi::Subscription};

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
        let is_ccd = drv_interface.contains(indi::DriverInterface::CCD);
        let is_telescope = drv_interface.contains(indi::DriverInterface::TELESCOPE);

        let mut device_type = DeviceType::empty();
        device_type.set(DeviceType::CAMERA, is_ccd);
        device_type.set(DeviceType::TELESCOPE, is_telescope);

        device_type
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
    fn state(&self) -> eyre::Result<HalState> {
        let indi_state = self.indi.state();
        match indi_state {
            indi::ConnState::Connecting    => Ok(HalState::Connecting),
            indi::ConnState::Connected     => Ok(HalState::Connected),
            indi::ConnState::Disconnecting => Ok(HalState::Disconnecting),
            indi::ConnState::Disconnected  => Ok(HalState::Disconnected),
            indi::ConnState::Error(err)    => Ok(HalState::Error(err)),
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
        let camera = IndiCamera {
            id:   id.to_string(),
            name: name.to_string(),
            indi: Arc::clone(&self.indi),
            ccd,
        };
        Ok(Arc::new(camera))
    }
}

///////////////////////////////////////////////////////////////////////////////
// Camera

struct IndiCamera {
    id:   String,
    name: String,
    ccd:  indi::CamCcd,
    indi: Arc<indi::Connection>,
}

impl Device for IndiCamera {
    fn id(&self) -> &str {
        &self.id
    }

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.indi.is_device_enabled(&self.name)?)
    }
}

impl Camera for IndiCamera {
    // Common

    fn init_before_shot(&self) -> eyre::Result<()> {
        // Disable fast toggle

        if self.indi.camera_is_fast_toggle_supported(&self.name).unwrap_or(false) {
            self.indi.camera_enable_fast_toggle(&self.name, false, false, Some(SET_PROP_TIME_OUT))?;
        }

        // Polling period

        if self.indi.device_is_polling_period_supported(&self.name)? {
            self.indi.device_set_polling_period(&self.name, 500, false, None)?;
        }

        // Make binning mode is alwais AVG (if camera supports it)

        if self.indi.camera_is_binning_mode_supported(&self.name, self.ccd)? {
            _ = self.indi.camera_set_binning_mode(
                &self.name,
                indi::BinningMode::Avg,
                false, Some(SET_PROP_TIME_OUT)
            );
        }

        // Capture format = RAW

        if self.indi.camera_is_capture_format_supported(&self.name)? {
            self.indi.camera_set_capture_format(
                &self.name,
                indi::CaptureFormat::Raw,
                false, None
            )?;
        }

        Ok(())
    }

    // Exposure

    fn exposure_range(&self) -> eyre::Result<Range<f64>> {
        let exp_prop_value = self.indi.camera_get_exposure_prop_value(&self.name, self.ccd)?;
        Ok(exp_prop_value.min..exp_prop_value.max)
    }

    fn start_exposure(&self, duration: f64) -> eyre::Result<()> {
        self.indi.camera_start_exposure(&self.name, self.ccd, duration)?;
        Ok(())
    }

    // Frame type

    fn set_frame_type(&self, frame_type: FrameType) -> eyre::Result<()> {
        let frame_type = match frame_type {
            FrameType::Lights => indi::FrameType::Light,
            FrameType::Flats  => indi::FrameType::Flat,
            FrameType::Darks  => indi::FrameType::Dark,
            FrameType::Biases => indi::FrameType::Bias,
        };

        self.indi.camera_set_frame_type(
            &self.name,
            self.ccd,
            frame_type,
            true,
            Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Frame

    fn is_frame_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_frame_supported(&self.name, self.ccd)?)
    }

    fn ccd_size(&self) -> eyre::Result<(usize, usize)> {
        Ok(self.indi.camera_get_max_frame_size(&self.name, self.ccd)?)
    }

    fn set_frame(&self, x: usize, y: usize, width: usize, height: usize) -> eyre::Result<()> {
        self.indi.camera_set_frame(
            &self.name, self.ccd,
            x, y, width, height,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Gain

    fn is_gain_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_gain_supported(&self.name)?)
    }

    fn gain_range(&self) -> eyre::Result<Range<f64>> {
        let gain_prop = self.indi.camera_get_gain_prop_value(&self.name)?;
        Ok(gain_prop.min..gain_prop.max)
    }

    fn set_gain(&self, value: f64) -> eyre::Result<()> {
        self.indi.camera_set_gain(&self.name, value, true, Some(SET_PROP_TIME_OUT))?;
        Ok(())
    }

    // Offset

    fn is_offset_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_offset_supported(&self.name)?)
    }

    fn offset_range(&self) -> eyre::Result<Range<f64>> {
        let offset_prop = self.indi.camera_get_offset_prop_value(&self.name)?;
        Ok(offset_prop.min..offset_prop.max)
    }

    fn set_offset(&self, value: f64) -> eyre::Result<()> {
        let offset_prop = self.indi.camera_get_offset_prop_value(&self.name)?;
        let mut next_offset = value + 1.0;
        if next_offset > offset_prop.max {
            next_offset = value - 1.0;
        }

        // Due to a bug in INDI
        self.indi.camera_set_offset(&self.name, next_offset, false, None)?;

        self.indi.camera_set_offset(&self.name, value, true, Some(SET_PROP_TIME_OUT))?;
        Ok(())
    }

    // Bin

    fn is_binning_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_binning_supported(&self.name, self.ccd)?)
    }

    fn max_binning(&self) -> eyre::Result<(usize, usize)> {
        let (max_bin_x, max_bin_y) = self.indi.camera_get_max_binning(&self.name, self.ccd)?;
        Ok((max_bin_x, max_bin_y))
    }

    fn set_binning(&self, bin_x: usize, bin_y: usize) -> eyre::Result<()> {
        self.indi.camera_set_binning(
            &self.name, self.ccd,
            bin_x, bin_y,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Cooler

    fn is_cooler_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_cooler_supported(&self.name)?)
    }

    fn temperature_range(&self) -> eyre::Result<Range<f64>> {
        let temp_prop = self.indi.camera_get_temperature_prop_value(&self.name)?;
        Ok(temp_prop.min..temp_prop.max)
    }

    fn set_temperature(&self, temperature: Option<f64>) -> eyre::Result<()> {
        if let Some(temperature) = temperature {
            self.indi.camera_set_temperature(&self.name, temperature)?;
        }
        self.indi.camera_enable_cooler(
            &self.name,
            temperature.is_some(),
            true,
            Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Heater

    fn is_heater_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_heater_str_supported(&self.name)?)
    }

    fn heater_ctrl_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        let list = self.indi.camera_get_heater_items(&self.name)?;
        let result: Vec<_> = list.iter().map(|(id, text)| (id.to_string(), text.to_string())).collect();
        Ok(result)
    }

    fn control_heater(&self, id: &str) -> eyre::Result<()> {
        self.indi.camera_set_heater_str(&self.name, id, true, Some(SET_PROP_TIME_OUT))?;
        Ok(())
    }

    // Fan

    fn is_fan_ctrl_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_fan_supported(&self.name)?)
    }

    // Low noise mode

    fn is_low_noise_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_low_noise_supported(&self.name)?)
    }

    fn enable_low_noise_mode(&self, enable: bool) -> eyre::Result<()> {
        self.indi.camera_set_low_noise(
            &self.name,
            enable,
            true,
            Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // High fullwell mode

    fn is_high_fullwell_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_high_fullwell_supported(&self.name)?)
    }

    fn enable_high_fullwell_mode(&self, enable: bool) -> eyre::Result<()> {
        self.indi.camera_set_high_fullwell(
            &self.name,
            enable,
            true,
            Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    // Conversion gain

    fn is_conversion_gain_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.camera_is_conversion_gain_str_supported(&self.name)?)
    }

    fn conversion_gain_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        let list = self.indi.camera_get_conversion_gain_items(&self.name)?;
        let result: Vec<_> = list.iter().map(|(id, text)| (id.to_string(), text.to_string())).collect();
        Ok(result)
    }

    fn set_conversion_gain(&self, id: &str) -> eyre::Result<()> {
        self.indi.camera_set_conversion_gain_str(
            &self.name,
            id,
            true,
            Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

}
