use std::sync::Arc;

use crate::indi;

const DEVICE_WAIT_BEFORE_ACTIVATE_TIME: usize = 1; // in seconds

struct NotInitializedYet {
    name:         String,
    wait_time_ms: usize,
}

pub struct DevicesWatchdog {
    not_init_yet: Vec<NotInitializedYet>,
}

impl DevicesWatchdog {
    pub fn new() -> Self {
        Self {
            not_init_yet: Vec::new(),
        }
    }

    pub fn notify_indi_prop_change(&mut self, prop_change: &indi::PropChangeEvent) -> anyhow::Result<()> {
        let indi::PropChange::New{ prop_name, elem_name, value, .. } = &prop_change.change else {
            return Ok(());
        };
        if !(**elem_name == "CONNECT" && **prop_name == "CONNECTION") {
            return Ok(());
        }
        let connected = value.to_bool()?;
        if connected {
            return Ok(());
        }
        let existing = self.not_init_yet
            .iter_mut()
            .find(|dev| dev.name == **prop_change.device_name);
        if let Some(existing) = existing {
            existing.wait_time_ms = 0;
        } else {
            self.not_init_yet.push(NotInitializedYet {
                name:         prop_change.device_name.to_string(),
                wait_time_ms: 0
            });
        }
        Ok(())
    }

    pub fn notify_timer(&mut self, timer_period_ms: usize, indi: &Arc<indi::Connection>) -> anyhow::Result<()> {
        for dev in &mut self.not_init_yet {
            dev.wait_time_ms += timer_period_ms;
        }
        loop {
            let pos = self.not_init_yet
                .iter()
                .position(|dev| dev.wait_time_ms > DEVICE_WAIT_BEFORE_ACTIVATE_TIME * 1000);
            let Some(pos) = pos else { break; };
            let dev = &self.not_init_yet[pos];
            log::info!("Activating device \"{}\" ...", dev.name);
            indi.command_enable_device(&dev.name, true, true, None)?;
            self.not_init_yet.remove(pos);
        }
        Ok(())
    }

}