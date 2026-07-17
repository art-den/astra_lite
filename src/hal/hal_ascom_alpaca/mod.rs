use std::io::{BufWriter, Write};
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::fs::File;

use ascom_alpaca as aa;
use bitflags::bitflags;
use itertools::izip;

use crate::hal::*;
use crate::hal::events::{HalEvent, HalEventHandlers};
use crate::image::raw::{CfaType, RawImage, RawImageInfo};
use crate::image::simple_fits::{FitsWriter, Header};

const SIDERAL_RATE_DEG_PER_SEC: f64 = 360.0 / (23.0 * 60.0 * 60.0 + 56.0 * 60.0 + 4.09);

///////////////////////////////////////////////////////////////////////////////
// AscomAlpacaHalImpl

struct AscomAlpacaHalData {
    async_runtime:  Arc<tokio::runtime::Runtime>,
    client:         aa::Client,
    aa_devices:     Vec<aa::api::TypedDevice>,
    devices:        Vec<DeviceInfo>,
    cameras:        Vec<Arc<AscomAlpacaCamera>>,
    telescopes:     Vec<Arc<AscomAlpacaTelescope>>,
    focusers:       Vec<Arc<AscomAlpacaFocuser>>,
    filter_wheels:  Vec<Arc<AscomAlpacaFilterWheel>>,
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
                        (dev.unique_id(), dev.static_name(), DeviceType::FLT_WHEEL),
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
                AscomAlpacaCamera::new(aa_device, &async_runtime, &self.event_handlers).ok()
            })
            .map(Arc::new)
            .collect();

        let telescopes = aa_devices
            .iter()
            .filter_map(|typed_device| {
                if let aa::api::TypedDevice::Telescope(dev) = typed_device { Some(dev) } else { None }
            })
            .filter_map(|aa_device| {
                AscomAlpacaTelescope::new(aa_device, &async_runtime, &self.event_handlers).ok()
            })
            .map(Arc::new)
            .collect();

        let focusers = aa_devices
            .iter()
            .filter_map(|typed_device| {
                if let aa::api::TypedDevice::Focuser(dev) = typed_device { Some(dev) } else { None }
            })
            .filter_map(|aa_device| {
                AscomAlpacaFocuser::new(aa_device, &async_runtime, &self.event_handlers).ok()
            })
            .map(Arc::new)
            .collect();

        let filter_wheels = aa_devices
            .iter()
            .filter_map(|typed_device| {
                if let aa::api::TypedDevice::FilterWheel(dev) = typed_device { Some(dev) } else { None }
            })
            .filter_map(|aa_device| {
                AscomAlpacaFilterWheel::new(aa_device, &async_runtime, &self.event_handlers).ok()
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
            focusers,
            filter_wheels,
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

    fn data_opt(&self) -> Option<Arc<AscomAlpacaHalData>> {
        self.data.read().unwrap().clone()
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
            }
            for camera in &data.cameras {
                self.event_handlers.send(
                    HalEvent::CameraIsReadyToWork(Arc::clone(&camera.device_id))
                );
                self.event_handlers.send(
                    HalEvent::CameraIsReadyForCooling(Arc::clone(&camera.device_id))
                );
                self.event_handlers.send(
                    HalEvent::CameraOffsetCanBeControlled(Arc::clone(&camera.device_id))
                );
                self.event_handlers.send(
                    HalEvent::CameraGainCanBeControlled(Arc::clone(&camera.device_id))
                );
            }
            for telescope in &data.telescopes {
                self.event_handlers.send(
                    HalEvent::TelescopeSlewRateListReady(Arc::clone(&telescope.device_id))
                );
            }
            for focuser in &data.focusers {
                if let Ok(abs_value) = focuser.abs_position() {
                    self.event_handlers.send(
                        HalEvent::FocuserAbsValueCanBeControlled {
                            device_id: Arc::clone(&focuser.device_id),
                            abs_value,
                        }
                    );
                }
            }
            for filter_wheel in &data.filter_wheels {
                self.event_handlers.send(
                    HalEvent::FilterWheelNameChanged(Arc::clone(&filter_wheel.device_id))
                );
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
            for focuser in &data.focusers {
                focuser.notify_periodical_timer_tick(timer_period)?;
            }
            for filter_wheel in &data.filter_wheels {
                filter_wheel.notify_periodical_timer_tick(timer_period)?;
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
                    id:   cam.device.unique_id().to_string(),
                    name: cam.device.static_name().to_string(),
                    ccd:  CcdPurpose::Unknown,
                }
            })
            .collect();
        Ok(result)
    }

    fn camera(&self, id: &str) -> Option<Arc<dyn Camera + Send + Sync>> {
        let data = self.data_opt()?;
        data.cameras
            .iter()
            .find(|device| *device.device_id == id)
            .map(|device| Arc::clone(device) as Arc<dyn Camera + Send + Sync>)
    }

    fn telescope(&self, id: &str) -> Option<Arc<dyn Telescope + Send + Sync>> {
        let data = self.data_opt()?;
        data.telescopes
            .iter()
            .find(|device| *device.device_id == id)
            .map(|device| Arc::clone(device) as Arc<dyn Telescope + Send + Sync>)
    }

    fn focuser(&self, id: &str) -> Option<Arc<dyn Focuser + Send + Sync>> {
        let data = self.data_opt()?;
        data.focusers
            .iter()
            .find(|device| *device.device_id == id)
            .map(|device| Arc::clone(device) as Arc<dyn Focuser + Send + Sync>)
    }

    fn filter_wheel(&self, id: &str) -> Option<Arc<dyn FilterWheel + Send + Sync>> {
        let data = self.data_opt()?;
        data.filter_wheels
            .iter()
            .find(|device| *device.device_id == id)
            .map(|device| Arc::clone(device) as Arc<dyn FilterWheel + Send + Sync>)
    }
}

//////////////////////////////////////////////////////////////////////////////
// CameraShot

struct AscomAlpacaCameraShot {
    array:          aa::api::camera::ImageArray,
    sensor_type:    aa::api::camera::SensorType,
    dl_time:        f64,
    raw_image_info: RawImageInfo,
}

impl AscomAlpacaCameraShot {
    fn new(
        async_runtime: &tokio::runtime::Runtime,
        aa_camera:     &Arc<dyn aa::api::Camera>,
        bayer_offset:  Option<[u8; 2]>,
        sensor_type:   aa::api::camera::SensorType,
        max_adu:       u32,
        frame_type:    Option<FrameType>,
        exposure:      f64,
    ) -> eyre::Result<Self> {
        async_runtime.block_on(async {
            let timer = std::time::Instant::now();
            let array = aa_camera.image_array().await?;
            let start = aa_camera.start().await?;
            let dl_time = timer.elapsed().as_secs_f64();

            let raw_2d_arr = array.index_axis(ndarray::Axis(2), 0);
            let (width, height) = raw_2d_arr.dim();

            let cfa_type = match sensor_type {
                aa::api::camera::SensorType::Monochrome =>
                    CfaType::None,
                aa::api::camera::SensorType::RGGB if let Some(bayer_offset) = bayer_offset => {
                    let bayer_offset_x = (start[0] + bayer_offset[0] as u32) % 2;
                    let bayer_offset_y = (start[0] + bayer_offset[0] as u32) % 2;
                    match (bayer_offset_x, bayer_offset_y) {
                        (0, 0) => CfaType::RGGB,
                        (1, 0) => CfaType::GRBG,
                        (0, 1) => CfaType::GBRG,
                        (1, 1) => CfaType::BGGR,
                        _ => unreachable!(),
                    }
                }
                _ => eyre::bail!("Sensor type {:?} not supported", sensor_type),
            };

            let mut image_info = RawImageInfo::default();
            image_info.width = width;
            image_info.height = height;
            image_info.cfa = cfa_type;
            image_info.max_value = max_adu as _;
            image_info.frame_type = frame_type.unwrap_or(FrameType::Lights);
            image_info.camera = aa_camera.static_name().to_string();
            image_info.gain = aa_camera.gain().await.unwrap_or(0) as _;
            image_info.offset = aa_camera.offset().await.unwrap_or(0);
            image_info.exposure = exposure;
            image_info.bin = aa_camera.bin_x().await.unwrap_or(0);
            image_info.ccd_temp = aa_camera.ccd_temperature().await.ok();

            Ok(Self { array, sensor_type, dl_time, raw_image_info: image_info })
        })
    }

    fn save_raw_file(&self, file_name: &Path) -> eyre::Result<()> {
        let raw_2d_arr = self.array.index_axis(ndarray::Axis(2), 0);
        let info = &self.raw_image_info;

        let mut file = BufWriter::new(File::create(file_name)?);
        let writer = FitsWriter::new();
        let mut hdu = Header::new_2d(info.width, info.height);
        info.save_to_fits_header(&mut hdu);
        writer.write_header(&mut file, &hdu)?;

        for row in 0..info.height {
            for src in raw_2d_arr.column(row) {
                let value = (*src).clamp(0, u16::MAX as i32) as u16;
                file.write_all(&value.to_be_bytes())?;
            }
        }

        Ok(())
    }

    fn save_image(&self, _file_name: &Path) -> eyre::Result<()> {
        todo!()
    }
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

        for row in 0..height {
            for (src, dst) in izip!(raw_2d_arr.column(row), &mut data[row * width..]) {
                *dst = (*src).clamp(0, u16::MAX as i32) as u16;
            }
        }

        let cfa_arrary = self.raw_image_info.cfa.get_array();
        Ok(RawImage::new(self.raw_image_info.clone(), data, cfa_arrary))
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

    fn save_to_file(&self, file_name: &Path) -> eyre::Result<()> {
        match self.get_type() {
            CameraShotType::RawCcdData =>
                self.save_raw_file(file_name),
            CameraShotType::ReadyImage =>
                self.save_image(file_name),
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
        const CAN_GET_COOL_PWR  = (1 << 6);
        const CAN_GET_CCD_TEMP  = (1 << 7);
    }
}

struct ExposureData {
    duration:   f64,
    start_time: std::time::Instant,
}

#[derive(Default)]
struct CameraDynData {
    prev_temperature: Option<f64>,
    prev_cool_pwr:    Option<f64>,
    frame_type:       Option<FrameType>,
    exposure:         f64,
}

struct AscomAlpacaCamera {
    device:         Arc<dyn aa::api::Camera>,
    device_id:      Arc<String>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
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
    dyn_data:       Mutex<CameraDynData>,
    bayer_offset:   Option<[u8; 2]>,
    sensor_type:    aa::api::camera::SensorType,
    max_adu:        u32,
}

impl AscomAlpacaCamera {
    fn new(
        aa_camera:      &Arc<dyn aa::api::Camera>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {

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

            let mut camera_flags = CameraFlags::empty();
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
                device:         Arc::clone(aa_camera),
                event_handlers: Arc::clone(event_handlers),
                async_runtime:  Arc::clone(async_runtime),
                exp_data:       Mutex::new(None),
                flags:          camera_flags,
                exp_range:      exp_range.start().as_secs_f64() ..= exp_range.end().as_secs_f64(),
                gain_range:     min_gain ..= max_gain,
                offset_range:   min_offset ..= max_offset,
                dyn_data:       Mutex::new(CameraDynData::default()),
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
                self.device.image_ready().await
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
                let mut data = self.dyn_data.lock().unwrap();

                // Check for CCD temparature change
                if self.flags.contains(CameraFlags::CAN_GET_CCD_TEMP) {
                    let temperature = self.device.ccd_temperature().await.ok();
                    if data.prev_temperature != temperature && let Some(temperature) = temperature {
                        self.event_handlers.send(HalEvent::CameraCcdTempChanged {
                            device_id: Arc::clone(&self.device_id),
                            temperature
                        });
                    }
                    data.prev_temperature = temperature;
                }

                // Check for cooling power change
                if self.flags.contains(CameraFlags::CAN_GET_COOL_PWR) {
                    let cool_pwr = self.device.cooler_power().await.ok();
                    if data.prev_cool_pwr != cool_pwr && let Some(cool_pwr) = cool_pwr {
                        self.event_handlers.send(HalEvent::CameraCoolerPwrChanged {
                            device_id: Arc::clone(&self.device_id),
                            power:     cool_pwr,
                        });
                    }
                    data.prev_cool_pwr = cool_pwr;
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

        let data = self.dyn_data.lock().unwrap();
        let frame_type = data.frame_type;
        let exposure = data.exposure;
        drop(data);

        let camera_shot = AscomAlpacaCameraShot::new(
            &self.async_runtime,
            &self.device,
            self.bayer_offset,
            self.sensor_type,
            self.max_adu,
            frame_type,
            exposure,
        );

        *self.exp_data.lock().unwrap() = None;

        self.event_handlers.send(HalEvent::CameraShotResult{
            device_id: Arc::clone(&self.device_id),
            shot:      Arc::new(camera_shot?),
        });

        Ok(())
    }
}

impl Device for AscomAlpacaCamera {
    fn id(&self) -> &str {
        self.device.unique_id()
    }

    fn name(&self) -> &str {
        self.device.static_name()
    }

    fn is_active(&self) -> eyre::Result<bool> {
        let exposure_in_progress = self.exp_data.lock().unwrap().is_some();
        if exposure_in_progress {
            return Ok(true);
        }
        Ok(self.async_runtime.block_on(async {
            self.device.connected().await
        })?)
    }
}

impl Camera for AscomAlpacaCamera {
    fn features(&self) -> CameraFeatures {
        CameraFeatures::empty()
    }

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
            let frame_type = self.dyn_data.lock().unwrap().frame_type.unwrap_or(FrameType::Lights);
            self.device.start_exposure(duration, frame_type == FrameType::Lights).await?;
            eyre::Ok(())
        });
        if result.is_err() {
            *self.exp_data.lock().unwrap() = None;
        }
        self.dyn_data.lock().unwrap().exposure = duration;
        result
    }

    fn abort_exposure(&self) -> eyre::Result<()> {
        if !self.flags.contains(CameraFlags::CAN_STOP_EXP) {
            return Ok(());
        }
        self.async_runtime.block_on(async {
            self.device.stop_exposure().await?;
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
        self.dyn_data.lock().unwrap().frame_type = Some(frame_type);
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
            self.device.set_start_x(x as u32).await?;
            self.device.set_start_y(y as u32).await?;
            self.device.set_num_x(width as u32).await?;
            self.device.set_num_y(height as u32).await?;
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
            self.device.set_gain(value as i32).await?;
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
            self.device.set_offset(value as i32).await?;
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
            self.device.set_bin_x(bin_x as u8).await?;
            self.device.set_bin_y(bin_y as u8).await?;
            eyre::Ok(())
        })
    }

    // Cooler

    fn is_cooler_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(CameraFlags::COOLER_SUPPORTED))
    }

    fn temperature(&self) -> eyre::Result<f64> {
        self.async_runtime.block_on(async {
            let result = self.device.ccd_temperature().await?;
            eyre::Ok(result)
        })
    }

    fn temperature_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(-100.0 ..= 50.0)
   }

    fn set_temperature(&self, temperature: Option<f64>) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            if let Some(temperature) = temperature {
                self.device.set_set_ccd_temperature(temperature).await?;
                self.device.set_cooler_on(true).await?;
            } else {
                self.device.set_cooler_on(false).await?;
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

bitflags! {
    struct TeleasopeFlags: u32 {
        const GUIDE_RATE_SUPPORTED = (1 << 0);
        const CAN_SET_GUIDE_RATE   = (1 << 1);
    }
}

struct TelescopeData {
    ns_reverted: bool,
    we_reverted: bool,
    axis_rate: f64, // deg. in second
    prev_state: Option<TelescopeState>,
    prev_tracking: Option<bool>,
    prev_parked: Option<bool>,
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

struct StateInternal {
    state:       TelescopeState,
    is_tracking: bool,
    is_parked:   bool,
}

struct AscomAlpacaTelescope {
    device:         Arc<dyn aa::api::Telescope>,
    device_id:      Arc<String>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
    move_rates:     Vec<(String, f64)>,
    flags:          TeleasopeFlags,
    data:           Mutex<TelescopeData>,
}

impl AscomAlpacaTelescope {
    fn new(
        aa_telescope:   &Arc<dyn aa::api::Telescope>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {
        let result = async_runtime.block_on(async {
            let move_prim_axis_rates = aa_telescope.axis_rates(aa::api::telescope::TelescopeAxis::Primary)
                .await
                .unwrap_or_default();
            let move_sec_axis_rates = aa_telescope.axis_rates(aa::api::telescope::TelescopeAxis::Secondary)
                .await
                .unwrap_or_default();

            let prim_max_rate = move_prim_axis_rates.iter()
                .map(|range| range.end())
                .max_by(|x, y| f64::partial_cmp(x, y).unwrap_or(std::cmp::Ordering::Equal))
                .copied()
                .unwrap_or(SIDERAL_RATE_DEG_PER_SEC);
            let sec_max_rate = move_sec_axis_rates.iter()
                .map(|range| range.end())
                .max_by(|x, y| f64::partial_cmp(x, y).unwrap_or(std::cmp::Ordering::Equal))
                .copied()
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

            let guide_rate_supported = aa_telescope.guide_rates_ra_dec().await.is_ok();
            let can_set_guide_rate = aa_telescope.can_set_guide_rates().await.unwrap_or(false);
            let mut flags = TeleasopeFlags::empty();
            flags.set(TeleasopeFlags::GUIDE_RATE_SUPPORTED, guide_rate_supported);
            flags.set(TeleasopeFlags::CAN_SET_GUIDE_RATE, can_set_guide_rate);

            eyre::Ok(Self {
                device_id:      Arc::new(aa_telescope.unique_id().to_string()),
                device:         Arc::clone(aa_telescope),
                event_handlers: Arc::clone(event_handlers),
                async_runtime:  Arc::clone(async_runtime),
                move_rates:     move_rates,
                data:           Mutex::new(TelescopeData::default()),
                flags,
            })
        })?;

        Ok(result)
    }

    fn notify_periodical_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        let state = if let Ok(state) = self.state_internal() {
            state
        } else {
            StateInternal {
                state: TelescopeState::Error, is_parked: false, is_tracking: false,
            }
        };
        let mut data = self.data.lock().unwrap();
        let state_changed = data.prev_state != Some(state.state);
        let tracking_changed = data.prev_tracking != Some(state.is_tracking);
        let parked_changed = data.prev_parked != Some(state.is_parked);
        data.prev_state = Some(state.state);
        data.prev_tracking = Some(state.is_tracking);
        data.prev_parked = Some(state.is_parked);
        drop(data);

        if state_changed {
            self.event_handlers.send(HalEvent::TelescopeStateChanged {
                device_id: Arc::clone(&self.device_id),
                state:     state.state,
            });
        }
        if tracking_changed {
            self.event_handlers.send(HalEvent::TelescopeTrackingChanged {
                device_id: Arc::clone(&self.device_id),
                tracking:  state.is_tracking,
            });
        }
        if parked_changed {
            self.event_handlers.send(
                if state.is_parked {
                    HalEvent::TelescopeParked(Arc::clone(&self.device_id))
                } else {
                    HalEvent::TelescopeUnparked(Arc::clone(&self.device_id))
                }
            );
        }
        Ok(())
    }

    fn state_internal(&self) -> eyre::Result<StateInternal> {
        self.async_runtime.block_on(async {
            let is_tracking = self.device.tracking().await?;
            let is_parked = self.device.at_park().await?;
            let is_slewing = self.device.slewing().await?;
            let is_pulse_guiding = self.device.is_pulse_guiding().await?;
            let state = if is_parked {
                TelescopeState::Parked
            } else if is_pulse_guiding {
                TelescopeState::Correction
            } else if is_slewing {
                TelescopeState::Slewing
            } else if is_tracking {
                TelescopeState::Tracking
            } else {
                TelescopeState::Stopped
            };
            eyre::Ok(StateInternal {
                state, is_tracking, is_parked
            })
        })
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
        Ok(self.state_internal()?.state)
    }

    fn site(&self) -> eyre::Result<TelescopeSite> {
        self.async_runtime.block_on(async {
            let latitude = self.device.site_latitude().await?;
            let longitude = self.device.site_longitude().await?;
            let elevation = self.device.site_elevation().await?;
            eyre::Ok(TelescopeSite { latitude, longitude, elevation })
        })
    }

    fn is_abort_motion_supported(&self) -> bool {
        true
    }

    fn abort_motion(&self) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            _ = self.device.abort_slew().await;
            _ = self.device.move_axis(aa::api::telescope::TelescopeAxis::Primary, 0.0).await;
            _ = self.device.move_axis(aa::api::telescope::TelescopeAxis::Secondary, 0.0).await;
            _ = self.device.move_axis(aa::api::telescope::TelescopeAxis::Tertiary, 0.0).await;
            eyre::Ok(())
        })
    }

    fn is_parked(&self) -> eyre::Result<bool> {
        Ok(self.async_runtime.block_on(async {
            self.device.at_park().await
        })?)
    }

    fn park(&self) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.device.park().await?;
            eyre::Ok(())
        })
    }

    fn unpark(&self) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.device.unpark().await?;
            eyre::Ok(())
        })
    }

    fn is_tracking(&self) -> eyre::Result<bool> {
        Ok(self.async_runtime.block_on(async {
            self.device.tracking().await
        })?)
    }

    fn track(&self, enabled: bool) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.device.set_tracking(enabled).await?;
            eyre::Ok(())
        })
    }

    fn revert_motion(&self, reverse_ns: bool, reverse_we: bool) -> eyre::Result<()> {
        let mut data = self.data.lock().unwrap();
        data.ns_reverted = reverse_ns;
        data.we_reverted = reverse_we;
        Ok(())
    }

    fn move_(&self, direction: TelescopeMoveDir) -> eyre::Result<()> {
        use aa::api::telescope::TelescopeAxis;

        let data = self.data.lock().unwrap();
        let axis_rate = data.axis_rate;
        let ns_reverted = data.ns_reverted;
        let we_reverted = data.we_reverted;
        drop(data);

        let move_prim_axis = async |mut rate: f64| -> eyre::Result<()> {
            if we_reverted {
                rate = -rate;
            }
            self.device.move_axis(TelescopeAxis::Primary, rate).await?;
            Ok(())
        };

        let move_sec_axis = async |mut rate: f64| -> eyre::Result<()> {
            if ns_reverted {
                rate = -rate;
            }
            self.device.move_axis(TelescopeAxis::Secondary, rate).await?;
            Ok(())
        };

        self.async_runtime.block_on(async {
            match direction {
                TelescopeMoveDir::North => {
                    move_sec_axis(axis_rate).await?;
                }
                TelescopeMoveDir::South => {
                    move_sec_axis(-axis_rate).await?;
                }
                TelescopeMoveDir::West => {
                    move_prim_axis(axis_rate).await?;
                }
                TelescopeMoveDir::East => {
                    move_prim_axis(-axis_rate).await?;
                }
                TelescopeMoveDir::NorthWest => {
                    move_sec_axis(axis_rate).await?;
                    move_prim_axis(axis_rate).await?;
                }
                TelescopeMoveDir::NorthEast => {
                    move_sec_axis(axis_rate).await?;
                    move_prim_axis(-axis_rate).await?;
                }
                TelescopeMoveDir::SouthWest => {
                    move_sec_axis(-axis_rate).await?;
                    move_prim_axis(axis_rate).await?;
                }
                TelescopeMoveDir::SouthEast => {
                    move_sec_axis(-axis_rate).await?;
                    move_prim_axis(-axis_rate).await?;
                }
            }
            eyre::Ok(())
        })
    }

    fn slew_speed_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        let list = self.move_rates
            .iter()
            .map(|(name, _)| (name.to_string(), name.to_string()))
            .collect();
        Ok(list)
    }

    fn set_slew_speed(&self, speed_id: &str) -> eyre::Result<()> {
        let item = self.move_rates
            .iter()
            .find(|(name, _)| name == speed_id);
        if let Some((_, rate)) = item {
            let mut data = self.data.lock().unwrap();
            data.axis_rate = *rate;
        };
        Ok(())
    }

    fn eq_coord(&self) -> eyre::Result<(f64/*ra*/, f64/*dec*/)> {
        self.async_runtime.block_on(async {
            let ra = self.device.right_ascension().await?;
            let dec = self.device.declination().await?;
            eyre::Ok((ra, dec))
        })
    }

    fn goto_and_track(&self, ra: f64, dec: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.device.slew_to_coordinates_async(ra, dec).await?;
            eyre::Ok(())
        })
    }

    fn is_slewing(&self) -> eyre::Result<bool> {
        Ok(self.async_runtime.block_on(async {
            self.device.slewing().await
        })?)
    }

    fn sync(&self, ra: f64, dec: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.device.sync_to_coordinates(ra, dec).await?;
            eyre::Ok(())
        })
    }

    fn is_guide_rate_supported(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(TeleasopeFlags::GUIDE_RATE_SUPPORTED))
    }

    fn guide_rate(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)> {
        let rates = self.async_runtime.block_on(async {
            self.device.guide_rates_ra_dec().await
        })?;
        Ok((
            rates.right_ascension / SIDERAL_RATE_DEG_PER_SEC,
            rates.declination / SIDERAL_RATE_DEG_PER_SEC,
        ))
    }

    fn pulse_max_duration(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)> {
        Ok((3000.0, 3000.0))
    }

    fn can_set_guide_rate(&self) -> eyre::Result<bool> {
        Ok(self.flags.contains(TeleasopeFlags::CAN_SET_GUIDE_RATE))
    }

    fn set_guide_rate(&self, rate_ns: f64, rate_we: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            let crd = aa::api::telescope::RaDec {
                right_ascension: rate_we * SIDERAL_RATE_DEG_PER_SEC,
                declination:     rate_ns * SIDERAL_RATE_DEG_PER_SEC,
            };
            self.device.set_guide_rates_ra_dec(crd).await?;
            eyre::Ok(())
        })
    }

    fn pulse_guide(&self, duration_ns: f64, duration_we: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            if duration_ns < 0.0 {
                let duration = std::time::Duration::from_millis(duration_ns.abs() as _);
                self.device.pulse_guide(aa::api::telescope::GuideDirection::North, duration).await?;
            }
            if duration_ns > 0.0 {
                let duration = std::time::Duration::from_millis(duration_ns.abs() as _);
                self.device.pulse_guide(aa::api::telescope::GuideDirection::South, duration).await?;
            }
            if duration_we < 0.0 {
                let duration = std::time::Duration::from_millis(duration_we.abs() as _);
                self.device.pulse_guide(aa::api::telescope::GuideDirection::West, duration).await?;
            }
            if duration_we > 0.0 {
                let duration = std::time::Duration::from_millis(duration_we.abs() as _);
                self.device.pulse_guide(aa::api::telescope::GuideDirection::East, duration).await?;
            }
            eyre::Ok(())
        })
    }

    fn is_pulse_guiding(&self) -> eyre::Result<bool> {
        Ok(self.async_runtime.block_on(async {
            self.device.is_pulse_guiding().await
        })?)
    }
}

///////////////////////////////////////////////////////////////////////////////
// Focuser

#[derive(Default)]
struct FocuserData {
    prev_state: Option<FocuserState>,
    prev_pos:   Option<i32>,
    prev_temp:  Option<f64>,
}

struct AscomAlpacaFocuser {
    device:         Arc<dyn aa::api::Focuser>,
    device_id:      Arc<String>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
    range:          RangeInclusive<f64>,
    data:           Mutex<FocuserData>,
}

impl AscomAlpacaFocuser {
    fn new(
        aa_focuser:     &Arc<dyn aa::api::Focuser>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {
        let result = async_runtime.block_on(async {
            let max_pos = aa_focuser.max_step().await.unwrap_or(0);
            eyre::Ok(Self {
                device:         Arc::clone(aa_focuser),
                device_id:      Arc::new(aa_focuser.unique_id().to_string()),
                async_runtime:  Arc::clone(&async_runtime),
                event_handlers: Arc::clone(&event_handlers),
                range:          0.0 ..= max_pos as f64,
                data:           Mutex::new(FocuserData::default()),
            })
        })?;
        Ok(result)
    }

    fn notify_periodical_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            let state = self.state_impl().await;
            let pos = self.device.position().await.unwrap_or(-1);
            let temperature = self.device.temperature().await.unwrap_or(25.0);
            let mut data = self.data.lock().unwrap();
            let state_changed = data.prev_state != Some(state);
            let pos_changed = data.prev_pos != Some(pos);
            let temp_changed = data.prev_temp != Some(temperature);
            data.prev_state = Some(state);
            data.prev_pos = Some(pos);
            data.prev_temp = Some(temperature);
            drop(data);

            if state_changed {
                self.event_handlers.send(HalEvent::FocuserStateChanged {
                    device_id: Arc::clone(&self.device_id),
                    state,
                });
            }
            if pos_changed {
                self.event_handlers.send(HalEvent::FocuserAbsValueChanged {
                    device_id: Arc::clone(&self.device_id),
                    abs_value: pos as f64,
                });
            }
            if temp_changed {
                self.event_handlers.send(HalEvent::FocuserTemperatureChanged {
                    device_id: Arc::clone(&self.device_id),
                    temperature,
                });
            }
            eyre::Ok(())
        })
    }

    async fn state_impl(&self) -> FocuserState {
        match self.device.is_moving().await {
            Ok(true)  => FocuserState::Moving,
            Ok(false) => FocuserState::Stopped,
            Err(_)    => FocuserState::Error,
        }
    }
}

impl Device for AscomAlpacaFocuser {
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

impl Focuser for AscomAlpacaFocuser {
    fn state(&self) -> eyre::Result<FocuserState> {
        Ok(self.async_runtime.block_on(self.state_impl()))
    }

    fn abs_position_range(&self) -> eyre::Result<RangeInclusive<f64>> {
        Ok(self.range.clone())
    }

    fn abs_position(&self) -> eyre::Result<f64> {
        Ok(self.async_runtime.block_on(async {
            self.device.position().await.map(|v| v as f64)
        })?)
    }

    fn set_abs_position(&self, value: f64) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            self.device.move_(value as i32).await?;
            eyre::Ok(())
        })
    }

    fn temperature(&self) -> eyre::Result<f64> {
        Ok(self.async_runtime.block_on(async {
            self.device.temperature().await
        })?)
    }
}


///////////////////////////////////////////////////////////////////////////////
// Filter wheel

#[derive(Default)]
struct FilterWheelData {
    prev_pos: Option<usize>,
}

struct AscomAlpacaFilterWheel {
    device:         Arc<dyn aa::api::FilterWheel>,
    device_id:      Arc<String>,
    async_runtime:  Arc<tokio::runtime::Runtime>,
    event_handlers: Arc<HalEventHandlers>,
    data:           Mutex<FilterWheelData>,
}

impl AscomAlpacaFilterWheel {
    fn new(
        aa_filterwheel: &Arc<dyn aa::api::FilterWheel>,
        async_runtime:  &Arc<tokio::runtime::Runtime>,
        event_handlers: &Arc<HalEventHandlers>
    ) -> eyre::Result<Self> {
        let result = async_runtime.block_on(async {
            eyre::Ok(Self {
                device:         Arc::clone(aa_filterwheel),
                device_id:      Arc::new(aa_filterwheel.unique_id().to_string()),
                async_runtime:  Arc::clone(&async_runtime),
                event_handlers: Arc::clone(&event_handlers),
                data:           Mutex::new(FilterWheelData::default()),
            })
        })?;
        Ok(result)
    }

    fn notify_periodical_timer_tick(&self, _timer_period: usize) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            let pos = self.device.position().await.unwrap_or_default();
            let mut data = self.data.lock().unwrap();
            let pos_changed = data.prev_pos != pos;
            data.prev_pos = pos;
            drop(data);
            if pos_changed {
                self.event_handlers.send(HalEvent::FilterWheelSlotChange {
                    device_id: Arc::clone(&self.device_id),
                    slot:      pos.map(|v| v as i32),
                });
            }
            eyre::Ok(())
        })
    }
}

impl Device for AscomAlpacaFilterWheel {
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

impl FilterWheel for AscomAlpacaFilterWheel {
    fn list_and_active(&self) -> eyre::Result<(Vec<String>, usize)> {
        Ok(self.async_runtime.block_on(async {
            let names = self.device.names().await?;
            let pos = self.device.position()
                .await?
                .ok_or_else(|| eyre::eyre!("Position is not acessible now"))?;
            eyre::Ok((names, pos))
        })?)
    }

    fn set_active(&self, active_elem: usize) -> eyre::Result<()> {
        self.async_runtime.block_on(async {
            if self.device.position().await.unwrap_or_default() == Some(active_elem) {
                return eyre::Ok(());
            }
            self.device.set_position(active_elem).await?;
            self.event_handlers.send(HalEvent::FilterWheelSlotChange {
                device_id: Arc::clone(&self.device_id),
                slot:      None,
            });
            eyre::Ok(())
        })
    }
}
