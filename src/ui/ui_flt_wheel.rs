use std::{cell::{Cell, RefCell}, collections::HashSet, rc::Rc, sync::Arc};

use gtk::{prelude::*, glib, glib::clone};

use macros::FromBuilder;

use crate::{
    core::{api::{Command, FilterWheelCommand, Query, QueryReply, Reply}, engine::Engine, events::Event},
    hal::{DeviceType, HalState, events::HalEvent},
    options::Options,
    ui::{gtk_utils, module::*, ui_main::MainUi, utils::{DelayedActions, ExclusiveCaller, fill_devices_list_into_combobox}},
    utils::log_utils::log_if_error,
};

pub fn init_ui(
    window:   &gtk::ApplicationWindow,
    main_ui:  &Rc<MainUi>,
    engine:   &Arc<Engine>,
) -> Rc<dyn UiModule> {
    let widgets = Widgets::from_builder_str(include_str!(r"resources/flt_wheel.ui"));
    let obj = Rc::new(FltWheelUi {
        widgets,
        excl_caller:     ExclusiveCaller::new(),
        main_ui:         Rc::clone(main_ui),
        window:          window.clone(),
        delayed_actions: DelayedActions::new(250),
        engine:          Arc::clone(engine),
        state_syncing:   Cell::new(false),
        pending_cmds:    RefCell::new(HashSet::new()),
    });
    obj.delayed_actions.set_event_handler(
        clone!(@weak obj => move |action| {
            obj.delayed_actions_handler(action);
        })
    );
    obj.connect_widgets_events();
    obj.correct_widgets_props();
    obj
}

struct FltWheelUi {
    widgets:         Widgets,
    excl_caller:     ExclusiveCaller,
    main_ui:         Rc<MainUi>,
    window:          gtk::ApplicationWindow,
    delayed_actions: DelayedActions<DelayedAction>,
    engine:          Arc<Engine>,
    // Suppresses command handlers while widgets mirror the device state
    state_syncing:   Cell<bool>,
    pending_cmds:    RefCell<HashSet<u64>>,
}

#[derive(Hash, Eq, PartialEq)]
enum DelayedAction {
    UpdateDevicesList,
    UpdateFilterList,
}


#[derive(FromBuilder)]
struct Widgets {
    grd:       gtk::Grid,
    cb_device: gtk::ComboBoxText,
    cb_filter: gtk::ComboBoxText,
}


impl Drop for FltWheelUi {
    fn drop(&mut self) {
        log::info!("FltWheelUi dropped");
    }
}

impl UiModule for FltWheelUi {
    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
                str_id: "flt_wheel",
                name:   "Filter Wheel".to_string(),
                widget: self.widgets.grd.clone().upcast(),
                pos:    PanelPosition::Right,
                tab:    TabPage::Main,
                flags:  PanelFlags::empty(),
        }]
    }

    fn show_options(&self, _options: &Options) {}

    fn on_show_options_first_time(&self) {
        // Fill the device list at first show: ASCOM Classic drivers are always
        // available, so the list must not depend on connect events
        self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
    }

    fn on_event(&self, event: &Event) {
        match event {
            Event::CommandReply { id, result } => {
                if !self.pending_cmds.borrow_mut().remove(id) {
                    return;
                }
                if let Err(err_msg) = result {
                    gtk_utils::show_error_message(Some(&self.window), "Error", err_msg);
                }
            }
            Event::FilterWheelDeviceChanged(new_device_name) => {
                if self.widgets.cb_device.active_id().as_deref() != Some(new_device_name.as_str()) {
                    self.excl_caller.exec(|| {
                        self.widgets.cb_device.set_active_id(Some(new_device_name.as_str()));
                    });
                }
                self.update_filters_list_and_select_active();
                self.correct_widgets_props();
            }
            _ => {},
        }
    }

    fn on_hal_event(&self, event: &HalEvent) {
        match event {
            HalEvent::StateChanged(HalState::Connected|HalState::Disconnected) => {
                self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
            }
            HalEvent::DeviceConnected(info)|HalEvent::DeviceDisconnected(info) => {
                if info.type_.contains(DeviceType::FLT_WHEEL) {
                    self.delayed_actions.schedule(DelayedAction::UpdateDevicesList);
                }
            }
            HalEvent::FilterWheelSlotChange { device_id, slot } => {
                if self.main_ui.snapshot().options.filter_wheel.device == **device_id {
                    if let Some(slot) = slot && *slot >= 0 {
                        let prev = self.state_syncing.replace(true);
                        self.widgets.cb_filter.set_active(Some(*slot as _));
                        self.state_syncing.set(prev);
                    }
                    self.widgets.cb_filter.set_sensitive(slot.is_some());
                }
            }
            HalEvent::FilterWheelNameChanged(device_id) => {
                if self.main_ui.snapshot().options.filter_wheel.device == **device_id {
                    self.delayed_actions.schedule(DelayedAction::UpdateFilterList);
                }
            }
            _ => {}
        }
    }
}

impl FltWheelUi {
    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.cb_device.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            let Some(new_device_name) = cb.active_id() else { return; };
            self_.excl_caller.exec(|| {
                log_if_error(
                    &self_.engine.exec_command(
                        Command::SetFilterWheelDevice { device_id: new_device_name.to_string() }
                    ),
                    "Select filter wheel device"
                );
            });
        }));

        self.widgets.cb_filter.connect_active_notify(clone!(@weak self as self_ => move |cb| {
            if self_.state_syncing.get() { return; }
            let Some(active) = cb.active() else { return; };
            match self_.engine.submit_command(
                Command::FilterWheel(FilterWheelCommand::SetSlot { slot: active as usize })
            ) {
                Ok(id) => { _ = self_.pending_cmds.borrow_mut().insert(id); }
                Err(err) => gtk_utils::show_error_message(
                    Some(&self_.window), "Error", &err.to_string()
                ),
            }
        }));
    }

    fn update_filters_list_and_select_active(&self) {
        let Some(filter_wheel) = self.engine.cur_devices.filter_wheel() else { return; };

        let Ok((list, active_id)) = filter_wheel.list_and_active() else { return; };
        let prev = self.state_syncing.replace(true);
        self.widgets.cb_filter.remove_all();
        for filter_name in list {
            self.widgets.cb_filter.append(Some(filter_name.as_str()), filter_name.as_str());
        }
        let new_cb_active = Some(active_id as u32);
        if self.widgets.cb_filter.active() != new_cb_active {
            self.widgets.cb_filter.set_active(new_cb_active);
        }
        self.state_syncing.set(prev);
    }

    fn delayed_actions_handler(&self, action: &DelayedAction) {
        match action {
            DelayedAction::UpdateFilterList => {
                self.update_filters_list_and_select_active();
                self.correct_widgets_props();
            }

            DelayedAction::UpdateDevicesList => {
                self.update_devices_list();
                self.correct_widgets_props();
            }
        }
    }

    fn update_devices_list(&self) {
        let cur_filter_wheel = self.main_ui.snapshot().options.filter_wheel.device.clone();

        let Ok(Reply::Query(q)) = self.engine.exec_command(Command::Query(Query::Devices))
        else { return; };
        let QueryReply::Devices(devices) = *q else { return; };
        let list = devices.iter()
            .filter(|dev| dev.type_.contains(DeviceType::FLT_WHEEL))
            .map(|dev| (dev.id.to_string(), dev.name.to_string()))
            .collect::<Vec<_>>();
        fill_devices_list_into_combobox(
            &list,
            &self.widgets.cb_device,
            if !cur_filter_wheel.is_empty() { Some(cur_filter_wheel.as_str()) } else { None },
            |id| {
                log_if_error(
                    &self.engine.exec_command(
                        Command::SetFilterWheelDevice { device_id: id.to_string() }
                    ),
                    "Select filter wheel device"
                );
            }
        );
    }

    fn correct_widgets_props(&self) {
        let snap = self.main_ui.snapshot();
        let wheel_active = snap.devices.filter_wheel.as_ref().is_some_and(|w| w.is_active);
        self.widgets.cb_filter.set_sensitive(wheel_active);
    }
}
