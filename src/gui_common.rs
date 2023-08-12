use std::{rc::Rc, cell::RefCell, time::Duration, collections::HashMap, hash::Hash};
use gtk::{prelude::*, glib, glib::clone};
use crate::{indi_api, gtk_utils};

pub fn fill_combobox_with_cam_list(
    indi:    &indi_api::Connection,
    cb:      &gtk::ComboBoxText,
    cur_cam: &str
) -> usize {
    let dev_list = indi.get_devices_list();
    let cameras = dev_list
        .iter()
        .filter(|device|
            device.interface.contains(indi_api::DriverInterface::CCD)
        );
    let last_active_id = cb.active_id().map(|s| s.to_string());
    cb.remove_all();
    for camera in cameras {
        cb.append(Some(&camera.name), &camera.name);
    }
    let cameras_count = gtk_utils::combobox_items_count(cb);
    if cameras_count == 1 {
        cb.set_active(Some(0));
    } else if cameras_count > 1 {
        if !cur_cam.is_empty() {
            cb.set_active_id(Some(cur_cam));
        }
        if cb.active_id().is_none() && last_active_id.is_some() {
            cb.set_active_id(last_active_id.as_deref());
        }
        if cb.active_id().is_none() {
            cb.set_active(Some(0));
        }
    }
    cameras_count
}

const DELAYED_ACTIONS_TIMER_PERIOD_MS: u64 = 100;

struct DelayedActionsData<Action: Hash+Eq + 'static> {
    items:         HashMap<Action, u64>,
    period:        u64,
    event_handler: Option<Box<dyn Fn(&Action) + 'static>>,
}

pub struct DelayedActions<Action: Hash+Eq + 'static> {
    data: Rc<RefCell<DelayedActionsData<Action>>>,
}

impl<Action: Hash+Eq + 'static> DelayedActions<Action> {
    pub fn new(period: u64) -> Self {
        let data = Rc::new(RefCell::new(DelayedActionsData {
            items:         HashMap::new(),
            event_handler: None,
            period,
        }));
        glib::timeout_add_local(
            Duration::from_millis(DELAYED_ACTIONS_TIMER_PERIOD_MS),
            clone!(@strong data => @default-return Continue(false),
            move || {
                let mut data = data.borrow_mut();
                if let Some(event_handler) = data.event_handler.take() {
                    for (key, value) in &mut data.items {
                        if *value > DELAYED_ACTIONS_TIMER_PERIOD_MS {
                            *value -= DELAYED_ACTIONS_TIMER_PERIOD_MS;
                        } else {
                            *value = 0;
                            event_handler(key);
                        }
                    }
                    data.event_handler = Some(event_handler);
                    data.items.retain(|_, v| { *v != 0 });
                }
                Continue(true)
            })
        );
        DelayedActions { data }
    }

    pub fn set_event_handler(&self, event_handler: impl Fn(&Action) + 'static) {
        let mut data = self.data.borrow_mut();
        data.event_handler = Some(Box::new(event_handler));
    }

    pub fn schedule(&self, item: Action) {
        let mut data = self.data.borrow_mut();
        let period = data.period;
        data.items.insert(item, period);
    }

    pub fn schedule_ex(&self, item: Action, period: u64) {
        let mut data = self.data.borrow_mut();
        data.items.insert(item, period);
    }
}