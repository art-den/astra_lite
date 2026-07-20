use std::sync::{Arc, Mutex, RwLock};
use crate::{hal::{events::HalEvent, *}, options::Options};
use super::events::*;


#[derive(Default)]
struct CurDevicesData {
    camera:       Option<Arc<dyn Camera + Send + Sync>>,
    telescope:    Option<Arc<dyn Telescope + Send + Sync>>,
    focuser:      Option<Arc<dyn Focuser + Send + Sync>>,
    filter_wheel: Option<Arc<dyn FilterWheel + Send + Sync>>,
}

pub struct CurDevices {
    data:     Mutex<CurDevicesData>,
    hal:      Arc<Hal>,
    options:  Arc<RwLock<Options>>,
    events:   Arc<EventHandlers>,
}

impl CurDevices {
    pub fn new(options: &Arc<RwLock<Options>>, hal: &Arc<Hal>, events: &Arc<EventHandlers>) -> Arc::<Self> {
        let result = Arc::new(Self {
            data:     Mutex::new(CurDevicesData::default()),
            hal:      Arc::clone(hal),
            options:  Arc::clone(options),
            events:   Arc::clone(events),
        });

        let self_ = Arc::clone(&result);
        hal.connect_event_handler(move |event| _ = self_.hal_event_handler(event));

        result
    }

    fn hal_event_handler(self: &Arc<Self>, event: HalEvent) -> eyre::Result<()> {
        match event {
            HalEvent::DeviceConnected(info) => {
                let options = self.options.read().unwrap();
                if info.type_.contains(DeviceType::CAMERA) && options.cam.device_id == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.camera = Some(self.hal.camera(&info.id)?);
                }
                if info.type_.contains(DeviceType::TELESCOPE) && options.mount.device == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.telescope = Some(self.hal.telescope(&info.id)?);
                }
                if info.type_.contains(DeviceType::FOCUSER) && options.focuser.device == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.focuser = Some(self.hal.focuser(&info.id)?);
                }
                if info.type_.contains(DeviceType::FLT_WHEEL) && options.filter_wheel.device == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.filter_wheel = Some(self.hal.filter_wheel(&info.id)?);
                }
            }
            HalEvent::DeviceDisconnected(info) => {
                let options = self.options.read().unwrap();
                if info.type_.contains(DeviceType::CAMERA) && options.cam.device_id == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.camera = None;
                }
                if info.type_.contains(DeviceType::TELESCOPE) && options.mount.device == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.telescope = None;
                }
                if info.type_.contains(DeviceType::FOCUSER) && options.focuser.device == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.focuser = None;
                }
                if info.type_.contains(DeviceType::FLT_WHEEL) && options.filter_wheel.device == info.id {
                    let mut data = self.data.lock().unwrap();
                    data.filter_wheel = None;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn camera(&self) -> Option<Arc<dyn Camera + Send + Sync>> {
        let data = self.data.lock().unwrap();
        data.camera.as_ref().map(Arc::clone)
    }

    pub fn camera_or_err(&self) -> eyre::Result<Arc<dyn Camera + Send + Sync>> {
        let data = self.data.lock().unwrap();
        let Some(camera) = data.camera.as_ref() else {
            eyre::bail!("Camera object is None");
        };
        Ok(Arc::clone(camera))
    }

    pub fn change_camera(self: &Arc<Self>, new_camera_id: &str) {
        let mut options = self.options.write().unwrap();
        let prev_camera_id = options.cam.device_id.clone();
        if prev_camera_id == new_camera_id { return; }
        options.cam.device_id = new_camera_id.to_string();
        drop(options);

        let mut data = self.data.lock().unwrap();
        data.camera = self.hal.camera(new_camera_id).ok();
        drop(data);

        self.events.send(Event::CameraDeviceChanged {
            prev_camera_id: prev_camera_id.to_string(),
            new_camera_id:  new_camera_id.to_string(),
        });
    }

    pub fn telescope(&self) -> Option<Arc<dyn Telescope + Send + Sync>> {
        let data = self.data.lock().unwrap();
        data.telescope.as_ref().map(Arc::clone)
    }

    pub fn telescope_or_err(&self) -> eyre::Result<Arc<dyn Telescope + Send + Sync>> {
        let data = self.data.lock().unwrap();
        let Some(telescope) = data.telescope.as_ref() else {
            eyre::bail!("Telescope object is None");
        };
        Ok(Arc::clone(telescope))
    }

    pub fn change_telescope(&self, new_telescope_id: &str) {
        let mut options = self.options.write().unwrap();
        if options.mount.device == new_telescope_id { return; }
        options.mount.device = new_telescope_id.to_string();
        drop(options);

        let mut data = self.data.lock().unwrap();
        data.telescope = self.hal.telescope(new_telescope_id).ok();
        drop(data);

        self.events.send(
            Event::MountDeviceChanged(new_telescope_id.to_string())
        );
    }

    pub fn focuser(&self) -> Option<Arc<dyn Focuser + Send + Sync>> {
        let data = self.data.lock().unwrap();
        data.focuser.as_ref().map(Arc::clone)
    }

    pub fn focuser_or_err(&self) -> eyre::Result<Arc<dyn Focuser + Send + Sync>> {
        let data = self.data.lock().unwrap();
        let Some(focuser) = data.focuser.as_ref() else {
            eyre::bail!("Focuser object is None");
        };
        Ok(Arc::clone(focuser))
    }

    pub fn change_focuser(&self, new_focuser_id: &str) {
        let mut options = self.options.write().unwrap();
        if options.focuser.device == new_focuser_id { return; }
        options.focuser.device = new_focuser_id.to_string();
        drop(options);

        let mut data = self.data.lock().unwrap();
        data.focuser = self.hal.focuser(new_focuser_id).ok();
        drop(data);

        self.events.send(
            Event::FocuserDeviceChanged(new_focuser_id.to_string())
        );
    }

    pub fn filter_wheel(&self) -> Option<Arc<dyn FilterWheel + Send + Sync>> {
        let data = self.data.lock().unwrap();
        data.filter_wheel.as_ref().map(Arc::clone)
    }

    pub fn filter_wheel_or_err(&self) -> eyre::Result<Arc<dyn FilterWheel + Send + Sync>> {
        let data = self.data.lock().unwrap();
        let Some(filter_wheel) = data.filter_wheel.as_ref() else {
            eyre::bail!("Filter wheel object is None");
        };
        Ok(Arc::clone(filter_wheel))
    }

    pub fn change_filter_wheel(&self, new_filter_wheel_id: &str) {
        let mut options = self.options.write().unwrap();
        if options.filter_wheel.device == new_filter_wheel_id { return; }
        options.filter_wheel.device = new_filter_wheel_id.to_string();
        drop(options);

        let mut data = self.data.lock().unwrap();
        data.filter_wheel = self.hal.filter_wheel(new_filter_wheel_id).ok();
        drop(data);

        self.events.send(
            Event::FilterWheelDeviceChanged(new_filter_wheel_id.to_string())
        );
    }
}
