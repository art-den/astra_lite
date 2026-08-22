use std::{collections::HashMap, sync::{Arc, RwLock, atomic::AtomicU64}};

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
        device_id: Arc<String>,
        slot:      Option<i32>,
    },

    FilterWheelNameChanged(Arc<String/*device id*/>),
}

type HalEventHandlerFun = dyn Fn(HalEvent) + Send + Sync + 'static;

pub struct EventHandlerId(u64);

pub struct HalEventHandlers {
    items:   RwLock<HashMap<u64, Arc<HalEventHandlerFun>>>,
    next_id: AtomicU64,
}

impl HalEventHandlers {
    pub fn new() -> Self {
        Self {
            items:   RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    pub fn connect(
        &self,
        fun: impl Fn(HalEvent) + Send + Sync + 'static
    ) -> EventHandlerId {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut items = self.items.write().unwrap();
        items.insert(id, Arc::new(fun));
        EventHandlerId(id)
    }

    pub fn send(&self, event: HalEvent) {
        // Copy handlers while holding the lock, then release it before executing
        // This prevents deadlocks if handlers try to access the event system
        let handlers = {
            let items = self.items.read().unwrap();
            items.clone()
        };

        // Execute handlers without holding the lock
        for handler in handlers.values() {
            handler(event.clone());
        }
    }

    pub fn disconnect(&self, EventHandlerId(id): EventHandlerId) {
        let removed = {
            let mut items = self.items.write().unwrap();
            items.remove(&id)
        };
        // Drop the handler outside the lock: its Drop may re-enter
        // the event system (e.g. call disconnect again)
        drop(removed);
    }

    /// Disconnects all handlers.
    pub fn disconnect_all(&self) {
        let mut event_handlers = HashMap::new();
        let mut items = self.items.write().unwrap();
        // Swaps the handlers out, then drops the lock
        std::mem::swap(&mut event_handlers, &mut items);
        drop(items);
        event_handlers.clear();
    }
}
