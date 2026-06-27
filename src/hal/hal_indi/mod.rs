use std::{io::Cursor, ops::RangeInclusive, sync::{Arc, Mutex}};
use itertools::Itertools;
use crate::{
    hal::{
        events::*, hal_indi::{camera_watchdog::CamWatchdog, dev_watchdog::DevicesWatchdog},
        indi::{ConnSettings, EventHandlerId},
        *,
    },
    image::{
        io::{find_color_image_hdu_in_fits, find_mono_image_hdu_in_fits, load_raw_image_from_fits_reader},
        simple_fits::FitsReader,
    },
};
use super::{indi, HalImpl, Camera, DeviceInfo, DeviceType};

pub const CAM_CCD2_POSTFIX: &str = "_CCD2";
pub const SET_PROP_TIME_OUT: u64 = 2000; // ms

mod dev_watchdog;
mod camera_watchdog;

struct Watchdogs {
    camera:  CamWatchdog,
    devices: DevicesWatchdog,
}

///////////////////////////////////////////////////////////////////////////////
// IndiHalImpl

pub struct IndiHalImpl {
    indi:            Arc<indi::Connection>,
    conn_settings:   Mutex<indi::ConnSettings>,
    event_handlers:  Arc<HalEventHandlers>,
    indi_evt_subscr: Mutex<Option<EventHandlerId>>,
    watchdogs:       Mutex<Watchdogs>,
    indi_drivers:    indi::Drivers,
}

impl IndiHalImpl {
    pub fn new(event_handlers: &Arc<HalEventHandlers>) -> Arc<Self> {
        let (drivers, _) =
            if cfg!(target_os = "windows") {
                (indi::Drivers::new_empty(), None)
            } else {
                match indi::Drivers::new() {
                    Ok(drivers) =>
                        (drivers, None),
                    Err(err) =>
                        (indi::Drivers::new_empty(), Some(err.to_string())),
                }
            };

        let indi = Arc::new(indi::Connection::new());

        let watchdogs = Watchdogs {
            camera:  CamWatchdog::new(&indi, event_handlers),
            devices: DevicesWatchdog::new(&indi),
        };

        let result = Arc::new(Self {
            indi:            Arc::clone(&indi),
            conn_settings:   Mutex::new(ConnSettings::default()),
            event_handlers:  Arc::clone(event_handlers),
            indi_evt_subscr: Mutex::new(None),
            watchdogs:       Mutex::new(watchdogs),
            indi_drivers:    drivers,
        });

        let self_ = Arc::clone(&result);
        let indi_evt_subscr = indi.connect_event_handler(move |event| {
            self_.indi_event_handler(event);
        });

        *result.indi_evt_subscr.lock().unwrap() = Some(indi_evt_subscr);
        result
    }

    pub fn indi(&self) -> &Arc<indi::Connection> {
        &self.indi
    }

    pub fn drivers(&self) -> &indi::Drivers {
        &self.indi_drivers
    }

    pub fn connect(
        &self,
        remote:       bool,
        address:      &str,
        mount_id:     &Option<String>,
        camera_id:    &Option<String>,
        guid_cam_id:  &Option<String>,
        focuser_id:   &Option<String>,
        flt_wheel_id: &Option<String>,
        aux1_id:      &Option<String>,
        aux2_id:      &Option<String>,

    ) -> eyre::Result<()> {
        let drivers = if !remote {
            let telescopes    = self.indi_drivers.get_group_by_name("Telescopes")?;
            let cameras       = self.indi_drivers.get_group_by_name("CCDs")?;
            let focusers      = self.indi_drivers.get_group_by_name("Focusers")?;
            let filter_wheels = self.indi_drivers.get_group_by_name("Filter Wheels")?;
            let aux           = self.indi_drivers.get_group_by_name("Auxiliary")?;

            fn get_driver<'a>(
                device_name: &Option<String>,
                group:       &'a indi::DriverGroup
            ) -> Option<&'a String> {
                device_name
                    .as_ref()
                    .and_then(|name| group.get_item_by_device_name(name))
                    .map(|d| &d.driver)
            }

            [ get_driver(mount_id,     telescopes),
              get_driver(camera_id,    cameras),
              get_driver(guid_cam_id,  cameras),
              get_driver(focuser_id,   focusers),
              get_driver(flt_wheel_id, filter_wheels),
              get_driver(aux1_id,      aux),
              get_driver(aux2_id,      aux),
            ].into_iter()
                .filter_map(|v| v)
                .cloned()
                .unique()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        if !remote && drivers.is_empty() {
            eyre::bail!("No devices selected");
        }

        log::info!(
            "Connecting to INDI, remote={}, address={}, drivers='{}' ...",
            remote,
            address,
            drivers.iter().join(",")
        );

        let conn_settings = indi::ConnSettings {
            drivers,
            remote: remote,
            host:   address.to_string(),
            .. Default::default()
        };

        self.indi.connect(&conn_settings)?;
        Ok(())

        //*self.conn_settings.lock().unwrap() = conn_settings.clone();
    }

    fn indi_event_handler(self: &Arc<Self>, event: indi::Event) {
        let result = || -> eyre::Result<()> {
            match event {
                indi::Event::ConnChange(state) => {
                    if state == indi::ConnState::Disconnected {
                        let mut watchdogs = self.watchdogs.lock().unwrap();
                        watchdogs.camera.reset();
                        watchdogs.devices.reset();
                        drop(watchdogs);
                    }
                    let hal_state = match state {
                        indi::ConnState::Disconnected   => HalState::Disconnected,
                        indi::ConnState::Connecting     => HalState::Connecting,
                        indi::ConnState::Connected      => HalState::Connected,
                        indi::ConnState::Disconnecting  => HalState::Disconnecting,
                        indi::ConnState::Error(err_str) => HalState::Error(err_str),
                    };
                    self.event_handlers.send(HalEvent::StateChanged(hal_state));
                }
                indi::Event::NewDevice(evt) => if evt.connected {
                    self.process_device_connect_evt(&evt.device_name, evt.interface, evt.connected);
                }
                indi::Event::DeviceConnected(evt) => {
                    self.process_device_connect_evt(&evt.device_name, evt.interface, evt.connected);
                }
                indi::Event::DeviceDelete(evt) => {
                    self.process_device_connect_evt(&evt.device_name, evt.interface, false);
                }
                indi::Event::PropChange(prop_change) => {
                    let mut watchdogs = self.watchdogs.lock().unwrap();
                    _ = watchdogs.camera.notify_indi_prop_change(&prop_change);
                    _ = watchdogs.devices.notify_indi_prop_change(&prop_change);
                    drop(watchdogs);
                    self.process_indi_prop_change_event(&prop_change)?;
                }
                indi::Event::BlobStart(blob_start) => {
                    let mut device_id = blob_start.device_name.to_string();
                    if *blob_start.elem_name == "CCD2" {
                        device_id += CAM_CCD2_POSTFIX;
                    }
                    self.event_handlers.send(HalEvent::CameraBeginDownloadData(
                        Arc::new(device_id)
                    ));
                }
                _ => {}
            }
            Ok(())
        } ();
        if let Err(err) = result {
            self.event_handlers.send(HalEvent::Error(
                Arc::new(err.to_string())
            ));
        }
    }

    fn process_indi_prop_change_event(
        self:        &Arc<Self>,
        prop_change: &indi::PropChangeEvent
    ) -> eyre::Result<()> {
        use indi::*;

        match &prop_change.change {
            PropChange::New { prop_name, elem_name, value, state } => {
                self.process_prop_change(&prop_change.device_name, prop_name, elem_name, value, *state, true)?;
            }
            PropChange::Change { value, prop_name, elem_name, new_state, .. } => {
                self.process_prop_change(&prop_change.device_name, prop_name, elem_name, value, *new_state, false)?;
            }
             _ => {},
        }
        Ok(())
    }

    fn process_prop_change(
        self:        &Arc<Self>,
        device_name: &Arc<String>,
        prop_name:   &str,
        elem_name:   &str,
        value:       &indi::PropValue,
        state:       indi::PropState,
        new_prop:    bool,
    ) -> eyre::Result<()> {
        use indi::*;

        match (prop_name, elem_name, value, state, new_prop) {
            (_, _, PropValue::Blob(blob), _, false) => {
                self.process_indi_blob_event(blob, device_name, prop_name)?;
            }
            (_, _, PropValue::Num(value), _, _)
            if Connection::camera_is_cooler_pwr_property(prop_name, elem_name) => {
                self.event_handlers.send(HalEvent::CameraCoolerPwrChanged {
                    device_id: Arc::clone(device_name),
                    power:     value.value
                });
            }
            ("CCD_COOLER", _, _, _, true) => {
                self.event_handlers.send(HalEvent::CameraCoolerCanBeControlled(
                    Arc::clone(device_name)
                ));
            }
            ("CCD_OFFSET", _, _, _, true) => {
                self.event_handlers.send(HalEvent::CameraOffsetCanBeControlled(
                    Arc::clone(device_name)
                ));
            }
            ("CCD_GAIN", _, _, _, true) => {
                self.event_handlers.send(HalEvent::CameraGainCanBeControlled(
                    Arc::clone(device_name)
                ));
            }
            ("CCD_EXPOSURE", _, _, _, true) => {
                self.event_handlers.send(HalEvent::CameraIsReadyToWork(
                    Arc::clone(device_name)
                ));
            }
            ("CCD_EXPOSURE", "CCD_EXPOSURE_VALUE", PropValue::Num(value), _, false) => {
                self.event_handlers.send(HalEvent::CameraTimeUntilEndOfExposure {
                    device_id: Arc::clone(device_name),
                    time:      value.value
                });
            }
            ("GUIDER_EXPOSURE", _, _, _, true) => {
                self.event_handlers.send(HalEvent::CameraIsReadyToWork(
                    Arc::new(device_name.to_string() + CAM_CCD2_POSTFIX)
                ));
            }
            ("GUIDER_EXPOSURE", "GUIDER_EXPOSURE_VALUE", PropValue::Num(value), _, false) => {
                self.event_handlers.send(HalEvent::CameraTimeUntilEndOfExposure {
                    device_id: Arc::new(device_name.to_string() + CAM_CCD2_POSTFIX),
                    time:      value.value
                });
            }
            ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE"|"CCD_TEMPERATURE", PropValue::Num(value), _, _) => {
                self.event_handlers.send(HalEvent::CameraCcdTempChanged {
                    device_id:    Arc::clone(device_name),
                    temperature:  value.value
                });
            }
            ("CCD_INFO", "CCD_MAX_X"|"CCD_MAX_Y", _, _, _) => {
                self.event_handlers.send(HalEvent::CameraCcdSizeChanged(
                    Arc::clone(device_name)
                ));
            }
            ("TELESCOPE_TRACK_STATE", "TRACK_ON", indi::PropValue::Switch(true), _, _) => {
                self.event_handlers.send(HalEvent::TelescopeTrackingChanged {
                    device_id: Arc::clone(device_name),
                    tracking: true,
                });
            }
            ("TELESCOPE_TRACK_STATE", "TRACK_OFF", indi::PropValue::Switch(true), _, _) => {
                self.event_handlers.send(HalEvent::TelescopeTrackingChanged {
                    device_id: Arc::clone(device_name),
                    tracking:  false,
                });
                self.send_telescope_state_changed_event(device_name);
            }

            ("TELESCOPE_PARK", "PARK", indi::PropValue::Switch(true), _, _) => {
                self.event_handlers.send(HalEvent::TelescopeParked(
                    Arc::clone(device_name)
                ));
                self.send_telescope_state_changed_event(device_name);
            }
            ("TELESCOPE_PARK", "UNPARK", indi::PropValue::Switch(true), _, _) => {
                self.event_handlers.send(HalEvent::TelescopeUnparked(
                    Arc::clone(device_name)
                ));
                self.send_telescope_state_changed_event(device_name);
            }
            ("TELESCOPE_MOTION_NS" | "TELESCOPE_MOTION_WE" |
             "TELESCOPE_TIMED_GUIDE_NS" | "TELESCOPE_TIMED_GUIDE_WE" |
             "EQUATORIAL_EOD_COORD",
             ..) => {
                 self.send_telescope_state_changed_event(device_name);
            }
            ("TELESCOPE_SLEW_RATE", _, _, _, true) => {
                self.event_handlers.send(HalEvent::TelescopeSlewRateListReady(
                    Arc::clone(device_name)
                ));
            }
            ("ABS_FOCUS_POSITION", "FOCUS_ABSOLUTE_POSITION", PropValue::Num(value), state, _) => {
                self.event_handlers.send(HalEvent::FocuserAbsValueChanged {
                    device_id: Arc::clone(device_name),
                    abs_value: value.value,
                });
                self.event_handlers.send(HalEvent::FocuserStateChanged {
                    device_id: Arc::clone(device_name),
                    state:     abs_pos_prop_state_to_focuser_state(state),
                });
            }
            ("FOCUS_TEMPERATURE", "TEMPERATURE", PropValue::Num(value), _, _) => {
                self.event_handlers.send(HalEvent::FocuserTemperatureChanged {
                    device_id:   Arc::clone(device_name),
                    temperature: value.value,
                });
            }
            ("FILTER_SLOT", "FILTER_SLOT_VALUE", PropValue::Num(value), _, _) => {
                let in_progress = !matches!(state, PropState::Ok|PropState::Idle);
                let slot = if in_progress {
                    Some(value.value as i32 - value.min as i32)
                } else {
                    None
                };

                self.event_handlers.send(HalEvent::FilterWheelSlotChange {
                    device_id: Arc::clone(device_name),
                    slot
                });
            }
            ("FILTER_NAME", _, _, _, _) => {
                self.event_handlers.send(HalEvent::FilterWheelNameChanged (
                    Arc::clone(device_name)
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn send_telescope_state_changed_event(&self, device_name: &Arc<String>) {
        self.event_handlers.send(HalEvent::TelescopeStateChanged {
            device_id: Arc::clone(device_name),
            state:     telescope_state(&self.indi, device_name).unwrap_or(TelescopeState::Error),
        });
    }

    fn process_indi_blob_event(
        self:        &Arc<Self>,
        blob:        &Arc<indi::BlobPropValue>,
        device_name: &str,
        device_prop: &str,
    ) -> eyre::Result<()> {
        let mut device_id = device_name.to_string();
        if device_prop == "CCD2" {
            device_id += CAM_CCD2_POSTFIX;
        }
        let camera_shot = IndiCameraShot::new(blob)?;
        self.event_handlers.send(HalEvent::CameraShotResult{
            device_id: Arc::new(device_id),
            shot:      Arc::new(camera_shot),
        });
        Ok(())
    }

    fn process_device_connect_evt(
        &self,
        device_name: &Arc<String>,
        interface:   indi::DriverInterface,
        connected:   bool
    ) {
        let device_type = Self::driver_interface_to_dev_type(interface);
        if device_type.is_empty() {
            return;
        }

        if interface.contains(indi::DriverInterface::CCD) {
            let mut watchdogs = self.watchdogs.lock().unwrap();
            if connected {
                watchdogs.camera.notify_device_added(device_name);
            } else {
                watchdogs.camera.notify_device_deleted(device_name);
            }
            drop(watchdogs);
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
        self.event_handlers.send(event_to_send);
    }

    fn driver_interface_to_dev_type(drv_interface: indi::DriverInterface) -> DeviceType {
        let is_ccd          = drv_interface.contains(indi::DriverInterface::CCD);
        let is_telescope    = drv_interface.contains(indi::DriverInterface::TELESCOPE);
        let is_focuser      = drv_interface.contains(indi::DriverInterface::FOCUSER);
        let is_filter_wheel = drv_interface.contains(indi::DriverInterface::FILTER);

        let mut device_type = DeviceType::empty();
        device_type.set(DeviceType::CAMERA,    is_ccd);
        device_type.set(DeviceType::TELESCOPE, is_telescope);
        device_type.set(DeviceType::FOCUSER,   is_focuser);
        device_type.set(DeviceType::FLT_WHELL, is_filter_wheel);

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
            self.indi.disconnect_event_handler(indi_evt_subscr);
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

    fn disconnect(&self) -> eyre::Result<()> {
        self.indi.disconnect_and_wait()?;
        Ok(())
    }

    fn notify_periodical_timer_tick(&self, timer_period_ms: usize) -> eyre::Result<()> {
        let mut watchdogs = self.watchdogs.lock().unwrap();
        watchdogs.camera.notify_periodical_timer_tick(timer_period_ms)?;
        watchdogs.devices.notify_periodical_timer_tick(timer_period_ms)?;
        drop(watchdogs);

        Ok(())
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

    fn cameras(&self) -> eyre::Result<Vec<CameraInfo>> {
        struct SensorSize {
            device: indi::ExportDevice,
            sensor_width: isize,
        }

        let mut all_cemeras: Vec<_> = self.indi.get_devices_list_by_interface(indi::DriverInterface::CCD)
            .iter()
            .filter_map(|d| {
                let fun = || -> eyre::Result<SensorSize> {
                    let (pixel_size_x, _) = self.indi.camera_get_pixel_size_um(&d.name, indi::CamCcd::Main)?;
                    let (sensor_width, _) = self.indi.camera_get_max_frame_size(&d.name, indi::CamCcd::Main)?;
                    Ok(SensorSize {
                        device: d.clone(),
                        sensor_width: (pixel_size_x * sensor_width as f64) as _,
                    })
                };
                fun().ok()
            })
            .collect();

        all_cemeras.sort_by_key(|ss| -ss.sensor_width);
        let all_cemeras_len = all_cemeras.len();

        let mut result = Vec::new();

        for (idx, camera) in all_cemeras.into_iter().enumerate() {
            let purpose = if idx == 0 || *camera.device.name == "CCD Simulator" {
                CcdPurpose::MainTelescopeCcd
            } else if (idx == 1 && all_cemeras_len == 2) || *camera.device.name == "Guide Simulator" {
                CcdPurpose::GuiderCcd
            } else {
                CcdPurpose::Unknown
            };

            result.push(CameraInfo {
                name: camera.device.name.to_string(),
                id: camera.device.name.to_string(),
                ccd: purpose,
            });

            if self.indi.property_exists(&camera.device.name, "CCD2", None).unwrap_or(false) {
                let purpose = if idx == 0 {
                    CcdPurpose::SecodnaryTelescopeCcd
                } else {
                    CcdPurpose::Unknown
                };
                result.push(CameraInfo {
                    name: camera.device.name.to_string(),
                    id:   camera.device.name.to_string()+CAM_CCD2_POSTFIX,
                    ccd:  purpose,
                });
            }
        }
        Ok(result)
    }

    fn camera(&self, id: &str) -> Option<Arc<dyn Camera + Send + Sync>> {
        let mut ccd = indi::CamCcd::Main;
        let mut name = id;
        if id.ends_with(CAM_CCD2_POSTFIX) {
            let new_len = id.len() - CAM_CCD2_POSTFIX.len();
            name = &id[..new_len];
            ccd = indi::CamCcd::Guider;
        }
        if !self.indi.device_exists(id) {
            return None;
        }
        let device = IndiDevice {
            id:   id.to_string(),
            name: name.to_string(),
            indi: Arc::clone(&self.indi),
        };
        let camera = IndiCamera { device, ccd };
        Some(Arc::new(camera))
    }

    fn telescope(&self, id: &str) -> Option<Arc<dyn Telescope + Send + Sync>> {
        if !self.indi.device_exists(id) {
            return None;
        }
        Some(Arc::new(self.create_indi_device(id)))
    }

    fn focuser(&self, id: &str) -> Option<Arc<dyn Focuser + Send + Sync>> {
        if !self.indi.device_exists(id) {
            return None;
        }
        Some(Arc::new(self.create_indi_device(id)))
    }

    fn filter_wheel(&self, id: &str) -> Option<Arc<dyn FilterWheel + Send + Sync>> {
        if !self.indi.device_exists(id) {
            return None;
        }
        Some(Arc::new(self.create_indi_device(id)))
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

    fn name(&self) -> &str {
        &self.name
    }

    fn is_active(&self) -> eyre::Result<bool> {
        Ok(self.indi.is_device_enabled(&self.name)?)
    }
}

//////////////////////////////////////////////////////////////////////////////
// CameraShot

struct IndiCameraShot {
    blob:       Arc<indi::BlobPropValue>,
    image_type: CameraShotType,
}

impl IndiCameraShot {
    fn new(blob: &Arc<indi::BlobPropValue>) -> eyre::Result<Self> {
        let mut stream = Cursor::new(blob.data.as_slice());
        let fits_reader = FitsReader::new(&mut stream)?;
        let is_raw_image = find_mono_image_hdu_in_fits(&fits_reader).is_some();
        let is_color_image = find_color_image_hdu_in_fits(&fits_reader).is_some();
        let image_type = if is_raw_image {
            CameraShotType::RawCcdData
        } else if is_color_image {
            CameraShotType::ReadyImage
        } else {
            eyre::bail!("Can't find out type of image");
        };
        Ok(Self {
            blob: Arc::clone(blob),
            image_type
        })
    }
}

impl CameraShot for IndiCameraShot {
    fn get_type(&self) -> CameraShotType {
        self.image_type
    }

    fn get_raw(&self) -> eyre::Result<crate::image::raw::RawImage> {
        let mut stream = Cursor::new(self.blob.data.as_slice());
        let reader = FitsReader::new(&mut stream)?;
        let raw_image = load_raw_image_from_fits_reader(&reader, &mut stream)?;
        Ok(raw_image)
    }

    fn get_image(&self, _image: &mut crate::image::image::Image) -> eyre::Result<()> {
        eyre::bail!("Color image is unimplemented for INDI drivers");
    }

    fn download_time(&self) -> f64 {
        self.blob.dl_time
    }

    fn file_ext(&self) -> &str {
        self.blob.format.trim()
    }

    fn save_to_file(&self, file_name: &Path) -> eyre::Result<()> {
        std::fs::write(file_name, self.blob.data.as_slice())
            .map_err(|e| eyre::eyre!(
                "Error '{}'\nwhen saving file '{}'",
                e, file_name.to_str().unwrap_or_default(),
            ))?;
        Ok(())
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

    fn name(&self) -> &str {
        self.device.id() // self.name is only for internal use for camera impl.
    }

    fn is_active(&self) -> eyre::Result<bool> {
        self.device.is_active()
    }
}

impl Camera for IndiCamera {
    // Common

    fn features(&self) -> CameraFeatures {
        CameraFeatures::CAN_START_EXP_AT_DOWNLOAD_BEGIN
    }

    fn init_before_shot(&self) -> eyre::Result<()> {
        // Enable blob
        self.device.indi.command_enable_blob(
            &self.device.name,
            None,
            indi::BlobEnable::Also,
        )?;

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

    fn remaining_time(&self) -> Option<f64> {
        self.device.indi.camera_get_exposure(&self.device.name, self.ccd).ok()
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

    fn enable_fan(&self, enable: bool) -> eyre::Result<()> {
        self.device.indi.camera_control_fan(
            &self.device.name,
            enable,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
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

    fn set_telescope_focal_len(&self, focal_len: f64) -> eyre::Result<()> {
        let (foc_len, aperture) = self.device.indi.camera_get_telescope_focal_len_and_aperture(&self.device.name)?;
        let apertute_to_set = if aperture < 0.01 {
            Some(0.2 * foc_len)
        } else {
            None
        };
        self.device.indi.camera_set_telescope_focal_len_and_aperture(
            &self.device.name,
            focal_len,
            apertute_to_set,
            true,
            Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }
}

///////////////////////////////////////////////////////////////////////////////
// Telescope (mount)

fn telescope_state(
    indi:        &Arc<indi::Connection>,
    device_name: &str
) -> eyre::Result<TelescopeState> {
    let eq_coord_prop_state = indi.mount_get_eq_coord_prop_state(device_name)?;
    let is_parked = indi.mount_is_parked(device_name)?;
    let is_error = eq_coord_prop_state == indi::PropState::Alert;
    let is_tracking = indi.mount_is_tracking(device_name)?;
    let is_slewing = eq_coord_prop_state == indi::PropState::Busy;
    let is_correction = indi.mount_is_timed_guiding(device_name)?;
    let is_moved = indi.mount_is_moving(device_name)?;
    let result = if is_parked {
        TelescopeState::Parked
    } else if is_error {
        TelescopeState::Error
    } else if is_moved {
        TelescopeState::Moved
    } else if is_correction {
        TelescopeState::Correcton
    } else if is_tracking {
        TelescopeState::Tracking
    } else if is_slewing {
        TelescopeState::Slewing
    } else {
        TelescopeState::Stopped
    };
    Ok(result)
}

impl Telescope for IndiDevice {
    fn state(&self) -> eyre::Result<TelescopeState> {
        telescope_state(&self.indi, &self.name)
    }

    fn is_abort_motion_supported(&self) -> bool {
        true
    }

    fn abort_motion(&self) -> eyre::Result<()> {
        self.indi.mount_abort_motion(&self.name)?;
        self.indi.mount_stop_move(&self.name)?;
        Ok(())
    }

    fn is_parked(&self) -> eyre::Result<bool> {
        Ok(self.indi.mount_is_parked(&self.name)?)
    }

    fn park(&self) -> eyre::Result<()> {
        self.indi.mount_set_parked(&self.name, true, true, None)?;
        Ok(())
    }

    fn unpark(&self) -> eyre::Result<()> {
        self.indi.mount_set_parked(&self.name, false, true, None)?;
        Ok(())
    }

    fn is_tracking(&self) -> eyre::Result<bool> {
        Ok(self.indi.mount_is_tracking(&self.name)?)
    }

    fn track(&self, enabled: bool) -> eyre::Result<()> {
        self.indi.mount_set_tracking(&self.name, enabled, true, None)?;
        Ok(())
    }

    fn revert_motion(&self, reverse_ns: bool, reverse_we: bool) -> eyre::Result<()> {
        self.indi.mount_revert_motion(
            &self.name,
            reverse_ns, reverse_we,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        Ok(())
    }

    fn move_(&self, direction: TelescopeMoveDir) -> eyre::Result<()> {
        match direction {
            TelescopeMoveDir::North => {
                self.indi.mount_start_move_north(&self.name)?;
            }
            TelescopeMoveDir::South => {
                self.indi.mount_start_move_south(&self.name)?;
            }
            TelescopeMoveDir::West => {
                self.indi.mount_start_move_west(&self.name)?;
            }
            TelescopeMoveDir::East => {
                self.indi.mount_start_move_east(&self.name)?;
            }
            TelescopeMoveDir::NorthWest => {
                self.indi.mount_start_move_north(&self.name)?;
                self.indi.mount_start_move_west(&self.name)?;
            }
            TelescopeMoveDir::NorthEast => {
                self.indi.mount_start_move_north(&self.name)?;
                self.indi.mount_start_move_east(&self.name)?;
            }
            TelescopeMoveDir::SouthWest => {
                self.indi.mount_start_move_south(&self.name)?;
                self.indi.mount_start_move_west(&self.name)?;
            }
            TelescopeMoveDir::SouthEast => {
                self.indi.mount_start_move_south(&self.name)?;
                self.indi.mount_start_move_east(&self.name)?;
            }
        }
        Ok(())
    }

    fn slew_speed_list(&self) -> eyre::Result<Vec<(String/*id*/, String/*text*/)>> {
        let list = self.indi.mount_get_slew_speed_list(&self.name)?
            .into_iter()
            .map(|(id, text)| (id.to_string(), text.unwrap_or(id).to_string()))
            .collect();
        Ok(list)
    }

    fn set_slew_speed(&self, speed_id: &str) -> eyre::Result<()> {
        self.indi.mount_set_slew_speed(
            &self.name,
            speed_id,
            true, Some(SET_PROP_TIME_OUT),
        )?;
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

    fn is_slewing(&self) -> eyre::Result<bool> {
        let crd_prop_state = self.indi.mount_get_eq_coord_prop_state(&self.name)?;
        Ok(crd_prop_state == indi::PropState::Busy)
    }

    fn sync(&self, ra: f64, dec: f64) -> eyre::Result<()> {
        self.indi.mount_set_after_coord_action(
            &self.name,
            indi::AfterCoordSetAction::Sync,
            true, Some(SET_PROP_TIME_OUT)
        )?;
        self.indi.mount_set_eq_coord(&self.name, ra, dec, true, None)?;
        Ok(())
    }

    fn is_guide_rate_supported(&self) -> eyre::Result<bool> {
        Ok(self.indi.mount_is_guide_rate_supported(&self.name)?)
    }

    fn guide_rate(&self) -> eyre::Result<(f64/*ra*/, f64/*dec*/)> {
        Ok(self.indi.mount_get_guide_rate(&self.name)?)
    }

    fn pulse_max_duration(&self) -> eyre::Result<(f64/*ns*/, f64/*we*/)> {
        Ok(self.indi.mount_get_timed_guide_max(&self.name)?)
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

fn abs_pos_prop_state_to_focuser_state(prop_state: indi::PropState) -> FocuserState {
    match prop_state {
        indi::PropState::Ok |
        indi::PropState::Idle  => FocuserState::Stopped,
        indi::PropState::Alert => FocuserState::Error,
        indi::PropState::Busy  => FocuserState::Moving,
    }
}

impl Focuser for IndiDevice {
    fn state(&self) -> eyre::Result<FocuserState> {
        let prop = self.indi.focuser_get_abs_value_prop(&self.id)?;
        Ok(abs_pos_prop_state_to_focuser_state(prop.state))
    }

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

///////////////////////////////////////////////////////////////////////////////
// Filter wheel

impl FilterWheel for IndiDevice {
    fn list_and_active(&self) -> eyre::Result<(Vec<String>, usize)> {
        let (indi_list, active) = self.indi.filter_get_list_and_active(&self.name)?;
        let list = indi_list.iter().map(|text| text.to_string()).collect();
        Ok((list, active))
    }

    fn set_active(&self, active_elem: usize) -> eyre::Result<()> {
        self.indi.filter_set_active(&self.name, active_elem as _)?;
        Ok(())
    }
}
