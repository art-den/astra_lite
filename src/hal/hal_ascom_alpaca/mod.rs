use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::AtomicBool;

use bitflags::bitflags;

use crate::hal::{
    Camera, CameraInfo, CcdPurpose, Device, DeviceInfo, DeviceType, FilterWheel, Focuser, FrameType,
    HalFeatures, HalImpl, HalState, Telescope,
};
use crate::hal::events::{HalEvent, HalEventHandlers};

///////////////////////////////////////////////////////////////////////////////
// AscomAlpacaHalImpl

struct AscomAlpacaHalData {
    async_runtime:  Arc<tokio::runtime::Runtime>,
    client:         ascom_alpaca::Client,
    aa_devices:     Vec<ascom_alpaca::api::TypedDevice>,
    devices:        Vec<DeviceInfo>,
    cameras:        Vec<Arc<AscomAlpacaCamera>>,
}

pub struct AscomAlpacaHalImpl {
    data:           RwLock<Option<Arc<AscomAlpacaHalData>>>,
    event_handlers: Arc<HalEventHandlers>,
}

impl AscomAlpacaHalImpl {
    pub fn new(event_handlers: &Arc<HalEventHandlers>) -> Arc<Self> {
        Arc::new(Self {
            data:           RwLock::new(None),
            event_handlers: Arc::clone(event_handlers),
        })
    }

    fn connect_impl(&self, address: &str) -> eyre::Result<()> {
        let async_runtime = Arc::new(tokio::runtime::Runtime::new()?);
        let client = ascom_alpaca::Client::new(address)?;

        let aa_devices = async_runtime.block_on(async {
            let list = client.get_devices().await?.collect::<Vec<_>>();
            eyre::Ok(list)
        })?;

        let devices = aa_devices
            .iter()
            .map(|typed_device| {
                let (id, name, dev_type) = match typed_device {
                    ascom_alpaca::api::TypedDevice::Camera(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::CAMERA),
                    ascom_alpaca::api::TypedDevice::Telescope(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::TELESCOPE),
                    ascom_alpaca::api::TypedDevice::Focuser(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::FOCUSER),
                    ascom_alpaca::api::TypedDevice::FilterWheel(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::FLT_WHELL),
                };
                DeviceInfo {
                    id:    id.to_string(),
                    name:  name.to_string(),
                    type_: dev_type
                }
            })
            .collect();

        let cameras = aa_devices
            .iter()
            .filter_map(|typed_device| {
                if let ascom_alpaca::api::TypedDevice::Camera(dev) = typed_device {
                    Some(dev)
                } else {
                    None
                }
            })
            .filter_map(|device| {
                Self::create_camera_from_aa_device(
                    device,
                    &async_runtime,
                    &self.event_handlers
                ).ok()
            })
            .collect();

        let data = AscomAlpacaHalData {
            client,
            aa_devices,
            devices,
            async_runtime,
            cameras,
        };

        *self.data.write().unwrap() = Some(Arc::new(data));

        Ok(())
    }

    fn data(&self) -> eyre::Result<Arc<AscomAlpacaHalData>> {
        let data_mutex = self.data.read().unwrap();
        if let Some(data) = &*data_mutex {
            Ok(Arc::clone(data))
        } else {
            eyre::bail!("AscomAlpacaHalImpl not connected!");
        }
    }

    pub fn connect(&self, address: &str) -> eyre::Result<()> {
        if self.state() == HalState::Connected {
            self.disconnect()?;
        }

        self.event_handlers.send(HalEvent::StateChanged(HalState::Connecting));

        let result = self.connect_impl(address);
        match &result {
            Ok(_) => {
                self.event_handlers.send(HalEvent::StateChanged(HalState::Connected));
                self.send_devices_connected_events();
            }
            Err(err) =>
                self.event_handlers.send(HalEvent::StateChanged(HalState::Error(err.to_string()))),
        }
        result
    }

    fn send_devices_connected_events(&self) {
        let data = self.data.read().unwrap();
        if let Some(data) = &*data {
            for device in &data.devices {
                self.event_handlers.send(
                    HalEvent::DeviceConnected(Arc::new(device.clone()))
                );
                if device.type_.contains(DeviceType::CAMERA) {
                    let cam_id = Arc::new(device.id.clone());
                    self.event_handlers.send(
                        HalEvent::CameraIsReadyToWork(Arc::clone(&cam_id))
                    );
                    self.event_handlers.send(
                        HalEvent::CameraIsReadyForCooling(Arc::clone(&cam_id))
                    );
                    self.event_handlers.send(
                        HalEvent::CameraOffsetCanBeControlled(Arc::clone(&cam_id))
                    );
                    self.event_handlers.send(
                        HalEvent::CameraGainCanBeControlled(Arc::clone(&cam_id))
                    );
                }
            }
        }
    }

    fn create_camera_from_aa_device(
        aa_camera:      &Arc<dyn ascom_alpaca::api::Camera>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>

    ) -> eyre::Result<Arc<AscomAlpacaCamera>> {

        let mut camera_flags = CameraFlags::empty();

        async_runtime.block_on(async {
            let max_bin_x = aa_camera.max_bin_x().await?;
            let max_bin_y = aa_camera.max_bin_y().await?;
            let bin_supported = u8::min(max_bin_x, max_bin_y) > 1;
            let cooler_supported = aa_camera.can_set_ccd_temperature().await?;
            let can_stop_exposure = aa_camera.can_stop_exposure().await?;

            camera_flags.set(CameraFlags::FRAME_SUPPORTED, true);
            camera_flags.set(CameraFlags::GAIN_SUPPORTED, true);
            camera_flags.set(CameraFlags::OFFSET_SUPPORTED, true);
            camera_flags.set(CameraFlags::BIN_SUPPORTED, bin_supported);
            camera_flags.set(CameraFlags::COOLER_SUPPORTED, cooler_supported);
            camera_flags.set(CameraFlags::CAN_STOP_EXP, can_stop_exposure);
            eyre::Ok(())
        })?;

         Ok(Arc::new(AscomAlpacaCamera {
            flags:          camera_flags,
            camera:         Arc::clone(aa_camera),
            event_handlers: Arc::clone(event_handlers),
            async_runtime:  Arc::clone(async_runtime),
            light_frame:    AtomicBool::new(true),
            exp_data:       Mutex::new(None),
        }))
    }

    fn typed_device_to_device(typed_devive: &ascom_alpaca::api::TypedDevice) -> Arc<dyn ascom_alpaca::api::Device> {
        match typed_devive {
            ascom_alpaca::api::TypedDevice::Camera(dev) =>
                Arc::clone(dev) as Arc<dyn ascom_alpaca::api::Device>,
            ascom_alpaca::api::TypedDevice::Telescope(dev) =>
                Arc::clone(dev) as Arc<dyn ascom_alpaca::api::Device>,
            ascom_alpaca::api::TypedDevice::Focuser(dev) =>
                Arc::clone(dev) as Arc<dyn ascom_alpaca::api::Device>,
            ascom_alpaca::api::TypedDevice::FilterWheel(dev) =>
                Arc::clone(dev) as Arc<dyn ascom_alpaca::api::Device>,
        }
    }
}

impl HalImpl for AscomAlpacaHalImpl {
    fn features(&self) -> HalFeatures {
        HalFeatures::empty()
    }

    fn state(&self) -> HalState {
        let data = self.data.read().unwrap();
        if data.is_some() {
            HalState::Connected
        } else {
            HalState::Disconnected
        }
    }

    fn disconnect(&self) -> eyre::Result<()> {
        self.event_handlers.send(HalEvent::StateChanged(HalState::Disconnecting));
        let mut data = self.data.write().unwrap();
        let devices = if let Some(data) = data.take() {
            let list = data.devices.iter().cloned().collect();
            drop(data);
            list
        } else {
            Vec::new()
        };

        self.event_handlers.send(HalEvent::StateChanged(HalState::Disconnected));

        for device in devices {
            self.event_handlers.send(
                HalEvent::DeviceDisconnected(Arc::new(device))
            );
        }

        Ok(())
    }

    fn notify_periodical_timer_tick(&self, timer_period: usize) -> eyre::Result<()> {
        if let Ok(data) = self.data() {
            for camera in &data.cameras {
                camera.notify_periodical_timer_tick(timer_period)?;
            }
        }

        Ok(())
    }

    fn devices(&self, type_filter: DeviceType) -> eyre::Result<Vec<DeviceInfo>> {
        let data = self.data()?;
        let result = data.devices
            .iter()
            .filter(|dev| dev.type_.contains(type_filter))
            .cloned()
            .collect();
        Ok(result)
    }

    fn cameras(&self) -> eyre::Result<Vec<CameraInfo>> {
        let data = self.data()?;
        let result = data.cameras
            .iter()
            .map(|cam| {
                CameraInfo {
                    id:   cam.camera.unique_id().to_string(),
                    name: cam.camera.static_name().to_string(),
                    ccd:  CcdPurpose::Unknown,
                }
            })
            .collect();
        Ok(result)
    }

    fn camera(&self, id: &str) -> eyre::Result<Arc<dyn Camera + Send + Sync>> {
        let data = self.data()?;
        data.cameras
            .iter()
            .find(|camera| {
                camera.camera.unique_id() == id
            })
            .map(|camera| Arc::clone(camera) as Arc<dyn Camera + Send + Sync>)
            .ok_or_else(|| eyre::eyre!("Camera with id {id} not fount"))
    }

    fn telescope(&self, _id: &str) -> eyre::Result<Arc<dyn Telescope + Send + Sync>> {
        eyre::bail!("Not supported yet");
    }

    fn focuser(&self, _id: &str) -> eyre::Result<Arc<dyn Focuser + Send + Sync>> {
        eyre::bail!("Not supported yet");
    }

    fn filter_wheel(&self, _id: &str) -> eyre::Result<Arc<dyn FilterWheel + Send + Sync>> {
        eyre::bail!("Not supported yet");
    }
}

///////////////////////////////////////////////////////////////////////////////
// Camera

bitflags! {
    struct CameraFlags: u32 {
        const FRAME_SUPPORTED  = (1 << 0);
        const GAIN_SUPPORTED   = (1 << 1);
        const OFFSET_SUPPORTED = (1 << 1);
        const BIN_SUPPORTED    = (1 << 2);
        const COOLER_SUPPORTED = (1 << 3);
        const CAN_STOP_EXP     = (1 << 4);
    }
}

struct ExposureData {
    duration: f64,
    start_time: std::time::Instant,
}

struct AscomAlpacaCamera {
    camera:         Arc<dyn ascom_alpaca::api::Camera>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
    light_frame:    AtomicBool,
    exp_data:       Mutex<Option<ExposureData>>,
    flags:          CameraFlags,
}

impl AscomAlpacaCamera {
    fn notify_periodical_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let mut exp_data_mutex = self.exp_data.lock().unwrap();
        if let Some(exp_data) = &*exp_data_mutex {
            let eplased = exp_data.start_time.elapsed().as_secs_f64();
            let remaining = exp_data.duration - eplased;
            self.event_handlers.send(HalEvent::CameraTimeUntilEndOfExposure {
                device_id: Arc::new(self.camera.unique_id().to_string()),
                time:      remaining.clamp(0.0, exp_data.duration)
            });

            let image_ready_result = self.async_runtime.block_on(async {
                self.camera.image_ready().await
            });
            match image_ready_result {
                Ok(ready) => {
                    if ready {
                        *exp_data_mutex = None;
                    }
                }
                Err(err) => {
                    *exp_data_mutex = None;
                    eyre::bail!("Error during wait of end of exposure: {}", err);
                }
            }
        }

        Ok(())
    }
}

impl Device for AscomAlpacaCamera {
    fn id(&self) -> &str {
        self.camera.unique_id()
    }

    fn name(&self) -> &str {
        self.camera.static_name()
    }

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.async_runtime.block_on(async {
            self.camera.connected().await
        })?)
    }
}

impl Camera for AscomAlpacaCamera {
    fn init_before_shot(&self) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.camera.connected().await?;
            eyre::Ok(())
        })?;
        Ok(())
    }

    // Exposure

    fn exposure_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(self.async_runtime.block_on(async {
            let range = self.camera.exposure_range().await?;
            eyre::Ok(range.start().as_secs_f64() ..= range.end().as_secs_f64())
        })?)
    }

    fn start_exposure(&self, duration: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            let duration = std::time::Duration::from_secs_f64(duration);
            let is_light_frame = self.light_frame.load(std::sync::atomic::Ordering::Relaxed);
            self.camera.start_exposure(duration, is_light_frame).await?;
            eyre::Ok(())
        })?;
        *self.exp_data.lock().unwrap() = Some(ExposureData{
            duration,
            start_time: std::time::Instant::now(),
        });
        Ok(())
    }

    fn abort_exposure(&self) -> eyre::Result<()> {
        if !self.flags.contains(CameraFlags::CAN_STOP_EXP) {
            return Ok(());
        }
        self.async_runtime.block_on(async {
            self.camera.stop_exposure().await?;
            eyre::Ok(())
        })?;
        Ok(())
    }

    fn remaining_time(&self) -> Option<f64> {
        let exp_data = self.exp_data.lock().unwrap();
        if let Some(exp_data) = &*exp_data {
            let eplased = exp_data.start_time.elapsed().as_secs_f64();
            let remaining = exp_data.duration - eplased;
            Some(remaining.clamp(0.0, exp_data.duration))
        } else {
            None
        }
    }

    // Frame type

    fn set_frame_type(&self, frame_type: FrameType) -> eyre::Result<()> {
        let light_frame = frame_type == FrameType::Lights;
        self.light_frame.store(light_frame, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    // Frame

    fn pixel_size_um(&self) -> eyre::Result<(f64, f64)> {
        self.async_runtime.block_on(async {
            let pixel_size_x = self.camera.pixel_size_x().await?;
            let pixel_size_y = self.camera.pixel_size_y().await?;
            eyre::Ok((pixel_size_x, pixel_size_y))
        })
    }

    fn is_frame_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::FRAME_SUPPORTED))
    }

    fn ccd_size(&self) -> eyre::Result<(usize, usize)> {
        self.async_runtime.block_on(async {
            let ccd_size_x = self.camera.camera_x_size().await? as usize;
            let ccd_size_y = self.camera.camera_y_size().await? as usize;
            eyre::Ok((ccd_size_x, ccd_size_y))
        })
    }

    fn set_frame(&self, x: usize, y: usize, width: usize, height: usize) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.camera.set_start_x(x as u32).await?;
            self.camera.set_start_x(y as u32).await?;
            self.camera.set_num_x(width as u32).await?;
            self.camera.set_num_y(height as u32).await?;
            eyre::Ok(())
        })
    }

    // Gain

    fn is_gain_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::GAIN_SUPPORTED))
    }

    fn gain_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        self.async_runtime.block_on(async {
            let min_gain = self.camera.gain_min().await? as f64;
            let max_gain = self.camera.gain_max().await? as f64;
            eyre::Ok(min_gain ..= max_gain)
        })
    }

    fn set_gain(&self, value: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.camera.set_gain(value as i32).await?;
            eyre::Ok(())
        })
    }

    // Offset

    fn is_offset_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::OFFSET_SUPPORTED))
    }

    fn offset_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        self.async_runtime.block_on(async {
            let min_offset = self.camera.offset_min().await? as f64;
            let max_offset = self.camera.offset_max().await? as f64;
            eyre::Ok(min_offset ..= max_offset)
        })
    }

    fn set_offset(&self, value: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.camera.set_offset(value as i32).await?;
            eyre::Ok(())
        })
    }

    // Bin

    fn is_binning_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::BIN_SUPPORTED))
    }

    fn max_binning(&self) -> eyre::Result<(usize/*x*/, usize/*y*/)> {
        self.async_runtime.block_on(async {
            let max_bin_x = self.camera.max_bin_x().await? as usize;
            let max_bin_y = self.camera.max_bin_y().await? as usize;
            eyre::Ok((max_bin_x, max_bin_y))
        })
    }

    fn set_binning(&self, bin_x: usize, bin_y: usize) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.camera.set_bin_x(bin_x as u8).await?;
            self.camera.set_bin_y(bin_y as u8).await?;
            eyre::Ok(())
        })
    }

    // Cooler

    fn is_cooler_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::COOLER_SUPPORTED))
    }

    fn temperature(&self) -> eyre::Result<f64> {
        self.async_runtime.block_on(async {
            let result = self.camera.ccd_temperature().await?;
            eyre::Ok(result)
        })
    }

    fn temperature_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(-100.0 ..= 50.0)
   }

    fn set_temperature(&self, temperature: Option<f64>) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            if let Some(temperature) = temperature {
                self.camera.set_set_ccd_temperature(temperature).await?;
                self.camera.set_cooler_on(true).await?;
            } else {
                self.camera.set_cooler_on(false).await?;
            }
            eyre::Ok(())
        })
    }

    // Heater

    fn is_heater_supported(&self) -> eyre::Result<bool> {
        Ok(false)
    }

    fn heater_ctrl_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
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

    fn conversion_gain_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        unimplemented!();
    }

    fn set_conversion_gain(&self, _id: &str) -> eyre::Result<()> {
        unimplemented!();
    }

    // Telescope

    fn set_telescope_focal_len(&self, _focal_len: f64) -> eyre::Result<()> {
        // do nothing
        Ok(())
    }
}
