use std::sync::Arc;

use crate::hal::DeviceType;

const DEVICE_WAIT_BEFORE_CONNECT_TIME: usize = 1; // in seconds
const DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME: usize = 2; // in seconds

#[derive(Debug, Clone, PartialEq, Eq)]
enum DevState {
    /// Announced but not connected: enabling the device is due after `wait_ms`
    WaitingForConnect { wait_ms: usize },
    /// Enabled (or announced as connected): loading the saved options is due
    /// after `wait_ms`
    WaitingForLoadOpts { wait_ms: usize },
    /// The action was sent (successfully or not); a new `CONNECTION` event is
    /// needed to queue the device again
    Idle,
}

/// The watchdog action owed to a device, derived from its state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Activate,
    LoadOpts,
}

impl Command {
    fn desc(self) -> &'static str {
        match self {
            Command::Activate => "activate device",
            Command::LoadOpts => "load options for device",
        }
    }
}

struct WatchedDevice {
    name:  String,
    type_: DeviceType,
    state: DevState,
}

impl WatchedDevice {
    /// Advances the wait and sends the action that has come due.
    ///
    /// One attempt per queued event: on failure the device goes `Idle` and
    /// waits for a new `CONNECTION` event to be queued again
    fn process_device(
        &mut self,
        timer_period_ms: usize,
        command:         impl Fn(&str, Command) -> Result<(), indi::Error>,
    ) {
        let cmd = match &mut self.state {
            DevState::WaitingForConnect { wait_ms } => {
                *wait_ms += timer_period_ms;
                if *wait_ms <= DEVICE_WAIT_BEFORE_CONNECT_TIME * 1000 {
                    return; // not due yet
                }
                Command::Activate
            }
            DevState::WaitingForLoadOpts { wait_ms } => {
                *wait_ms += timer_period_ms;
                if *wait_ms <= DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME * 1000 {
                    return; // not due yet
                }
                Command::LoadOpts
            }
            DevState::Idle => return,
        };

        self.state = match command(&self.name, cmd) {
            Ok(()) => match cmd {
                // `Activate` acceptance arms the opts load without waiting for the
                // connect event; a real connect event re-arms the wait, and a repeated
                // `CONFIG_LOAD` is idempotent
                Command::Activate => DevState::WaitingForLoadOpts { wait_ms: 0 },
                Command::LoadOpts => DevState::Idle,
            },
            Err(err) => {
                log::error!("Can't {} \"{}\": {}", cmd.desc(), self.name, err);
                DevState::Idle
            }
        };
    }
}

/// Brings devices that appeared on the INDI bus up: enables a device some time
/// after it is announced, then loads its saved options once the device is
/// enabled or connected.
///
/// Devices are never removed from the list: an entry lives until `reset()` and
/// only changes its state.
pub struct DevicesWatchdog {
    indi:    Arc<indi::Connection>,
    devices: Vec<WatchedDevice>,
}

impl DevicesWatchdog {
    pub fn new(indi: &Arc<indi::Connection>) -> Self {
        Self {
            indi:    Arc::clone(indi),
            devices: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.devices.clear();
    }

    pub fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent,
    ) -> eyre::Result<()> {
        // `New` covers a device announced while still disconnected (enabling is
        // owed), a `Change` only matters once the device has connected
        let (connected, is_new) = match &prop_change.change {
            indi::PropChange::New {
                prop_name,
                elem_name,
                value,
                ..
            }
            | indi::PropChange::Change {
                prop_name,
                elem_name,
                value,
                ..
            } if **prop_name == "CONNECTION" && **elem_name == "CONNECT" => (
                value.to_bool()?,
                matches!(&prop_change.change, indi::PropChange::New { .. }),
            ),
            _ => return Ok(()),
        };

        let state = if connected {
            DevState::WaitingForLoadOpts { wait_ms: 0 }
        } else if is_new {
            DevState::WaitingForConnect { wait_ms: 0 }
        } else {
            return Ok(()); // the device went offline: nothing to queue
        };

        let device_type = self.device_type(&prop_change.device_name);
        Self::schedule_device(
            &mut self.devices,
            &prop_change.device_name,
            device_type,
            state,
        );
        Ok(())
    }

    /// Queues a device for `state`, restarting its wait. A device that is
    /// already in the list (even `Idle`) is reused, never duplicated.
    fn schedule_device(
        devices:     &mut Vec<WatchedDevice>,
        device_name: &str,
        device_type: DeviceType,
        state:       DevState,
    ) {
        match devices.iter_mut().find(|dev| dev.name == device_name) {
            Some(dev) => {
                dev.type_ = device_type;
                dev.state = state;
            }
            None => devices.push(WatchedDevice {
                name:  device_name.to_string(),
                type_: device_type,
                state,
            }),
        }
    }

    /// A `CONNECTION` property implies the device is already announced, so its
    /// driver interface is known by now
    fn device_type(&self, device_name: &str) -> DeviceType {
        self.indi
            .get_devices_list()
            .iter()
            .find(|dev| dev.name.as_str() == device_name)
            .map(|dev| super::IndiHalImpl::driver_interface_to_dev_type(dev.interface))
            .unwrap_or_else(DeviceType::empty)
    }

    /// Sends the due actions.
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

        let indi = &self.indi;
        for dev in &mut self.devices {
            dev.process_device(timer_period_ms, |name, cmd| match cmd {
                Command::Activate => {
                    log::info!("Activating device \"{}\" ...", name);
                    indi.command_enable_device(name, true, true, None)
                }
                Command::LoadOpts => {
                    log::info!("Loading options for device \"{}\" ...", name);
                    indi.command_set_switch_property(
                        name,
                        "CONFIG_PROCESS",
                        &[("CONFIG_LOAD", true)],
                    )
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const PERIOD_MS: usize = 250; // mirrors `Engine::TIMER_PERIOD_MS`

    /// Stub command sender: records `(name, Command)`, fails for `fails` names
    struct Stub {
        sent:  RefCell<Vec<(String, Command)>>,
        fails: Vec<&'static str>,
    }

    impl Stub {
        fn new() -> Self {
            Self {
                sent:  RefCell::new(Vec::new()),
                fails: Vec::new(),
            }
        }

        fn failing(mut self, names: &[&'static str]) -> Self {
            self.fails = names.to_vec();
            self
        }

        fn command(&self) -> impl Fn(&str, Command) -> Result<(), indi::Error> {
            |name, cmd| {
                self.sent.borrow_mut().push((name.to_string(), cmd));
                if self.fails.contains(&name) {
                    Err(indi::Error::WrongSequence(name.into()))
                } else {
                    Ok(())
                }
            }
        }

        fn count(&self) -> usize {
            self.sent.borrow().len()
        }

        /// Recorded commands, sorted by device name
        fn commands(&self) -> Vec<(String, Command)> {
            let mut sent = self.sent.borrow().clone();
            sent.sort_by(|a, b| a.0.cmp(&b.0));
            sent
        }
    }

    fn entry(name: &str, type_: DeviceType, state: DevState) -> WatchedDevice {
        WatchedDevice {
            name:  name.to_string(),
            type_,
            state,
        }
    }

    fn conn_evt(device: &str, is_new: bool, connected: bool) -> indi::PropChangeEvent {
        let prop_name = Arc::new("CONNECTION".to_string());
        let elem_name = Arc::new("CONNECT".to_string());
        let value     = indi::PropValue::Switch(connected);
        let change = if is_new {
            indi::PropChange::New {
                prop_name,
                elem_name,
                value,
                state: indi::PropState::Ok,
            }
        } else {
            indi::PropChange::Change {
                prop_name,
                elem_name,
                value,
                prev_state: indi::PropState::Idle,
                new_state:  indi::PropState::Ok,
            }
        };
        indi::PropChangeEvent {
            timestamp:   None,
            device_name: Arc::new(device.to_string()),
            change,
        }
    }

    #[test]
    fn connect_is_sent_only_after_the_delay() {
        let stub  = Stub::new();
        let mut dev =
            entry("dev", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 });

        // 4 x 250 ms = 1000 ms, which is still not above the 1 s threshold
        for _ in 0..4 {
            dev.process_device(PERIOD_MS, stub.command());
        }
        assert_eq!(stub.count(), 0);
        assert!(matches!(dev.state, DevState::WaitingForConnect { .. }));

        // 1250 ms passes the threshold
        dev.process_device(PERIOD_MS, stub.command());
        assert_eq!(stub.commands(), vec![("dev".to_string(), Command::Activate)]);
        assert_eq!(dev.state, DevState::WaitingForLoadOpts { wait_ms: 0 });
    }

    #[test]
    fn successful_opts_load_finishes_the_device() {
        let stub  = Stub::new();
        let mut dev =
            entry("dev", DeviceType::TELESCOPE, DevState::WaitingForLoadOpts { wait_ms: 0 });

        // the opts threshold (2 s) needs more ticks than the connect one
        for _ in 0..8 {
            dev.process_device(PERIOD_MS, stub.command());
        }
        assert_eq!(stub.count(), 0);

        dev.process_device(PERIOD_MS, stub.command());
        assert_eq!(stub.commands(), vec![("dev".to_string(), Command::LoadOpts)]);
        assert_eq!(dev.state, DevState::Idle);
    }

    #[test]
    fn failed_action_gets_no_retry_and_drops_to_idle() {
        let stub  = Stub::new().failing(&["dev"]);
        let mut dev =
            entry("dev", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 });

        for _ in 0..20 {
            dev.process_device(PERIOD_MS, stub.command());
        }

        assert_eq!(stub.count(), 1);
        assert_eq!(dev.state, DevState::Idle);
    }

    #[test]
    fn all_devices_are_processed_even_when_one_fails() {
        let stub    = Stub::new().failing(&["gone"]);
        let mut devices = vec![
            entry("gone", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 }),
            entry("kept", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 }),
            entry("ok", DeviceType::FOCUSER, DevState::WaitingForLoadOpts { wait_ms: 0 }),
        ];

        // 5 x 250 ms: the connect entries are due, the opts one is not yet
        for _ in 0..5 {
            for dev in &mut devices {
                dev.process_device(PERIOD_MS, stub.command());
            }
        }

        assert_eq!(stub.commands(), vec![
            ("gone".to_string(), Command::Activate),
            ("kept".to_string(), Command::Activate),
        ]);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].state, DevState::Idle);
        assert_eq!(devices[1].state, DevState::WaitingForLoadOpts { wait_ms: 0 });
        // `ok` accumulated its wait but has not reached the 2 s threshold
        assert!(matches!(
            devices[2].state,
            DevState::WaitingForLoadOpts { wait_ms: 1250 }
        ));
    }

    #[test]
    fn rescheduling_idle_device_restarts_the_wait() {
        let mut devices = vec![entry("dev", DeviceType::CAMERA, DevState::Idle)];

        DevicesWatchdog::schedule_device(
            &mut devices,
            "dev",
            DeviceType::CAMERA,
            DevState::WaitingForConnect { wait_ms: 0 },
        );

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].state, DevState::WaitingForConnect { wait_ms: 0 });
    }

    #[test]
    fn schedule_adds_unknown_device_once() {
        let mut devices = Vec::new();

        DevicesWatchdog::schedule_device(
            &mut devices,
            "new",
            DeviceType::FLT_WHEEL,
            DevState::WaitingForConnect { wait_ms: 500 },
        );
        DevicesWatchdog::schedule_device(
            &mut devices,
            "new",
            DeviceType::FLT_WHEEL,
            DevState::WaitingForLoadOpts { wait_ms: 0 },
        );

        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "new");
        assert!(devices[0].type_.contains(DeviceType::FLT_WHEEL));
        assert_eq!(devices[0].state, DevState::WaitingForLoadOpts { wait_ms: 0 });
    }

    #[test]
    fn timer_tick_makes_no_progress_while_disconnected() {
        let mut watchdog = DevicesWatchdog::new(&Arc::new(indi::Connection::new()));

        watchdog
            .notify_indi_prop_change(&conn_evt("dev", true, false))
            .unwrap();
        // If the guard broke, the command would fail with WrongSequence and drop
        // the entry to `Idle`
        for _ in 0..10 {
            watchdog.timer_tick(PERIOD_MS, &indi::ConnState::Disconnected);
        }

        assert_eq!(
            watchdog.devices[0].state,
            DevState::WaitingForConnect { wait_ms: 0 }
        );
    }

    #[test]
    fn notify_queues_devices_by_event_kind() {
        let mut watchdog = DevicesWatchdog::new(&Arc::new(indi::Connection::new()));

        watchdog.notify_indi_prop_change(&conn_evt("new_off", true, false)).unwrap();
        watchdog.notify_indi_prop_change(&conn_evt("new_on", true, true)).unwrap();
        watchdog.notify_indi_prop_change(&conn_evt("changed_on", false, true)).unwrap();
        // going offline queues nothing
        watchdog.notify_indi_prop_change(&conn_evt("changed_off", false, false)).unwrap();

        assert_eq!(watchdog.devices.len(), 3);
        assert!(matches!(
            watchdog.devices[0].state,
            DevState::WaitingForConnect { wait_ms: 0 }
        ));
        assert!(matches!(
            watchdog.devices[1].state,
            DevState::WaitingForLoadOpts { wait_ms: 0 }
        ));
        assert!(matches!(
            watchdog.devices[2].state,
            DevState::WaitingForLoadOpts { wait_ms: 0 }
        ));
        // a driver unknown to the bus resolves to an empty type
        assert!(watchdog.devices[0].type_.is_empty());
    }
}
