use std::sync::Arc;

const DEVICE_WAIT_BEFORE_CONNECT_TIME: usize = 1; // in seconds
const DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME: usize = 2; // in seconds
const DEVICE_WAIT_CHECK_CUR_DEV_TIME: usize = 3; // in seconds after the last device appeared
const MAX_ACTION_ATTEMPTS: usize = 3;

struct DeviceIsWaitingForAction {
    name:         String,
    wait_time_ms: usize,
    /// Failed attempts; kept across re-queueing so a flapping device cannot
    /// refresh its retry budget.
    attempts:     usize,
}

/// Brings devices that appeared on the INDI bus up: enables a device some time
/// after it is announced, then loads its saved options after it connects.
pub struct DevicesWatchdog {
    indi:                  Arc<indi::Connection>,
    not_connected_yet:     Vec<DeviceIsWaitingForAction>,
    not_opts_loaded_yet:   Vec<DeviceIsWaitingForAction>,
    time_to_check_cur_dev: Option<usize>,
}

impl DevicesWatchdog {
    pub fn new(indi: &Arc<indi::Connection>) -> Self {
        Self {
            indi:                  Arc::clone(indi),
            not_connected_yet:     Vec::new(),
            not_opts_loaded_yet:   Vec::new(),
            time_to_check_cur_dev: None,
        }
    }

    pub fn reset(&mut self) {
        self.not_connected_yet.clear();
        self.not_opts_loaded_yet.clear();
        self.time_to_check_cur_dev = None;
    }

    pub fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent,
    ) -> eyre::Result<()> {
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
                self.time_to_check_cur_dev = Some(0);
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
            // `attempts` is left as it is on purpose: see its doc
            existing.wait_time_ms = 0;
        } else {
            items.push(DeviceIsWaitingForAction {
                name:         device_name.to_string(),
                wait_time_ms: 0,
                attempts:     0,
            });
        }
    }

    /// Sends the queued actions whose delay has expired.
    ///
    /// Errors are logged, never returned: one flaky device must not abort the
    /// active mode through the HAL tick chain.
    /// `conn_state` is passed in to keep the lock order, see the caller.
    pub fn timer_tick(
        &mut self,
        timer_period_ms: usize,
        conn_state:      &indi::ConnState,
    ) {
        // Not Connected: the server would reject commands with WrongSequence
        if *conn_state != indi::ConnState::Connected {
            return;
        }

        Self::process_due_actions(
            &mut self.not_connected_yet,
            timer_period_ms,
            DEVICE_WAIT_BEFORE_CONNECT_TIME * 1000,
            "activate device",
            |device_name| {
                log::info!("Activating device \"{}\" ...", device_name);
                self.indi.command_enable_device(device_name, true, true, None)
            },
        );

        Self::process_due_actions(
            &mut self.not_opts_loaded_yet,
            timer_period_ms,
            DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME * 1000,
            "load options for device",
            |device_name| {
                log::info!("Loading options for device \"{}\" ...", device_name);
                self.indi.command_set_switch_property(
                    device_name,
                    "CONFIG_PROCESS",
                    &[("CONFIG_LOAD", true)],
                )
            },
        );
    }

    /// A failed action is retried after the full delay, at most
    /// `MAX_ACTION_ATTEMPTS` times; an entry whose device is gone is dropped
    /// right away (it will be queued again if the device reappears).
    fn process_due_actions(
        items:           &mut Vec<DeviceIsWaitingForAction>,
        timer_period_ms: usize,
        threshold_ms:    usize,
        action_desc:     &str,
        command:         impl Fn(&str) -> Result<(), indi::Error>,
    ) {
        items.retain_mut(|dev| {
            dev.wait_time_ms += timer_period_ms;
            if dev.wait_time_ms <= threshold_ms {
                return true; // not due yet
            }

            let dev_name = dev.name.clone();
            match command(&dev_name) {
                Ok(()) => false,
                Err(err) => {
                    dev.attempts += 1;
                    let device_gone = matches!(err, indi::Error::DeviceNotExists(_));

                    if device_gone || dev.attempts >= MAX_ACTION_ATTEMPTS {
                        log::error!("Can't {} \"{}\": {}", action_desc, dev_name, err);
                        false
                    } else {
                        log::warn!(
                            "Can't {} \"{}\": {}. Retrying ...",
                            action_desc, dev_name, err
                        );
                        // Restart the wait, otherwise the retry would follow every tick
                        dev.wait_time_ms = 0;
                        true
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    const PERIOD_MS: usize    = 250; // mirrors `Engine::TIMER_PERIOD_MS`
    const THRESHOLD_MS: usize = 1000;

    fn entry(name: &str) -> DeviceIsWaitingForAction {
        DeviceIsWaitingForAction {
            name:         name.to_string(),
            wait_time_ms: 0,
            attempts:     0,
        }
    }

    #[test]
    fn action_is_sent_only_after_the_delay() {
        let calls = Cell::new(0usize);
        let mut items = vec![entry("dev")];

        // 4 x 250 ms = 1000 ms, which is still not above the threshold
        for _ in 0..4 {
            DevicesWatchdog::process_due_actions(
                &mut items,
                PERIOD_MS,
                THRESHOLD_MS,
                "activate device",
                |_| {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
            );
        }
        assert_eq!(calls.get(), 0);
        assert_eq!(items.len(), 1);

        // 1250 ms passes the threshold
        DevicesWatchdog::process_due_actions(
            &mut items,
            PERIOD_MS,
            THRESHOLD_MS,
            "activate device",
            |_| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert_eq!(calls.get(), 1);
        assert!(items.is_empty());
    }

    #[test]
    fn missing_device_is_dropped_without_retry() {
        let calls = Cell::new(0usize);
        let mut items = vec![entry("gone")];

        for _ in 0..10 {
            DevicesWatchdog::process_due_actions(
                &mut items,
                PERIOD_MS,
                THRESHOLD_MS,
                "activate device",
                |_| {
                    calls.set(calls.get() + 1);
                    Err(indi::Error::DeviceNotExists("gone".into()))
                },
            );
        }

        assert_eq!(calls.get(), 1);
        assert!(items.is_empty());
    }

    #[test]
    fn transient_error_retries_are_bounded_and_spaced() {
        let calls = Cell::new(0usize);
        let mut items = vec![entry("dev")];
        let mut ticks_with_command = Vec::new();

        for tick in 1..=40 {
            let calls_before = calls.get();
            DevicesWatchdog::process_due_actions(
                &mut items,
                PERIOD_MS,
                THRESHOLD_MS,
                "load options for device",
                |_| {
                    calls.set(calls.get() + 1);
                    Err(indi::Error::PropertyNotExists("dev".into(), "CONFIG_PROCESS".into()))
                },
            );
            if calls.get() > calls_before {
                ticks_with_command.push(tick);
            }
            if items.is_empty() {
                break;
            }
        }

        assert_eq!(calls.get(), MAX_ACTION_ATTEMPTS);
        assert!(items.is_empty());
        // first try at tick 5 (4 x 250 == threshold, 5 x 250 is above it),
        // then a full delay per retry
        assert_eq!(ticks_with_command, vec![5, 10, 15]);
    }

    #[test]
    fn entries_after_a_dropped_one_are_still_processed() {
        let commands = RefCell::new(Vec::new());
        let mut items = vec![entry("gone"), entry("kept"), entry("ok")];

        for _ in 0..5 { // exactly the tick at which the delay expires
            DevicesWatchdog::process_due_actions(
                &mut items,
                PERIOD_MS,
                THRESHOLD_MS,
                "activate device",
                |name| {
                    commands.borrow_mut().push(name.to_string());
                    match name {
                        "gone" => Err(indi::Error::DeviceNotExists(name.into())),
                        "kept" => Err(indi::Error::WrongSequence("busy".into())),
                        _ =>     Ok(()),
                    }
                },
            );
        }

        let mut sent = commands.borrow().clone();
        sent.sort();
        assert_eq!(sent, vec!["gone".to_string(), "kept".to_string(), "ok".to_string()]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "kept");
        assert_eq!(items[0].attempts, 1);
    }

    #[test]
    fn requeued_device_keeps_its_retry_budget() {
        let mut items = vec![DeviceIsWaitingForAction {
            name:         "dev".into(),
            wait_time_ms: 900,
            attempts:     2,
        }];

        DevicesWatchdog::add_device_to_schedule(&mut items, "dev");

        assert_eq!(items[0].wait_time_ms, 0);
        assert_eq!(items[0].attempts, 2);
    }
}
