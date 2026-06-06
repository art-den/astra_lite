use std::sync::{Arc, RwLock};

use crate::hal::{CameraShot, DeviceInfo};

#[derive(Clone)]
pub enum HalEvent {
    Error(Arc<String>),
    DeviceConnected(Arc<DeviceInfo>),
    DeviceDisconnected(Arc<DeviceInfo>),

    NeedRestartCameraExposure(Arc<String/*camera id*/>),
    NeedInitTelescopeFocalLenForCamera(Arc<String/*camera id*/>),
    CameraIsReadyForCooling(Arc<String/*camera id*/>),
    CameraIsReadyForCtrlFan(Arc<String/*camera id*/>),
    CameraIsReadyForCtrlHeater(Arc<String/*camera id*/>),
    BeginDownloadCameraData(Arc<String/*camera id*/>),
    CareraShotResult {
        cam_id: Arc<String>,
        shot:   Arc<dyn CameraShot + Send + Sync>,
    }
}

pub struct HalEventSubscribers {
    funs: RwLock<Vec<Box<dyn Fn(HalEvent) + Send + Sync>>>,
}

impl HalEventSubscribers {
    pub fn new() -> Self {
        Self {
            funs: RwLock::new(Vec::new()),
        }
    }

    pub fn connect_event_handler(&self, fun: impl Fn(HalEvent) + Send + Sync + 'static) {
        let mut funs = self.funs.write().unwrap();
        funs.push(Box::new(fun));
    }

    pub fn send_event(&self, event: HalEvent) {
        let funs = self.funs.read().unwrap();
        for fun in &*funs {
            fun(event.clone());
        }
    }

    pub fn disconnect_all_subscribers(&self) {
        let mut event_handlers = Vec::new();

        let mut funs = self.funs.write().unwrap();
        std::mem::swap(&mut event_handlers, &mut funs);
        drop(funs);

        event_handlers.clear();
    }
}
