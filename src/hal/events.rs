use std::sync::{Arc, RwLock};

use crate::hal::{CameraShot, DeviceInfo, HalState};

#[derive(Clone)]
pub enum HalEvent {
    Error(Arc<String>),
    StateChanged(HalState),
    DeviceConnected(Arc<DeviceInfo>),
    DeviceDisconnected(Arc<DeviceInfo>),

    CameraShotResult {
        cam_id: Arc<String>,
        shot:   Arc<dyn CameraShot + Send + Sync>,
    },
    CameraIsReadyToWork(Arc<String/*camera id*/>),
    CameraNeedRestartExposure(Arc<String/*camera id*/>),
    CameraNeedInitTelescopeFocalLen(Arc<String/*camera id*/>),
    CameraIsReadyForCooling(Arc<String/*camera id*/>),
    CameraIsReadyForCtrlFan(Arc<String/*camera id*/>),
    CameraIsReadyForCtrlHeater(Arc<String/*camera id*/>),
    CameraBeginDownloadData(Arc<String/*camera id*/>),
    CameraCoolerPwrChanged {
        cam_id: Arc<String>,
        power:  f64,
    },

    CameraTimeUntilEndOfExposure {
        cam_id: Arc<String>,
        time:   f64,
    },
    CameraCcdTempChanged {
        cam_id:      Arc<String>,
        temperature: f64,
    },
    CameraCoolerCanBeControlled(Arc<String/*camera id*/>),
    CameraHeaterCanBeControlled(Arc<String/*camera id*/>),
    CameraConvGainCanBeControlled(Arc<String/*camera id*/>),
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
