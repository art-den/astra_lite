use std::{collections::HashMap, sync::{Arc, RwLock, atomic::AtomicU64}};
use crate::{guiding::external_guider::ExtGuiderEvent, plate_solve::PlateSolverEvent};
use super::{engine::ModeKind, frame_processing::*, mode_focusing::*, mode_polar_align::PolarAlignmentEvent};

#[derive(Clone)]
pub struct Progress {
    pub cur: usize,
    pub total: usize,
}

#[derive(Clone)]
pub enum OverlayMessagePos {
    Top,
}

#[derive(Clone)]
pub enum Event {
    Error(String),
    ModeContinued,
    CameraDeviceChanged{
        prev_camera_id: String,
        new_camera_id: String,
    },
    MountDeviceChanged(String),
    FocuserDeviceChanged(String),
    FilterWheelDeviceChanged(String),
    ModeChanged,
    Progress(Option<Progress>, ModeKind),
    FrameProcessing(FrameProcessNotification),
    Focusing(FocuserEvent),
    PlateSolve(PlateSolverEvent),
    PolarAlignment(PolarAlignmentEvent),
    OverlayMessage {
        pos:  OverlayMessagePos,
        text: Arc<String>,
    },
    Guider(ExtGuiderEvent),
    FlatExposureCalculated(f64),
    TelescopeFocalLenChanged(f64),
    TelescopeBarlowChanged,
    GuiderFocalLenChanged(f64),
    CameraCoolingOptionsChanged,
    CameraFanOptionsChanged,
    CameraHeaterOptionsChanged,
}

type EventHandlerFun = dyn Fn(Event) + Send + Sync + 'static;

pub struct EventHandlerId(u64);

pub struct EventHandlers {
    items: RwLock<HashMap<u64, Arc<EventHandlerFun>>>,
    next_id: AtomicU64,
}

impl EventHandlers {
    pub fn new() -> Self {
        Self {
            items:   RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    pub fn connect(
        &self,
        fun: impl Fn(Event) + Send + Sync + 'static
    ) -> EventHandlerId {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut items = self.items.write().unwrap();
        items.insert(id, Arc::new(fun));
        EventHandlerId(id)
    }

    pub fn send(&self, event: Event) {
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
