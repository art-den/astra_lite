use std::sync::{Arc, RwLock};

use crate::hal::{CameraShot, DeviceInfo, FocuserState, HalState, TelescopeState};

#[derive(Clone)]
pub enum HalEvent {
    // Common
    //
    Error(Arc<String>),
    StateChanged(HalState),

    // Devices

    DeviceConnected(Arc<DeviceInfo>),
    DeviceDisconnected(Arc<DeviceInfo>),

    // Camera

    CameraShotResult {
        device_id: Arc<String>,
        shot:      Arc<dyn CameraShot + Send + Sync>,
    },
    CameraIsReadyToWork(Arc<String/*device id*/>),
    CameraNeedRestartExposure(Arc<String/*device id*/>),
    CameraNeedInitTelescopeFocalLen(Arc<String/*device id*/>),
    CameraIsReadyForCooling(Arc<String/*device id*/>),
    CameraIsReadyForCtrlFan(Arc<String/*device id*/>),
    CameraIsReadyForCtrlHeater(Arc<String/*device id*/>),
    CameraBeginDownloadData(Arc<String/*device id*/>),
    CameraCoolerPwrChanged {
        device_id: Arc<String>,
        power:     f64,
    },
    CameraTimeUntilEndOfExposure {
        device_id: Arc<String>,
        time:      f64,
    },
    CameraCcdTempChanged {
        device_id:    Arc<String>,
        temperature: f64,
    },
    CameraCoolerCanBeControlled(Arc<String/*device id*/>),
    CameraHeaterCanBeControlled(Arc<String/*device id*/>),
    CameraOffsetCanBeControlled(Arc<String/*device id*/>),
    CameraGainCanBeControlled(Arc<String/*device id*/>),
    CameraConvGainCanBeControlled(Arc<String/*device id*/>),
    CameraCcdSizeChanged(Arc<String/*device id*/>),

    // Telescope (mount)

    TelescopeSlewRateListReady(Arc<String/*device id*/>),
    TelescopeTrackingChanged{
        device_id: Arc<String>,
        tracking:  bool,
    },

    TelescopeParked(Arc<String/*device id*/>),
    TelescopeUnparked(Arc<String/*device id*/>),
    TelescopeStateChanged {
        device_id: Arc<String>,
        state:     TelescopeState,
    },

    // Focuser

    FocuserAbsValueCanBeControlled{
        device_id: Arc<String>,
        abs_value: f64,
    },
    FocuserAbsValueChanged{
        device_id: Arc<String>,
        abs_value: f64,
    },
    FocuserTemperatureChanged{
        device_id:   Arc<String>,
        temperature: f64,
    },
    FocuserStateChanged {
        device_id: Arc<String>,
        state:     FocuserState,
    },

    // Filter wheel

    FilterWheelSlotChange {
        device_id:   Arc<String>,
        slot:        i32,
        in_progress: bool,
    },

    FilterWheelNameChanged(Arc<String/*device id*/>),
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
