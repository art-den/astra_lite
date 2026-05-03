use std::sync::Arc;

use crate::indi;

const DEVICE_WAIT_BEFORE_CONNECT_TIME: usize = 1; // in seconds
const DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME: usize = 2; // in seconds

struct DeviceIsWaitingForAction {
    name:         String,
    wait_time_ms: usize,
}

pub struct DevicesWatchdog {
    not_connected_yet: Vec<DeviceIsWaitingForAction>,
    not_opts_loaded_yet:  Vec<DeviceIsWaitingForAction>,
}

impl DevicesWatchdog {
    pub fn new() -> Self {
        Self {
            not_connected_yet: Vec::new(),
            not_opts_loaded_yet: Vec::new(),
        }
    }

    pub fn notify_indi_prop_change(&mut self, prop_change: &indi::PropChangeEvent) -> anyhow::Result<()> {
        match &prop_change.change {
            indi::PropChange::New{ prop_name, elem_name, value, .. }
            if **prop_name == "CONNECTION" && **elem_name == "CONNECT" => {
                let connected = value.to_bool()?;
                if !connected {
                    Self::add_device_to_schedule(&mut self.not_connected_yet, &prop_change.device_name);
                } else {
                    Self::add_device_to_schedule(&mut self.not_opts_loaded_yet, &prop_change.device_name);
                }
            }
            indi::PropChange::Change{ prop_name, elem_name, value, .. }
            if **prop_name == "CONNECTION" && **elem_name == "CONNECT" => {
                let connected = value.to_bool()?;
                if connected {
                    Self::add_device_to_schedule(&mut self.not_opts_loaded_yet, &prop_change.device_name);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn add_device_to_schedule(items: &mut Vec<DeviceIsWaitingForAction>, device_name: &str) {
        let existing = items
            .iter_mut()
            .find(|dev| dev.name == device_name);
        if let Some(existing) = existing {
            existing.wait_time_ms = 0;
        } else {
            items.push(DeviceIsWaitingForAction {
                name:         device_name.to_string(),
                wait_time_ms: 0
            });
        }
    }

    pub fn notify_timer(&mut self, timer_period_ms: usize, indi: &Arc<indi::Connection>) -> anyhow::Result<()> {
        for dev in &mut self.not_connected_yet {
            dev.wait_time_ms += timer_period_ms;
        }

        for dev in &mut self.not_opts_loaded_yet {
            dev.wait_time_ms += timer_period_ms;
        }

        loop {
            let pos = self.not_connected_yet
                .iter()
                .position(|dev| dev.wait_time_ms > DEVICE_WAIT_BEFORE_CONNECT_TIME * 1000);
            let Some(pos) = pos else { break; };
            let dev = &self.not_connected_yet[pos];

            log::info!("Activating device \"{}\" ...", dev.name);
            indi.command_enable_device(&dev.name, true, true, None)?;

            self.not_connected_yet.remove(pos);
        }

        loop {
            let pos = self.not_opts_loaded_yet
                .iter()
                .position(|dev| dev.wait_time_ms > DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME * 1000);
            let Some(pos) = pos else { break; };
            let dev = &self.not_opts_loaded_yet[pos];

            log::info!("Loading options for device \"{}\" ...", dev.name);
            indi.command_set_switch_property(
                &dev.name,
                "CONFIG_PROCESS",
                &[("CONFIG_LOAD", true)]
            )?;

            self.not_opts_loaded_yet.remove(pos);
        }

        Ok(())
    }

}