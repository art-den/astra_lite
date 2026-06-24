use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::AtomicBool;

use ascom_alpaca as aa;

use bitflags::bitflags;
use itertools::izip;

use crate::hal::{
    Camera, CameraInfo, CameraShot, CameraShotType, CcdPurpose, Device, DeviceInfo,
    DeviceType, FilterWheel, Focuser, FrameType, HalFeatures, HalImpl, HalState,
    Telescope, TelescopeMoveDir, TelescopeState,
};
use crate::hal::events::{HalEvent, HalEventHandlers};
use crate::image::raw::{CfaType, RawImage, RawImageInfo};

///////////////////////////////////////////////////////////////////////////////
// AscomAlpacaHalImpl

struct AscomAlpacaHalData {
    async_runtime:  Arc<tokio::runtime::Runtime>,
    client:         aa::Client,
    aa_devices:     Vec<aa::api::TypedDevice>,
    devices:        Vec<DeviceInfo>,
    cameras:        Vec<Arc<AscomAlpacaCamera>>,
    telescopes:     Vec<Arc<AscomAlpacaTelescope>>,
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
                if let aa::api::TypedDevice::Camera(dev) = typed_device { Some(dev) } else { None }
            })
            .filter_map(|aa_device| {
                AscomAlpacaCamera::from_aa_device(aa_device, &async_runtime, &self.event_handlers).ok()
            })
            .map(Arc::new)
            .collect();

        let telescopes = aa_devices
            .iter()
            .filter_map(|typed_device| {
                if let aa::api::TypedDevice::Telescope(dev) = typed_device { Some(dev) } else { None }
            })
            .filter_map(|aa_device| {
                AscomAlpacaTelescope::from_aa_device(aa_device, &async_runtime, &self.event_handlers).ok()
            })
            .map(Arc::new)
            .collect();


        let data = AscomAlpacaHalData {
            client,
            aa_devices,
            devices,
            async_runtime,
            cameras,
            telescopes,
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
            for telescope in &data.telescopes {
                telescope.notify_periodical_timer_tick(timer_period)?;
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
                *camera.device_id == id
            })
            .map(|camera| Arc::clone(camera) as Arc<dyn Camera + Send + Sync>)
            .ok_or_else(|| eyre::eyre!("Camera with id {id} not fount"))
    }

    fn telescope(&self, id: &str) -> eyre::Result<Arc<dyn Telescope + Send + Sync>> {
        let data = self.data()?;
        data.telescopes
            .iter()
            .find(|telescope| {
                *telescope.device_id == id
            })
            .map(|camera| Arc::clone(camera) as Arc<dyn Telescope + Send + Sync>)
            .ok_or_else(|| eyre::eyre!("Telescope with id {id} not fount"))
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
    array:        aa::api::camera::ImageArray,
    sensor_type:  aa::api::camera::SensorType,
    bayer_offset: Option<[u8; 2]>,
    start:        [u32; 2],
    max_adu:      u32,
    dl_time:      f64,
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

        let cfa_type = match self.sensor_type {
            aa::api::camera::SensorType::Monochrome =>
                CfaType::None,
            aa::api::camera::SensorType::RGGB if let Some(bayer_offset) = self.bayer_offset => {
                let bayer_offset_x = (self.start[0] + bayer_offset[0] as u32) % 2;
                let bayer_offset_y = (self.start[0] + bayer_offset[0] as u32) % 2;
                match (bayer_offset_x, bayer_offset_y) {
                    (0, 0) => CfaType::RGGB,
                    (1, 0) => CfaType::GRBG,
                    (0, 1) => CfaType::GBRG,
                    (1, 1) => CfaType::BGGR,
                    _ => unreachable!(),
                }
            }
            _ => eyre::bail!("Sensor type {:?} not supported", self.sensor_type),
        };

        let mut info = RawImageInfo::default();
        info.width = width;
        info.height = height;
        info.cfa = cfa_type;
        info.max_value = self.max_adu as _;

        let data_len = width * height;
        let mut data = vec![0; data_len];

        for row in 0..height {
            for (src, dst) in izip!(raw_2d_arr.column(row), &mut data[row * width..]) {
                *dst = (*src).clamp(0, u16::MAX as i32) as u16;
            }
        }

        let cfa_arrary = info.cfa.get_array();
        Ok(RawImage::new(info, data, cfa_arrary))
    }

    fn get_image(&self, _image: &mut crate::image::image::Image) -> eyre::Result<()> {
        todo!()
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

    fn save_to_file(&self, _file_name: &Path) -> eyre::Result<()> {
        Ok(())
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
        const CAN_GET_COOL_PWR  = (1 << 6);
        const CAN_GET_CCD_TEMP  = (1 << 7);
    }
}

struct ExposureData {
    duration:   f64,
    start_time: std::time::Instant,
}

#[derive(Default)]
struct CameraPrevData {
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
    prev_data:      Mutex<CameraPrevData>,
    bayer_offset:   Option<[u8; 2]>,
    sensor_type:    aa::api::camera::SensorType,
    max_adu:        u32,
}

impl AscomAlpacaCamera {
    fn from_aa_device(
        aa_camera:      &Arc<dyn aa::api::Camera>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {
        let mut camera_flags = CameraFlags::empty();
        let result = async_runtime.block_on(async {
            let max_bin_x = aa_camera.max_bin_x().await.unwrap_or(1) as usize;
            let max_bin_y = aa_camera.max_bin_y().await.unwrap_or(1) as usize;
            let bin_supported = usize::min(max_bin_x, max_bin_y) > 1;
            let cooler_supported = aa_camera.can_set_ccd_temperature().await.unwrap_or(false);
            let can_read_ccd_temp = aa_camera.ccd_temperature().await.is_ok();
            let can_stop_exposure = aa_camera.can_stop_exposure().await.unwrap_or(false);
            let can_get_cooler_power = aa_camera.can_get_cooler_power().await.unwrap_or(false);
            let exp_range = aa_camera.exposure_range().await?;
            let pixel_size_x = aa_camera.pixel_size_x().await?;
            let pixel_size_y = aa_camera.pixel_size_y().await?;
            let ccd_size_x = aa_camera.camera_x_size().await? as usize;
            let ccd_size_y = aa_camera.camera_y_size().await? as usize;
            let gain_supported = aa_camera.gain().await.is_ok();
            let min_gain = aa_camera.gain_min().await.unwrap_or(0) as f64;
            let max_gain = aa_camera.gain_max().await.unwrap_or(100_000) as f64;
            let offset_supported = aa_camera.offset().await.is_ok();
            let min_offset = aa_camera.offset_min().await.unwrap_or(0) as f64;
            let max_offset = aa_camera.offset_max().await.unwrap_or(65535) as f64;
            let sensor_type = aa_camera.sensor_type().await.unwrap_or(aa::api::camera::SensorType::Monochrome);
            let bayer_offset = aa_camera.bayer_offset().await.ok();
            //let max_adu = aa_camera.max_adu().await.unwrap_or(u16::MAX as _);
            let max_adu = u16::MAX as _;

            camera_flags.set(CameraFlags::FRAME_SUPPORTED, true);
            camera_flags.set(CameraFlags::GAIN_SUPPORTED, gain_supported);
            camera_flags.set(CameraFlags::OFFSET_SUPPORTED, offset_supported);
            camera_flags.set(CameraFlags::BIN_SUPPORTED, bin_supported);
            camera_flags.set(CameraFlags::COOLER_SUPPORTED, cooler_supported);
            camera_flags.set(CameraFlags::CAN_STOP_EXP, can_stop_exposure);
            camera_flags.set(CameraFlags::CAN_GET_COOL_PWR, can_get_cooler_power);
            camera_flags.set(CameraFlags::CAN_GET_CCD_TEMP, can_read_ccd_temp);

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
                prev_data:      Mutex::new(CameraPrevData::default()),
                pixel_size_x,
                pixel_size_y,
                ccd_size_x,
                ccd_size_y,
                max_bin_x,
                max_bin_y,
                sensor_type,
                bayer_offset,
                max_adu,
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
                if self.flags.contains(CameraFlags::CAN_GET_CCD_TEMP) {
                    let temperature = self.camera.ccd_temperature().await.ok();
                    if prev_data.temperature != temperature && let Some(temperature) = temperature {
                        self.event_handlers.send(HalEvent::CameraCcdTempChanged {
                            device_id: Arc::clone(&self.device_id),
                            temperature
                        });
                    }
                    prev_data.temperature = temperature;
                }

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
        let array_n_start = self.async_runtime.block_on(async {
            let array = self.camera.image_array().await?;
            let start = self.camera.start().await?;
            eyre::Ok((array, start))
        });

        *self.exp_data.lock().unwrap() = None;

        let (array, start) = array_n_start?;

        let dl_time = timer.elapsed().as_secs_f64();
        let camera_shot = AscomAlpacaCameraShot {
            sensor_type: self.sensor_type,
            bayer_offset: self.bayer_offset,
            max_adu: self.max_adu,
            start,
            array,
            dl_time,
        };

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
        let exposure_in_progress = self.exp_data.lock().unwrap().is_some();
        if exposure_in_progress {
            return Ok(true);
        }
        Ok(self.async_runtime.block_on(async {
            self.camera.connected().await
        })?)
    }
}

impl Camera for AscomAlpacaCamera {
    fn init_before_shot(&self) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            eyre::Ok(())
        })?;
        Ok(())
    }

    // Exposure

    fn exposure_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(self.exp_range.clone())
    }

    fn start_exposure(&self, duration: f64) -> eyre::Result<()> {
        *self.exp_data.lock().unwrap() = Some(ExposureData{
            duration,
            start_time: std::time::Instant::now(),
        });
        let result = self.async_runtime.block_on(async {
            let duration = std::time::Duration::from_secs_f64(duration);
            let is_light_frame = self.light_frame.load(std::sync::atomic::Ordering::Relaxed);
            self.camera.start_exposure(duration, is_light_frame).await?;
            eyre::Ok(())
        });
        if result.is_err() {
            *self.exp_data.lock().unwrap() = None;
        }
        result
    }

    fn abort_exposure(&self) -> eyre::Result<()> {
        if !self.flags.contains(CameraFlags::CAN_STOP_EXP) {
            return Ok(());
        }
        self.async_runtime.block_on(async {
            self.camera.stop_exposure().await?;
            eyre::Ok(())
        })?;

        *self.exp_data.lock().unwrap() = None;

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
            self.camera.set_start_y(y as u32).await?;
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


///////////////////////////////////////////////////////////////////////////////
// Telescope (mount)

struct AscomAlpacaTelescope {
    device:         Arc<dyn aa::api::Telescope>,
    device_id:      Arc<String>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
}

impl AscomAlpacaTelescope {
    fn from_aa_device(
        aa_telescope:   &Arc<dyn aa::api::Telescope>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {
        Ok(Self {
            device_id:      Arc::new(aa_telescope.unique_id().to_string()),
            device:         Arc::clone(aa_telescope),
            event_handlers: Arc::clone(event_handlers),
            async_runtime:  Arc::clone(async_runtime),
        })
    }

    fn notify_periodical_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        Ok(())
    }
}

impl Device for AscomAlpacaTelescope {
    fn id(&self) -> &str {
        self.device.unique_id()
    }

    fn name(&self) -> &str {
        self.device.static_name()
    }

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.async_runtime.block_on(async {
            self.device.connected().await
        })?)
    }
}

impl Telescope for AscomAlpacaTelescope {
    fn state(&self) -> eyre::Result<TelescopeState> {
        unimplemented!()
    }

    fn is_abort_motion_supported(&self) -> bool {
        unimplemented!()
    }

    fn abort_motion(&self) -> eyre::Result<()> {
        unimplemented!()
    }

    fn is_parked(&self) -> eyre::Result<bool> {
        unimplemented!()
    }

    fn park(&self) -> eyre::Result<()> {
        unimplemented!()
    }

    fn unpark(&self) -> eyre::Result<()> {
        unimplemented!()
    }

    fn is_tracking(&self) -> eyre::Result<bool> {
        unimplemented!()
    }

    fn track(&self, _enabled: bool) -> eyre::Result<()> {
        unimplemented!()
    }

    fn revert_motion(&self, _reverse_ns: bool, _reverse_we: bool) -> eyre::Result<()> {
        unimplemented!()
    }

    fn move_(&self, _direction: TelescopeMoveDir) -> eyre::Result<()> {
        unimplemented!()
    }

    fn slew_speed_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        unimplemented!()
    }

    fn set_slew_speed(&self, _speed_id: &str) -> eyre::Result<()> {
        unimplemented!()
    }

    fn eq_coord(&self) -> eyre::Result<(f64/*ra*/, f64/*dec*/)> {
        unimplemented!()
    }

    fn goto_and_track(&self, _ra: f64, _dec: f64) -> eyre::Result<()> {
        unimplemented!()
    }

    fn is_slewing(&self) -> eyre::Result<bool> {
        unimplemented!()
    }

    fn sync(&self, _ra: f64, _dec: f64) -> eyre::Result<()> {
        unimplemented!()
    }

    fn is_guide_rate_supported(&self) -> eyre::Result<bool> {
        unimplemented!()
    }

    fn guide_rate(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)> {
        unimplemented!()
    }

    fn pulse_max_duration(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)> {
        unimplemented!()
    }

    fn can_set_guide_rate(&self) -> eyre::Result<bool> {
        unimplemented!()
    }

    fn set_guide_rate(&self, _rate_ns: f64, _rate_we: f64) -> eyre::Result<()> {
        unimplemented!()
    }

    fn pulse_guide(&self, _duration_ns: f64, _duration_we: f64) -> eyre::Result<()> {
        unimplemented!()
    }

    fn is_pulse_guiding(&self) -> eyre::Result<bool> {
        unimplemented!()
    }
}
