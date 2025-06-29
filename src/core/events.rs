use std::sync::{Arc, RwLock};
use crate::{guiding::external_guider::ExtGuiderEvent, plate_solve::PlateSolverEvent, DeviceAndProp};
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
        from: Option<DeviceAndProp>,
        to:   DeviceAndProp
    },
    MountDeviceSelected(String),
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
}

type EventFun = dyn Fn(Event) + Send + Sync + 'static;

pub struct Events {
    items: RwLock<Vec<Box<EventFun>>>,
}

impl Events {
    pub fn new() -> Self {
        Self {
            items:RwLock::new(Vec::new()),
        }
    }

    pub fn subscribe(
        &self,
        fun: impl Fn(Event) + Send + Sync + 'static
    ) {
        let mut items = self.items.write().unwrap();
        items.push(Box::new(fun));
    }

    pub fn unsubscribe_all(&self) {
        let mut items = self.items.write().unwrap();
        items.clear();
    }

    pub fn notify(&self, event: Event) {
        let items = self.items.read().unwrap();
        for s in &*items {
            s(event.clone());
        }
    }
}
