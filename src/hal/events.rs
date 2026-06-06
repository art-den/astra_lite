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

pub struct HalEventHandlers {
    items: RwLock<Vec<Box<dyn Fn(HalEvent) + Send + Sync>>>,
}

impl HalEventHandlers {
    pub fn new() -> Self {
        Self {
            items: RwLock::new(Vec::new()),
        }
    }

    pub fn connect(&self, fun: impl Fn(HalEvent) + Send + Sync + 'static) {
        let mut funs = self.items.write().unwrap();
        funs.push(Box::new(fun));
    }

    pub fn send(&self, event: HalEvent) {
        let funs = self.items.read().unwrap();
        for fun in &*funs {
            fun(event.clone());
        }
    }

    pub fn disconnect_all(&self) {
        let mut event_handlers = Vec::new();

        let mut funs = self.items.write().unwrap();
        std::mem::swap(&mut event_handlers, &mut funs);
        drop(funs);

        event_handlers.clear();
    }
}
