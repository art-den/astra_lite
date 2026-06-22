use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::AtomicBool;

use ascom_alpaca as aa;

use bitflags::bitflags;
use itertools::izip;

use crate::hal::{
    Camera, CameraInfo, CameraShot, CameraShotType, CcdPurpose, Device, DeviceInfo, DeviceType, FilterWheel, Focuser, FrameType, HalFeatures, HalImpl, HalState, Telescope
};
use crate::hal::events::{HalEvent, HalEventHandlers};
use crate::image::raw::{RawImage, RawImageInfo};

///////////////////////////////////////////////////////////////////////////////
// AscomAlpacaHalImpl

struct AscomAlpacaHalData {
    async_runtime:  Arc<tokio::runtime::Runtime>,
    client:         aa::Client,
    aa_devices:     Vec<aa::api::TypedDevice>,
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
        let client = aa::Client::new(address)?;

        let aa_devices = async_runtime.block_on(async {
            let list = client.get_devices().await?.collect::<Vec<_>>();
            eyre::Ok(list)
        })?;

        let devices = aa_devices
            .iter()
            .map(|typed_device| {
                let (id, name, dev_type) = match typed_device {
                    aa::api::TypedDevice::Camera(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::CAMERA),
                    aa::api::TypedDevice::Telescope(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::TELESCOPE),
                    aa::api::TypedDevice::Focuser(dev) =>
                        (dev.unique_id(), dev.static_name(), DeviceType::FOCUSER),
                    aa::api::TypedDevice::FilterWheel(dev) =>
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
                if let aa::api::TypedDevice::Camera(dev) = typed_device {
                    Some(dev)
                } else {
                    None
                }
            })
            .filter_map(|aa_device| {
                AscomAlpacaCamera::from_aa_device(
                    aa_device,
                    &async_runtime,
                    &self.event_handlers
                ).ok()
            })
            .map(Arc::new)
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

    fn typed_device_to_device(typed_devive: &aa::api::TypedDevice) -> Arc<dyn aa::api::Device> {
        match typed_devive {
            aa::api::TypedDevice::Camera(dev) =>
                Arc::clone(dev) as Arc<dyn aa::api::Device>,
            aa::api::TypedDevice::Telescope(dev) =>
                Arc::clone(dev) as Arc<dyn aa::api::Device>,
            aa::api::TypedDevice::Focuser(dev) =>
                Arc::clone(dev) as Arc<dyn aa::api::Device>,
            aa::api::TypedDevice::FilterWheel(dev) =>
                Arc::clone(dev) as Arc<dyn aa::api::Device>,
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

//////////////////////////////////////////////////////////////////////////////
// CameraShot

struct AscomAlpacaCameraShot {
    array:   aa::api::camera::ImageArray,
    dl_time: f64,
}

impl CameraShot for AscomAlpacaCameraShot {
    fn get_type(&self) -> CameraShotType {
        let (_, _, layers) = self.array.dim();
        if layers == 1 {
            CameraShotType::RawCcdData
        } else {
            CameraShotType::ReadyImage
        }
    }

    fn get_raw(&self) -> eyre::Result<RawImage> {
        assert!(self.get_type() == CameraShotType::RawCcdData);
        let raw_2d_arr = self.array.index_axis(ndarray::Axis(2), 0);
        let (width, height) = raw_2d_arr.dim();
        let data_len = width * height;
        let mut data = vec![0; data_len];
        for (src, dst) in izip!(raw_2d_arr, &mut data) {
            *dst = (*src).clamp(0, u16::MAX as i32) as u16;
        }
        let mut info = RawImageInfo::default();
        info.width = width;
        info.height = height;
        info.max_value = u16::MAX;
        let cfa_arrary = info.cfa.get_array();
        Ok(RawImage::new(info, data, cfa_arrary))
    }

    fn get_image(&self, image: &mut crate::image::image::Image) -> eyre::Result<()> {
        todo!()
    }

    fn download_time(&self) -> f64 {
        self.dl_time
    }

    fn file_ext(&self) -> &str {
        "fits"
    }

    fn save_to_file(&self, file_name: &Path) -> eyre::Result<()> {
        Ok(())
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
        const CAN_GET_COOL_PWR = (1 << 5);
    }
}

struct ExposureData {
    duration:   f64,
    start_time: std::time::Instant,
}

#[derive(Default)]
struct PrevData {
    temperature: Option<f64>,
    cool_pwr:    Option<f64>,
}

struct AscomAlpacaCamera {
    camera:         Arc<dyn aa::api::Camera>,
    device_id:      Arc<String>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
    light_frame:    AtomicBool,
    exp_data:       Mutex<Option<ExposureData>>,
    flags:          CameraFlags,
    exp_range:      RangeInclusive<f64>,
    gain_range:     RangeInclusive<f64>,
    offset_range:   RangeInclusive<f64>,
    pixel_size_x:   f64,
    pixel_size_y:   f64,
    ccd_size_x:     usize,
    ccd_size_y:     usize,
    max_bin_x:      usize,
    max_bin_y:      usize,
    prev_data:      Mutex<PrevData>,
}

impl AscomAlpacaCamera {
    fn from_aa_device(
        aa_camera:      &Arc<dyn aa::api::Camera>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {
        let mut camera_flags = CameraFlags::empty();
        let result = async_runtime.block_on(async {
            let max_bin_x = aa_camera.max_bin_x().await? as usize;
            let max_bin_y = aa_camera.max_bin_y().await? as usize;
            let bin_supported = usize::min(max_bin_x, max_bin_y) > 1;
            let cooler_supported = aa_camera.can_set_ccd_temperature().await?;
            let can_stop_exposure = aa_camera.can_stop_exposure().await?;
            let can_get_cooler_power = aa_camera.can_get_cooler_power().await.unwrap_or(false);
            let exp_range = aa_camera.exposure_range().await?;
            let pixel_size_x = aa_camera.pixel_size_x().await?;
            let pixel_size_y = aa_camera.pixel_size_y().await?;
            let ccd_size_x = aa_camera.camera_x_size().await? as usize;
            let ccd_size_y = aa_camera.camera_y_size().await? as usize;
            let min_gain = aa_camera.gain_min().await.unwrap_or(0) as f64;
            let max_gain = aa_camera.gain_max().await.unwrap_or(100_000) as f64;
            let min_offset = aa_camera.offset_min().await.unwrap_or(0) as f64;
            let max_offset = aa_camera.offset_max().await.unwrap_or(65535) as f64;
            let offset_supported = aa_camera.offset().await.is_ok();
            let gain_supported = aa_camera.gain().await.is_ok();

            camera_flags.set(CameraFlags::FRAME_SUPPORTED, true);
            camera_flags.set(CameraFlags::GAIN_SUPPORTED, gain_supported);
            camera_flags.set(CameraFlags::OFFSET_SUPPORTED, offset_supported);
            camera_flags.set(CameraFlags::BIN_SUPPORTED, bin_supported);
            camera_flags.set(CameraFlags::COOLER_SUPPORTED, cooler_supported);
            camera_flags.set(CameraFlags::CAN_STOP_EXP, can_stop_exposure);
            camera_flags.set(CameraFlags::CAN_GET_COOL_PWR, can_get_cooler_power);

            eyre::Ok(Self {
                device_id:      Arc::new(aa_camera.unique_id().to_string()),
                camera:         Arc::clone(aa_camera),
                event_handlers: Arc::clone(event_handlers),
                async_runtime:  Arc::clone(async_runtime),
                light_frame:    AtomicBool::new(true),
                exp_data:       Mutex::new(None),
                flags:          camera_flags,
                exp_range:      exp_range.start().as_secs_f64() ..= exp_range.end().as_secs_f64(),
                gain_range:     min_gain ..= max_gain,
                offset_range:   min_offset ..= max_offset,
                prev_data:      Mutex::new(PrevData::default()),
                pixel_size_x,
                pixel_size_y,
                ccd_size_x,
                ccd_size_y,
                max_bin_x,
                max_bin_y,
            })
        })?;
        Ok(result)
    }

    fn notify_periodical_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let exp_data_mutex = self.exp_data.lock().unwrap();
        let is_exposure_now = exp_data_mutex.is_some();
        if let Some(exp_data) = &*exp_data_mutex {
            let eplased = exp_data.start_time.elapsed().as_secs_f64();
            let remaining = exp_data.duration - eplased;
            self.event_handlers.send(HalEvent::CameraTimeUntilEndOfExposure {
                device_id: Arc::clone(&self.device_id),
                time:      remaining.clamp(0.0, exp_data.duration)
            });
            drop(exp_data_mutex);

            let image_ready_result = self.async_runtime.block_on(async {
                self.camera.image_ready().await
            });

            match image_ready_result {
                Ok(ready) => {
                    if ready {
                        *self.exp_data.lock().unwrap() = None;
                        self.get_image_from_camera_and_send_event()?;
                    }
                }
                Err(err) => {
                    *self.exp_data.lock().unwrap() = None;
                    eyre::bail!("Error during wait of end of exposure: {}", err);
                }
            }
        }

        if !is_exposure_now {
            self.async_runtime.block_on(async {
                let mut prev_data = self.prev_data.lock().unwrap();

                // Check for CCD temparature change
                let temperature = self.camera.ccd_temperature().await.ok();
                if prev_data.temperature != temperature && let Some(temperature) = temperature {
                    self.event_handlers.send(HalEvent::CameraCcdTempChanged {
                        device_id: Arc::clone(&self.device_id),
                        temperature
                    });
                };
                prev_data.temperature = temperature;

                // Check for cooling power change
                if self.flags.contains(CameraFlags::CAN_GET_COOL_PWR) {
                    let cool_pwr = self.camera.cooler_power().await.ok();
                    if prev_data.cool_pwr != cool_pwr && let Some(cool_pwr) = cool_pwr {
                        self.event_handlers.send(HalEvent::CameraCoolerPwrChanged {
                            device_id: Arc::clone(&self.device_id),
                            power:     cool_pwr,
                        });
                    }
                    prev_data.cool_pwr = cool_pwr;
                }

                eyre::Ok(())
            })?;
        }

        Ok(())
    }

    fn get_image_from_camera_and_send_event(&self) -> eyre::Result<()> {
        self.event_handlers.send(HalEvent::CameraBeginDownloadData(
            Arc::clone(&self.device_id)
        ));

        let timer = std::time::Instant::now();
        let array = self.async_runtime.block_on(async {
            self.camera.image_array().await
        })?;
        let dl_time = timer.elapsed().as_secs_f64();
        let camera_shot = AscomAlpacaCameraShot { array, dl_time };

        self.event_handlers.send(HalEvent::CameraShotResult{
            device_id: Arc::clone(&self.device_id),
            shot:      Arc::new(camera_shot),
        });

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
        Ok(self.exp_range.clone())
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
        Ok((self.pixel_size_x, self.pixel_size_y))
    }

    fn is_frame_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::FRAME_SUPPORTED))
    }

    fn ccd_size(&self) -> eyre::Result<(usize, usize)> {
        Ok((self.ccd_size_x, self.ccd_size_y))
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
        Ok(self.gain_range.clone())
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
        Ok(self.offset_range.clone())
    }

    fn set_offset(&self, value: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            dbg!(value);
            self.camera.set_offset(value as i32).await?;
            eyre::Ok(())
        })
    }

    // Bin

    fn is_binning_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::BIN_SUPPORTED))
    }

    fn max_binning(&self) -> eyre::Result<(usize/*x*/, usize/*y*/)> {
        Ok((self.max_bin_x, self.max_bin_y))
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
