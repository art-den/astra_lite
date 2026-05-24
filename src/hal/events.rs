use std::sync::{Arc, RwLock};

use crate::hal::DeviceInfo;

#[derive(Debug, Clone)]
pub enum HalEvent {
    DeviceConnected(Arc<DeviceInfo>),
    DeviceDisconnected(Arc<DeviceInfo>),
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
