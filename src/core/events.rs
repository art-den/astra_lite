use std::{collections::HashMap, sync::{atomic::AtomicUsize, RwLock}};
use crate::{plate_solve::PlateSolverEvent, DeviceAndProp};
use super::{core::ModeType, frame_processing::*, mode_focusing::*, mode_polar_align::PolarAlignmentEvent};

#[derive(Clone)]
pub struct Progress {
    pub cur: usize,
    pub total: usize,
}

#[derive(Clone)]
pub enum Event {
    Error(String),
    ModeContinued,
    CameraDeviceChanged(DeviceAndProp),
    MountDeviceSelected(String),
    ModeChanged,
    Progress(Option<Progress>, ModeType),
    FrameProcessing(FrameProcessResult),
    Focusing(FocusingStateEvent),
    PlateSolve(PlateSolverEvent),
    PolarAlignment(PolarAlignmentEvent),
}

type SubscriptionFun = dyn Fn(Event) + Send + Sync + 'static;

pub struct Subscription(usize);

pub struct EventSubscriptions {
    items:   RwLock<HashMap<usize, Box<SubscriptionFun>>>,
    next_id: AtomicUsize,
}

impl EventSubscriptions {
    pub fn new() -> Self {
        Self {
            items:     RwLock::new(HashMap::new()),
            next_id:   AtomicUsize::new(1),
        }
    }

    pub fn subscribe(
        &self,
        fun: impl Fn(Event) + Send + Sync + 'static
    ) -> Subscription {
        let mut items = self.items.write().unwrap();
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        items.insert(id, Box::new(fun));
        Subscription(id)
    }

    pub fn unsubscribe(&self, subscription: Subscription) {
        let Subscription(id) = subscription;
        let mut items = self.items.write().unwrap();
        items.remove(&id);
    }

    pub fn clear(&self) {
        let mut items = self.items.write().unwrap();
        items.clear();
    }

    pub fn notify(&self, event: Event) {
        let items = self.items.write().unwrap();
        for s in items.values() {
            s(event.clone());
        }
    }
}
