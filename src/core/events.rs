use std::sync::{Arc, RwLock};
use crate::{guiding::external_guider::ExtGuiderEvent, plate_solve::PlateSolverEvent};
use super::{core::ModeType, frame_processing::*, mode_focusing::*, mode_polar_align::PolarAlignmentEvent};

#[derive(Clone)]
pub struct Progress {
    pub cur: usize,
    pub total: usize,
}

#[derive(Clone)]
pub enum OverlayMessgagePos {
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
    FltWheelDeviceChanged(String),
    ModeChanged,
    Progress(Option<Progress>, ModeType),
    FrameProcessing(FrameProcessResult),
    Focusing(FocuserEvent),
    PlateSolve(PlateSolverEvent),
    PolarAlignment(PolarAlignmentEvent),
    OverlayMessage {
        pos:  OverlayMessgagePos,
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

pub struct EventHandlers {
    items: RwLock<Vec<Box<EventHandlerFun>>>,
}

impl EventHandlers {
    pub fn new() -> Self {
        Self {
            items:RwLock::new(Vec::new()),
        }
    }

    pub fn connect(
        &self,
        fun: impl Fn(Event) + Send + Sync + 'static
    ) {
        let mut items = self.items.write().unwrap();
        items.push(Box::new(fun));
    }

    pub fn send(&self, event: Event) {
        let items = self.items.read().unwrap();
        for s in &*items {
            s(event.clone());
        }
    }

    pub fn disconnect_all(&self) {
        let mut event_handlers = Vec::new();
        let mut items = self.items.write().unwrap();
        std::mem::swap(&mut event_handlers, &mut items);
        drop(items);
        event_handlers.clear();
    }
}
