use std::sync::{Arc, RwLock};

use crate::{core::events::{Event, Events}, indi, options::{DeviceAndProp, Options}};

const DEVICE_WAIT_BEFORE_CONNECT_TIME: usize = 1; // in seconds
const DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME: usize = 2; // in seconds
const DEVICE_WAIT_CHECK_CUR_DEV_TIME: usize = 3; // in seconds after last device appeared

struct DeviceIsWaitingForAction {
    name: String,
    wait_time_ms: usize,
}

pub struct DevicesWatchdog {
    options:             Arc<RwLock<Options>>,
    indi:                Arc<indi::Connection>,
    events:              Arc<Events>,
    not_connected_yet:   Vec<DeviceIsWaitingForAction>,
    not_opts_loaded_yet: Vec<DeviceIsWaitingForAction>,
    to_check_cur_dev:    Option<usize>,
}

impl DevicesWatchdog {
    pub fn new(
        options: &Arc<RwLock<Options>>,
        indi:    &Arc<indi::Connection>,
        events:  &Arc<Events>,
    ) -> Self {
        Self {
            options:             Arc::clone(options),
            indi:                Arc::clone(indi),
            events:              Arc::clone(events),
            not_connected_yet:   Vec::new(),
            not_opts_loaded_yet: Vec::new(),
            to_check_cur_dev:    None,
        }
    }

    pub fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent,
    ) -> anyhow::Result<()> {
        match &prop_change.change {
            indi::PropChange::New {
                prop_name,
                elem_name,
                value,
                ..
            } if **prop_name == "CONNECTION" && **elem_name == "CONNECT" => {
                let connected = value.to_bool()?;
                if !connected {
                    Self::add_device_to_schedule(
                        &mut self.not_connected_yet,
                        &prop_change.device_name,
                    );
                } else {
                    Self::add_device_to_schedule(
                        &mut self.not_opts_loaded_yet,
                        &prop_change.device_name,
                    );
                }
                self.to_check_cur_dev = Some(0);
            }
            indi::PropChange::Change {
                prop_name,
                elem_name,
                value,
                ..
            } if **prop_name == "CONNECTION" && **elem_name == "CONNECT" => {
                let connected = value.to_bool()?;
                if connected {
                    Self::add_device_to_schedule(
                        &mut self.not_opts_loaded_yet,
                        &prop_change.device_name,
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn add_device_to_schedule(items: &mut Vec<DeviceIsWaitingForAction>, device_name: &str) {
        let existing = items.iter_mut().find(|dev| dev.name == device_name);
        if let Some(existing) = existing {
            existing.wait_time_ms = 0;
        } else {
            items.push(DeviceIsWaitingForAction {
                name: device_name.to_string(),
                wait_time_ms: 0,
            });
        }
    }

    pub fn notify_timer(&mut self, timer_period_ms: usize) -> anyhow::Result<()> {
        for dev in &mut self.not_connected_yet {
            dev.wait_time_ms += timer_period_ms;
        }

        loop {
            let pos = self
                .not_connected_yet
                .iter()
                .position(|dev| dev.wait_time_ms > DEVICE_WAIT_BEFORE_CONNECT_TIME * 1000);
            let Some(pos) = pos else {
                break;
            };
            let dev = self.not_connected_yet.remove(pos);

            log::info!("Activating device \"{}\" ...", dev.name);
            self.indi.command_enable_device(&dev.name, true, true, None)?;
        }

        for dev in &mut self.not_opts_loaded_yet {
            dev.wait_time_ms += timer_period_ms;
        }

        loop {
            let pos = self
                .not_opts_loaded_yet
                .iter()
                .position(|dev| dev.wait_time_ms > DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME * 1000);
            let Some(pos) = pos else {
                break;
            };

            let dev = self.not_opts_loaded_yet.remove(pos);

            log::info!("Loading options for device \"{}\" ...", dev.name);
            self.indi.command_set_switch_property(
                &dev.name,
                "CONFIG_PROCESS",
                &[("CONFIG_LOAD", true)],
            )?;
        }

        if let Some(to_check_cur_dev) = &mut self.to_check_cur_dev {
            *to_check_cur_dev += timer_period_ms;
            if *to_check_cur_dev >= DEVICE_WAIT_CHECK_CUR_DEV_TIME * 1000 {
                self.to_check_cur_dev = None;
                let mut options = self.options.write().unwrap();

                let all_cameras = self.indi.get_devices_list_by_interface(indi::DriverInterface::CCD);
                if all_cameras.len() == 1 && options.cam.device.as_ref().map(|d| &d.name) != Some(&all_cameras[0].name) {
                    let prev_value = options.cam.device.clone();
                    let new_value = DeviceAndProp {
                        name: all_cameras[0].name.to_string(),
                        prop: "CCD1".to_string(),
                    };
                    log::info!("Camera device correcting from \"{:?}\" to \"{:?}\"", prev_value, new_value);
                    options.cam.device = Some(new_value.clone());
                    self.events.notify(Event::CameraDeviceChanged {
                        from: prev_value,
                        to: new_value
                    });
                }
                let all_mounts = self.indi.get_devices_list_by_interface(indi::DriverInterface::TELESCOPE);
                if all_mounts.len() == 1 && options.mount.device != *all_mounts[0].name {
                    let new_device_name = all_mounts[0].name.to_string();
                    log::info!("Mount device correcting from \"{}\" to \"{}\"", options.mount.device, new_device_name);
                    options.mount.device = new_device_name.clone();
                    self.events.notify(Event::MountDeviceChanged(new_device_name));
                }
                let all_focuser = self.indi.get_devices_list_by_interface(indi::DriverInterface::FOCUSER);
                if all_focuser.len() == 1 && options.focuser.device != *all_focuser[0].name {
                    let new_device_name = all_focuser[0].name.to_string();
                    log::info!("Focuser device correcting from \"{}\" to \"{}\"", options.focuser.device, new_device_name);
                    options.focuser.device = new_device_name.clone();
                    self.events.notify(Event::FocuserDeviceChanged(new_device_name));
                }
                let all_filter_wheels = self.indi.get_devices_list_by_interface(indi::DriverInterface::FILTER);
                if all_filter_wheels.len() == 1 && options.filter_wheel.device != *all_filter_wheels[0].name {
                    let new_device_name = all_filter_wheels[0].name.to_string();
                    log::info!("Filter wheel device correcting from \"{}\" to \"{}\"", options.filter_wheel.device, new_device_name);
                    options.filter_wheel.device = new_device_name.clone();
                    self.events.notify(Event::FltWheelDeviceChanged(new_device_name));
                }
            }
        }
        Ok(())
    }
}
