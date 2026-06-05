use std::{collections::HashMap, sync::{Arc, Mutex, atomic::AtomicU64}};

use chrono::{DateTime, Utc};

use super::connection::*;

#[derive(Clone)]
pub struct NewDeviceEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub connected:   bool,
    pub interface:   DriverInterface,
}

#[derive(Clone)]
pub struct DeviceConnectEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub connected:   bool,
    pub interface:   DriverInterface,
}

#[derive(Debug, PartialEq, Clone)]
pub enum PropChange {
    New {
        prop_name: Arc<String>,
        elem_name: Arc<String>,
        value:     PropValue,
        state:     PropState,
    },
    Change {
        prop_name:  Arc<String>,
        elem_name:  Arc<String>,
        value:      PropValue,
        prev_state: PropState,
        new_state:  PropState,
    },
    Delete {
        prop_name: Arc<String>,
    }
}

#[derive(Clone, Debug)]
pub struct PropChangeEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub change:      PropChange,
}

#[derive(Clone)]
pub struct DeviceDeleteEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub interface:   DriverInterface,
}

#[derive(Clone)]
pub struct MessageEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub text:        Arc<String>,
}

#[derive(Clone)]
pub struct BlobStartEvent {
    pub device_name: Arc<String>,
    pub prop_name:   Arc<String>,
    pub elem_name:   Arc<String>,
    pub format:      Arc<String>,
    pub len:         Option<usize>,
}

#[derive(Clone)]
pub enum Event {
    ConnChange(ConnState),
    ConnectionLost,
    NewDevice(NewDeviceEvent),
    DeviceConnected(DeviceConnectEvent),
    PropChange(PropChangeEvent),
    DeviceDelete(DeviceDeleteEvent),
    Message(MessageEvent),
    BlobStart(BlobStartEvent),
}

pub type EventFun = dyn Fn(Event) + Send + 'static;

#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct EventHandlerId(u64);

pub struct EventHandlers {
    items: Mutex<HashMap<EventHandlerId, Box<EventFun>>>,
    key:   AtomicU64,
}

impl EventHandlers {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(HashMap::new()),
            key:   AtomicU64::new(0),
        }
    }

    pub fn send(&self, event: Event) {
        let items = self.items.lock().unwrap();
        for fun in items.values() {
            fun(event.clone());
        }
    }

    pub fn connect(&self, fun: impl Fn(Event) + Send + 'static) -> EventHandlerId {
        let key = self.key.fetch_add(1, std::sync::atomic::Ordering::Release);
        let subscription = EventHandlerId(key);
        let mut items = self.items.lock().unwrap();
        items.insert(subscription, Box::new(fun));
        subscription
    }

    pub fn disconnect(&self, subscription: EventHandlerId) {
        let mut items = self.items.lock().unwrap();
        items.remove(&subscription);
    }

    pub fn disconnect_all(&self) {
        let mut empty_items = HashMap::new();

        let mut items = self.items.lock().unwrap();
        std::mem::swap(&mut *items, &mut empty_items);
        drop(items);

        empty_items.clear();
    }
}
