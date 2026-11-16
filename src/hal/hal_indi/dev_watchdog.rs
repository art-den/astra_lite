use std::sync::Arc;

use crate::hal::{
    DeviceType,
    events::{HalEvent, HalEventHandlers},
};

const DEVICE_WAIT_BEFORE_CONNECT_TIME: usize = 1; // in seconds
const DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME: usize = 2; // in seconds

const MAX_WAIT_BLOB_TIME: usize = 30; // in seconds
const MAX_SHUTDOWN_TIME: usize = 2; // in seconds
const WAIT_EXPOSURE_TIME: usize = 10; // in seconds
const MAX_SWITCHING_ON_TIME: usize = 5; // in seconds after exposure property appeared
const INIT_DELAY: usize = 2; // in seconds

const ALL_CCDS: [indi::CamCcd; 2] = [indi::CamCcd::Main, indi::CamCcd::Guider];

#[derive(Debug, Clone, PartialEq, Eq)]
enum DevState {
    /// Announced but not connected: enabling the device is due after `wait_ms`
    WaitingForConnect { wait_ms: usize },
    /// Enabled (or announced as connected): loading the saved options is due
    /// after `wait_ms`
    WaitingForLoadOpts { wait_ms: usize },
    /// Cameras only: the init events for the announced features are due
    /// `wait_ms` after `CONFIG_LOAD` was sent; this state is armed by
    /// `CONFIG_LOAD` only
    WaitingCameraInit { wait_ms: usize },
    /// The action was sent (successfully or not); a new `CONNECTION` event is
    /// needed to queue the device again
    Idle,
}

/// An action the watchdog owes a device: bring-up actions come from `state`,
/// recovery actions from the CCD machines, init actions from the camera wave
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Activate,
    LoadOpts,
    /// Power the camera off to clear a hung exposure
    Disable,
    /// Select the maximum CCD resolution during camera init
    SelectMaxRes,
}

impl Command {
    fn desc(self) -> &'static str {
        match self {
            Command::Activate => "activate device",
            Command::LoadOpts => "load options for device",
            Command::Disable => "power off device",
            Command::SelectMaxRes => "select maximum resolution for device",
        }
    }
}

/// Recovery machine for a hung CCD exposure
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CcdMode {
    /// Not watched: not a camera, not connected yet, or recovery abandoned.
    /// Coverage returns via `arm_ccd_modes()` when the device connects again
    Inactive,
    Waiting,
    WaitBlob(usize),
    Shutdown(usize),
    WaitExposureProp(usize),
    WaitAfterRestart(usize),
}

/// Per CCD recovery modes of a device. One physical camera powers off as a
/// whole, so a second hung CCD joins the restart already running for the first
/// instead of starting another power cycle
#[derive(Clone, Copy)]
struct CcdModes {
    main:   CcdMode,
    guider: CcdMode,
}

impl CcdModes {
    fn inactive() -> Self {
        Self { main: CcdMode::Inactive, guider: CcdMode::Inactive }
    }

    fn get(&self, ccd: indi::CamCcd) -> CcdMode {
        match ccd {
            indi::CamCcd::Main   => self.main,
            indi::CamCcd::Guider => self.guider,
        }
    }

    fn set(&mut self, ccd: indi::CamCcd, mode: CcdMode) {
        match ccd {
            indi::CamCcd::Main   => self.main = mode,
            indi::CamCcd::Guider => self.guider = mode,
        }
    }

    fn other(&self, ccd: indi::CamCcd) -> CcdMode {
        match ccd {
            indi::CamCcd::Main   => self.guider,
            indi::CamCcd::Guider => self.main,
        }
    }
}

/// Camera features announced via `New` property events, waiting to be reported
/// by the next init wave
#[derive(Default, Clone, Copy)]
struct CameraInitFlags {
    cooler:    bool,
    fan:       bool,
    heater:    bool,
    max_res:   bool,
    focal_len: bool,
}

struct WatchedDevice {
    name:      String,
    type_:     DeviceType,
    state:     DevState,
    /// Lives outside `state`: the feature properties are announced long before
    /// `WaitingCameraInit` is reached
    cam_init:  CameraInitFlags,
    ccd_modes: CcdModes,
}

impl WatchedDevice {
    fn is_camera(&self) -> bool {
        self.type_.contains(DeviceType::CAMERA)
    }

    fn device_id(&self, ccd: indi::CamCcd) -> String {
        if ccd == indi::CamCcd::Guider {
            super::ccd2_id(&self.name)
        } else {
            self.name.clone()
        }
    }

    /// Advances both machines. Recovery errors are reported before init ones:
    /// they are the severe ones
    fn process(
        &mut self,
        timer_period_ms: usize,
        command:         &mut impl FnMut(&str, Command) -> eyre::Result<()>,
        send_event:      &mut impl FnMut(HalEvent),
    ) -> eyre::Result<()> {
        let recovery_result = self.process_ccd_recovery(timer_period_ms, command, send_event);
        let state_result    = self.process_state(timer_period_ms, command, send_event);
        if recovery_result.is_err()
            && let Err(ref err) = state_result
        {
            log::error!("Additional camera error from device watchdog: {err:#}");
        }
        recovery_result.and(state_result)
    }

    /// Advances the bring-up wait and sends the action that has come due.
    ///
    /// One attempt per queued event: on failure the device goes `Idle` and
    /// waits for a new `CONNECTION` event to be queued again. Bring-up errors
    /// are logged, only camera init errors are returned
    fn process_state(
        &mut self,
        timer_period_ms: usize,
        command:         &mut impl FnMut(&str, Command) -> eyre::Result<()>,
        send_event:      &mut impl FnMut(HalEvent),
    ) -> eyre::Result<()> {
        let cmd = match &mut self.state {
            DevState::WaitingForConnect { wait_ms } => {
                *wait_ms += timer_period_ms;
                if *wait_ms <= DEVICE_WAIT_BEFORE_CONNECT_TIME * 1000 {
                    return Ok(()); // not due yet
                }
                Command::Activate
            }
            DevState::WaitingForLoadOpts { wait_ms } => {
                *wait_ms += timer_period_ms;
                if *wait_ms <= DEVICE_WAIT_BEFORE_LOAD_OPTS_TIME * 1000 {
                    return Ok(()); // not due yet
                }
                Command::LoadOpts
            }
            DevState::WaitingCameraInit { wait_ms } => {
                *wait_ms += timer_period_ms;
                if *wait_ms <= INIT_DELAY * 1000 {
                    return Ok(()); // not due yet
                }
                self.state = DevState::Idle;
                return self.fire_camera_init(command, send_event);
            }
            DevState::Idle => return Ok(()),
        };

        self.state = match command(&self.name, cmd) {
            Ok(()) => match cmd {
                // `Activate` acceptance arms the opts load without waiting for the
                // connect event; a real connect event re-arms the wait, and a repeated
                // `CONFIG_LOAD` is idempotent
                Command::Activate => DevState::WaitingForLoadOpts { wait_ms: 0 },
                // A camera gets its init event round after the options are loaded
                Command::LoadOpts => {
                    if self.is_camera() {
                        DevState::WaitingCameraInit { wait_ms: 0 }
                    } else {
                        DevState::Idle
                    }
                }
                // recovery and init actions are never queued by `state`
                Command::Disable | Command::SelectMaxRes => DevState::Idle,
            },
            Err(err) => {
                log::error!("Can't {} \"{}\": {}", cmd.desc(), self.name, err);
                // A camera is already enabled when only the opts load fails,
                // so its init wave is still due
                if self.is_camera() && cmd == Command::LoadOpts {
                    DevState::WaitingCameraInit { wait_ms: 0 }
                } else {
                    DevState::Idle
                }
            }
        };
        Ok(())
    }

    /// Reports a camera feature for both device ids (main and CCD2)
    fn send_to_both_ccds(
        &self,
        send_event: &mut impl FnMut(HalEvent),
        make_event: impl Fn(Arc<String>) -> HalEvent,
    ) {
        for ccd in ALL_CCDS {
            send_event(make_event(Arc::new(self.device_id(ccd))));
        }
    }

    /// Reports the camera features announced since the last wave
    fn fire_camera_init(
        &mut self,
        command:    &mut impl FnMut(&str, Command) -> eyre::Result<()>,
        send_event: &mut impl FnMut(HalEvent),
    ) -> eyre::Result<()> {
        let flags = std::mem::take(&mut self.cam_init);

        if flags.cooler {
            self.send_to_both_ccds(send_event, HalEvent::CameraIsReadyForCooling);
        }
        if flags.fan {
            self.send_to_both_ccds(send_event, HalEvent::CameraIsReadyForCtrlFan);
        }
        if flags.heater {
            self.send_to_both_ccds(send_event, HalEvent::CameraIsReadyForCtrlHeater);
        }
        if flags.max_res {
            command(&self.name, Command::SelectMaxRes)?;
        }
        if flags.focal_len {
            send_event(HalEvent::CameraNeedInitTelescopeFocalLen(Arc::new(self.name.clone())));
        }
        Ok(())
    }

    /// Advances the recovery machine of each CCD. All CCDs are ticked even if
    /// one fails; only the first error is returned
    fn process_ccd_recovery(
        &mut self,
        timer_period_ms: usize,
        command:         &mut impl FnMut(&str, Command) -> eyre::Result<()>,
        send_event:      &mut impl FnMut(HalEvent),
    ) -> eyre::Result<()> {
        let mut first_err: Option<eyre::Report> = None;

        for ccd in ALL_CCDS {
            let next = match self.ccd_modes.get(ccd) {
                CcdMode::Inactive | CcdMode::Waiting => continue,

                CcdMode::WaitBlob(time_ms) => {
                    let time_ms = time_ms + timer_period_ms;
                    if time_ms < MAX_WAIT_BLOB_TIME * 1000 {
                        self.ccd_modes.set(ccd, CcdMode::WaitBlob(time_ms));
                        continue;
                    }
                    log::error!(
                        "Waiting for BLOB of INDI camera {} too long (> {}) seconds",
                        self.name, MAX_WAIT_BLOB_TIME
                    );
                    // The camera powers off as a whole: join the restart already
                    // running for the other CCD instead of a second power cycle.
                    // The joiner skips the power-off wait (its `WaitExposureProp`
                    // window may overlap the leader's remaining `Shutdown` wait)
                    // and sends its own `CameraNeedRestartExposure`
                    match self.ccd_modes.other(ccd) {
                        CcdMode::Shutdown(_) | CcdMode::WaitExposureProp(_) => {
                            CcdMode::WaitExposureProp(0)
                        }
                        CcdMode::WaitAfterRestart(_) => CcdMode::WaitAfterRestart(0),
                        _ => {
                            log::info!("Shutdown INDI camera {} ...", self.name);
                            match command(&self.name, Command::Disable) {
                                Ok(()) => CcdMode::Shutdown(0),
                                Err(err) => {
                                    first_err = first_err
                                        .or_else(|| Some(self.abandon(ccd, "shutdown", err)));
                                    continue;
                                }
                            }
                        }
                    }
                }

                CcdMode::Shutdown(time_ms) => {
                    let time_ms = time_ms + timer_period_ms;
                    if time_ms < MAX_SHUTDOWN_TIME * 1000 {
                        self.ccd_modes.set(ccd, CcdMode::Shutdown(time_ms));
                        continue;
                    }
                    log::info!("Switching on camera {} ...", self.name);
                    // On failure the device stays powered off: switching it back
                    // on is up to the bring-up machine, which re-arms on the next
                    // `CONNECTION` event
                    match command(&self.name, Command::Activate) {
                        Ok(()) => CcdMode::WaitExposureProp(0),
                        Err(err) => {
                            first_err = first_err
                                .or_else(|| Some(self.abandon(ccd, "switch on", err)));
                            continue;
                        }
                    }
                }

                CcdMode::WaitExposureProp(time_ms) => {
                    let time_ms = time_ms + timer_period_ms;
                    if time_ms < WAIT_EXPOSURE_TIME * 1000 {
                        self.ccd_modes.set(ccd, CcdMode::WaitExposureProp(time_ms));
                        continue;
                    }
                    self.ccd_modes.set(ccd, CcdMode::Waiting);
                    first_err = first_err.or_else(|| {
                        Some(eyre::eyre!(
                            "Waiting camera restart too long (>{}s)!",
                            WAIT_EXPOSURE_TIME
                        ))
                    });
                    continue;
                }

                CcdMode::WaitAfterRestart(time_ms) => {
                    let time_ms = time_ms + timer_period_ms;
                    if time_ms < MAX_SWITCHING_ON_TIME * 1000 {
                        self.ccd_modes.set(ccd, CcdMode::WaitAfterRestart(time_ms));
                        continue;
                    }
                    send_event(HalEvent::CameraNeedRestartExposure(Arc::new(
                        self.device_id(ccd),
                    )));
                    CcdMode::Waiting
                }
            };
            self.ccd_modes.set(ccd, next);
        }

        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Gives up on an unreachable camera: disarms the CCD mode and builds the
    /// error reported to the user. No retry: a device that cannot be commanded
    /// at all means a hardware fault and needs the user
    fn abandon(&mut self, ccd: indi::CamCcd, action: &str, err: eyre::Report) -> eyre::Report {
        self.ccd_modes.set(ccd, CcdMode::Inactive);
        log::warn!(
            "Camera watchdog gives up on INDI camera {} (CCD {:?}), \
             no recovery until the device reconnects",
            self.name, ccd
        );
        eyre::eyre!(
            "Cannot {action} INDI camera {name}, it is unreachable. \
             Camera recovery abandoned! Reason: {err}",
            name = self.name,
        )
    }

    /// Starts watching the CCDs of a (re)connected camera. A recovery in
    /// progress is left alone: this event can be the recovery's own power cycle
    /// announcing the device again
    fn arm_ccd_modes(&mut self) {
        if !self.is_camera() {
            return;
        }
        for ccd in ALL_CCDS {
            if self.ccd_modes.get(ccd) == CcdMode::Inactive {
                self.ccd_modes.set(ccd, CcdMode::Waiting);
            }
        }
    }

    fn notify_exposure_prop_changed(
        &mut self,
        ccd:        indi::CamCcd,
        prev_state: indi::PropState,
        new_state:  indi::PropState,
        value:      f64,
    ) {
        let is_ready =
            prev_state == indi::PropState::Busy &&
            new_state == indi::PropState::Ok;
        let is_zero = value == 0.0;
        let is_waiting_for_blob = is_zero && new_state == indi::PropState::Busy;
        let mode = self.ccd_modes.get(ccd);
        if is_waiting_for_blob && mode == CcdMode::Waiting {
            self.ccd_modes.set(ccd, CcdMode::WaitBlob(0));
        }
        if (is_ready || !is_zero) && matches!(mode, CcdMode::WaitBlob(_)) {
            self.ccd_modes.set(ccd, CcdMode::Waiting);
        }
    }

    fn notify_exposure_new_prop(&mut self, ccd: indi::CamCcd) {
        if matches!(self.ccd_modes.get(ccd), CcdMode::WaitExposureProp(_)) {
            self.ccd_modes.set(ccd, CcdMode::WaitAfterRestart(0));
        }
    }

    /// Collects the announced camera features. Only `CONFIG_LOAD` arms the
    /// init wave (see `process_state`): features announced late are reported
    /// by the next CONFIG_LOAD round
    fn notify_new_indi_prop(&mut self, prop_name: &str, elem_name: &str) {
        if indi::Connection::camera_is_temperature_property(prop_name, elem_name) {
            self.cam_init.cooler = true;
        }
        if indi::Connection::camera_is_fan_str_property(prop_name) {
            self.cam_init.fan = true;
        }
        if indi::Connection::camera_is_heater_str_property(prop_name) {
            self.cam_init.heater = true;
        }
        if prop_name == "CCD_RESOLUTION" {
            self.cam_init.max_res = true;
        }
        if prop_name == "SCOPE_INFO" && elem_name == "FOCAL_LENGTH" {
            self.cam_init.focal_len = true;
        }
    }
}

/// Brings devices that appeared on the INDI bus up: enables a device some time
/// after it is announced, loads its saved options, then (for cameras) reports
/// the announced features through the init events.
///
/// Also watches the CCDs of cameras: when an exposure BLOB never arrives, the
/// camera is powered off and back on, and the exposure is restarted.
///
/// Devices are never removed from the list: an entry lives until `reset_all()`
/// and only changes its states.
pub struct DevicesWatchdog {
    indi:    Arc<indi::Connection>,
    events:  Arc<HalEventHandlers>,
    devices: Vec<WatchedDevice>,
}

impl DevicesWatchdog {
    pub fn new(indi: &Arc<indi::Connection>, events: &Arc<HalEventHandlers>) -> Self {
        Self {
            indi:    Arc::clone(indi),
            events:  Arc::clone(events),
            devices: Vec::new(),
        }
    }

    pub fn reset_all(&mut self) {
        self.devices.clear();
    }

    /// Clears the bring-up waits after a bus error. Camera recovery state is
    /// kept on purpose: the reconnect can restore the devices in flight
    pub fn reset_device_states(&mut self) {
        for dev in &mut self.devices {
            dev.state = DevState::Idle;
        }
    }

    pub fn notify_indi_prop_change(
        &mut self,
        prop_change: &indi::PropChangeEvent,
    ) -> eyre::Result<()> {
        // `New` covers a device announced while still disconnected (enabling is
        // owed), a `Change` only matters once the device has connected
        let conn_evt = match &prop_change.change {
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
            } if **prop_name == "CONNECTION" && **elem_name == "CONNECT" =>
                Some((
                    value.to_bool()?,
                    matches!(&prop_change.change, indi::PropChange::New { .. }),
                )),
            _ => None,
        };

        if let Some((connected, is_new)) = conn_evt {
            let state = if connected {
                Some(DevState::WaitingForLoadOpts { wait_ms: 0 })
            } else if is_new {
                Some(DevState::WaitingForConnect { wait_ms: 0 })
            } else {
                None // the device went offline: nothing to queue
            };

            let device_type = self.device_type(&prop_change.device_name);
            if let Some(state) = state {
                Self::schedule_device(
                    &mut self.devices,
                    &prop_change.device_name,
                    device_type,
                    state,
                );
            }

            if connected
                && let Some(dev) = self.device_mut(&prop_change.device_name)
            {
                dev.arm_ccd_modes();
            }
            return Ok(());
        }

        let Some(dev) = self.device_mut(&prop_change.device_name) else {
            return Ok(());
        };

        match &prop_change.change {
            indi::PropChange::Change {
                prop_name,
                elem_name,
                prev_state,
                new_state,
                value,
            } => {
                if let Some(ccd) =
                    indi::Connection::camera_get_ccd_for_property(prop_name, elem_name)
                {
                    dev.notify_exposure_prop_changed(
                        ccd,
                        *prev_state,
                        *new_state,
                        value.to_f64().unwrap_or(f64::NAN),
                    );
                }
            }
            indi::PropChange::New { prop_name, elem_name, .. } => {
                if let Some(ccd) =
                    indi::Connection::camera_get_ccd_for_property(prop_name, elem_name)
                {
                    dev.notify_exposure_new_prop(ccd);
                }
                // Flags are collected even while the type is not yet resolved,
                // the wave runs for cameras only anyway
                dev.notify_new_indi_prop(prop_name, elem_name);
            }
            _ => {}
        }
        Ok(())
    }

    /// Refreshes the device type from a `NewDevice`/`DeviceConnected` event
    /// and arms CCD watching: the `CONNECTION` property can be announced
    /// before `DRIVER_INFO`, when `device_type()` cannot resolve the interface
    pub fn notify_device_announced(&mut self, device_name: &str, device_type: DeviceType) {
        // The entry is always there: `NewDevice` follows the `CONNECTION` event
        // on the same in-order event channel of the indi crate
        let Some(dev) = self.device_mut(device_name) else {
            return;
        };
        if !device_type.is_empty() {
            dev.type_ = device_type;
        }
        dev.arm_ccd_modes();
    }

    /// Disarms the idle CCD modes of a device that left the bus. Modes in the
    /// middle of a recovery are kept: deleting them would abandon the restart
    pub fn notify_device_deleted(&mut self, device_name: &str) {
        let Some(dev) = self.device_mut(device_name) else {
            return;
        };
        for ccd in ALL_CCDS {
            if dev.ccd_modes.get(ccd) == CcdMode::Waiting {
                dev.ccd_modes.set(ccd, CcdMode::Inactive);
            }
        }
    }

    fn device_mut(&mut self, device_name: &str) -> Option<&mut WatchedDevice> {
        self.devices.iter_mut().find(|dev| dev.name == device_name)
    }

    /// Queues a device for `state`, restarting its wait and refreshing its type
    /// (an unresolvable type never overwrites a known one). A device that is
    /// already in the list (even `Idle`) is reused, never duplicated.
    fn schedule_device(
        devices:     &mut Vec<WatchedDevice>,
        device_name: &str,
        device_type: DeviceType,
        state:       DevState,
    ) {
        match devices.iter_mut().find(|dev| dev.name == device_name) {
            Some(dev) => {
                if !device_type.is_empty() {
                    dev.type_ = device_type;
                }
                dev.state = state;
            }
            None => devices.push(WatchedDevice {
                name:      device_name.to_string(),
                type_:     device_type,
                state,
                cam_init:  CameraInitFlags::default(),
                ccd_modes: CcdModes::inactive(),
            }),
        }
    }

    /// The driver interface may still be unknown when `CONNECTION` is announced
    /// before `DRIVER_INFO`; an empty type then means "unknown", so it must
    /// never overwrite a known one (see `notify_device_announced`)
    fn device_type(&self, device_name: &str) -> DeviceType {
        self.indi
            .get_devices_list()
            .iter()
            .find(|dev| dev.name.as_str() == device_name)
            .map(|dev| super::IndiHalImpl::driver_interface_to_dev_type(dev.interface))
            .unwrap_or_else(DeviceType::empty)
    }

    /// Sends the due bring-up actions and advances the camera recovery
    /// machines.
    ///
    /// Device bring-up errors are logged, camera errors are returned: a camera
    /// error may still abort the active mode through the HAL tick chain. All
    /// devices are ticked even if one fails, and only the first camera error
    /// reaches the user.
    /// `conn_state` is passed in to keep the lock order, see the caller.
    pub fn timer_tick(
        &mut self,
        timer_period_ms: usize,
        conn_state:      &indi::ConnState,
    ) -> eyre::Result<()> {
        // Not Connected: the server would reject commands with WrongSequence
        if *conn_state != indi::ConnState::Connected {
            return Ok(());
        }

        let indi = &self.indi;
        let events = &self.events;
        let mut errors: Vec<eyre::Report> = Vec::new();

        for dev in &mut self.devices {
            let result = dev.process(
                timer_period_ms,
                &mut |name, cmd| Self::run_command(indi, name, cmd),
                &mut |event| events.send(event),
            );
            if let Err(err) = result {
                errors.push(err);
            }
        }

        if errors.is_empty() {
            return Ok(());
        }
        for err in errors.iter().skip(1) {
            log::error!("Additional camera error from device watchdog: {:#}", err);
        }

        Err(errors.remove(0))
    }

    fn run_command(indi: &indi::Connection, name: &str, cmd: Command) -> eyre::Result<()> {
        match cmd {
            Command::Activate => {
                log::info!("Activating device \"{}\" ...", name);
                indi.command_enable_device(name, true, true, None)?;
            }
            Command::LoadOpts => {
                log::info!("Loading options for device \"{}\" ...", name);
                indi.command_set_switch_property(
                    name,
                    "CONFIG_PROCESS",
                    &[("CONFIG_LOAD", true)],
                )?;
            }
            Command::Disable => {
                indi.command_enable_device(name, false, true, None)?;
            }
            Command::SelectMaxRes => {
                if name.contains(" Simulator") {
                    return Ok(()); // don't do it for simulators
                }
                if indi.camera_is_resolution_supported(name).unwrap_or(false) {
                    log::info!("Setting maximum CCD resolution for camera {}", name);
                    indi.camera_select_max_resolution(name, true, None)?;
                }
            }
        }
        Ok(())
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

        fn command(&self) -> impl FnMut(&str, Command) -> eyre::Result<()> {
            |name, cmd| {
                self.sent.borrow_mut().push((name.to_string(), cmd));
                if self.fails.contains(&name) {
                    Err(eyre::eyre!("stub command for `{name}` failed"))
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
            name:      name.to_string(),
            type_,
            state,
            cam_init:  CameraInitFlags::default(),
            ccd_modes: CcdModes::inactive(),
        }
    }

    fn camera_entry(name: &str, state: DevState) -> WatchedDevice {
        let mut dev = entry(name, DeviceType::CAMERA, state);
        dev.ccd_modes = CcdModes {
            main:   CcdMode::Waiting,
            guider: CcdMode::Waiting,
        };
        dev
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

    fn exposure_change(
        device:     &str,
        ccd:        indi::CamCcd,
        prev_state: indi::PropState,
        new_state:  indi::PropState,
        value:      f64,
    ) -> indi::PropChangeEvent {
        let (prop_name, elem_name) = match ccd {
            indi::CamCcd::Main   => ("CCD_EXPOSURE", "CCD_EXPOSURE_VALUE"),
            indi::CamCcd::Guider => ("GUIDER_EXPOSURE", "GUIDER_EXPOSURE_VALUE"),
        };
        indi::PropChangeEvent {
            timestamp:   None,
            device_name: Arc::new(device.to_string()),
            change:      indi::PropChange::Change {
                prop_name: Arc::new(prop_name.to_string()),
                elem_name: Arc::new(elem_name.to_string()),
                value:     indi::PropValue::Num(indi::NumPropValue {
                    value,
                    min:    0.0,
                    max:    f64::MAX,
                    step:   None,
                    format: Arc::new("%g".to_string()),
                }),
                prev_state,
                new_state,
            },
        }
    }

    /// Identifies an init/restart event emitted by the watchdog
    fn event_kind_and_id(event: &HalEvent) -> (&'static str, String) {
        match event {
            HalEvent::CameraIsReadyForCooling(id) => ("cooling", id.to_string()),
            HalEvent::CameraIsReadyForCtrlFan(id) => ("fan", id.to_string()),
            HalEvent::CameraIsReadyForCtrlHeater(id) => ("heater", id.to_string()),
            HalEvent::CameraNeedInitTelescopeFocalLen(id) => ("focal_len", id.to_string()),
            HalEvent::CameraNeedRestartExposure(id) => ("restart", id.to_string()),
            _ => panic!("unexpected event"),
        }
    }

    fn new_watchdog() -> DevicesWatchdog {
        DevicesWatchdog::new(
            &Arc::new(indi::Connection::new()),
            &Arc::new(HalEventHandlers::new()),
        )
    }

    #[test]
    fn connect_is_sent_only_after_the_delay() {
        let stub  = Stub::new();
        let mut dev =
            entry("dev", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 });

        // 4 x 250 ms = 1000 ms, which is still not above the 1 s threshold
        for _ in 0..4 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);
        assert!(matches!(dev.state, DevState::WaitingForConnect { .. }));

        // 1250 ms passes the threshold
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
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
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);

        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        assert_eq!(stub.commands(), vec![("dev".to_string(), Command::LoadOpts)]);
        assert_eq!(dev.state, DevState::Idle);
    }

    #[test]
    fn failed_action_gets_no_retry_and_drops_to_idle() {
        let stub  = Stub::new().failing(&["dev"]);
        let mut dev =
            entry("dev", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 });

        for _ in 0..20 {
            _ = dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {});
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
                _ = dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {});
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
        let mut watchdog = new_watchdog();

        watchdog
            .notify_indi_prop_change(&conn_evt("dev", true, false))
            .unwrap();
        // If the guard broke, the command would fail with WrongSequence and drop
        // the entry to `Idle`
        for _ in 0..10 {
            watchdog.timer_tick(PERIOD_MS, &indi::ConnState::Disconnected).unwrap();
        }

        assert_eq!(
            watchdog.devices[0].state,
            DevState::WaitingForConnect { wait_ms: 0 }
        );
    }

    #[test]
    fn notify_queues_devices_by_event_kind() {
        let mut watchdog = new_watchdog();

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

    #[test]
    fn camera_load_opts_arms_the_camera_init() {
        let stub  = Stub::new();
        let mut dev = camera_entry("cam", DevState::WaitingForLoadOpts { wait_ms: 0 });

        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);

        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        assert_eq!(stub.commands(), vec![("cam".to_string(), Command::LoadOpts)]);
        assert_eq!(dev.state, DevState::WaitingCameraInit { wait_ms: 0 });
    }

    #[test]
    fn camera_init_sends_events_for_both_ccds() {
        let stub  = Stub::new();
        let mut dev = camera_entry("cam", DevState::WaitingCameraInit { wait_ms: 0 });
        dev.cam_init.cooler  = true;
        dev.cam_init.max_res = true;

        let mut events: Vec<HalEvent> = Vec::new();
        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        }
        assert!(events.is_empty());

        dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();

        assert_eq!(
            events.iter().map(event_kind_and_id).collect::<Vec<_>>(),
            vec![
                ("cooling", "cam".to_string()),
                ("cooling", "cam_CCD2".to_string()),
            ]
        );
        assert_eq!(stub.commands(), vec![("cam".to_string(), Command::SelectMaxRes)]);
        assert_eq!(dev.state, DevState::Idle);
        assert!(!dev.cam_init.cooler);
        assert!(!dev.cam_init.max_res);
    }

    #[test]
    fn late_camera_property_is_reported_after_next_load_opts() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);
        dev.cam_init.cooler = true;

        // A late feature does not start a wave on its own, only CONFIG_LOAD does
        dev.notify_new_indi_prop("CCD_RESOLUTION", "BINNING");
        assert!(dev.cam_init.max_res);
        assert_eq!(dev.state, DevState::Idle);
        for _ in 0..30 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);

        // The flags wait for the next CONFIG_LOAD round and are reported by it
        dev.state = DevState::WaitingForLoadOpts { wait_ms: 0 };
        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        assert_eq!(dev.state, DevState::WaitingCameraInit { wait_ms: 0 });

        let mut events: Vec<HalEvent> = Vec::new();
        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        }
        assert!(events.is_empty());
        dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        assert_eq!(
            events.iter().map(event_kind_and_id).collect::<Vec<_>>(),
            vec![
                ("cooling", "cam".to_string()),
                ("cooling", "cam_CCD2".to_string()),
            ]
        );
        assert_eq!(stub.commands(), vec![
            ("cam".to_string(), Command::LoadOpts),
            ("cam".to_string(), Command::SelectMaxRes),
        ]);
    }

    #[test]
    fn hung_blob_restarts_the_camera() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);

        // The exposure starts and its BLOB never arrives
        dev.notify_exposure_prop_changed(
            indi::CamCcd::Main,
            indi::PropState::Idle,
            indi::PropState::Busy,
            0.0,
        );
        assert!(matches!(dev.ccd_modes.main, CcdMode::WaitBlob(0)));

        for _ in 0..119 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap(); // 30 s passed
        assert_eq!(stub.commands(), vec![("cam".to_string(), Command::Disable)]);
        assert!(matches!(dev.ccd_modes.main, CcdMode::Shutdown(_)));

        for _ in 0..7 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap(); // 2 s passed
        assert_eq!(stub.commands(), vec![
            ("cam".to_string(), Command::Disable),
            ("cam".to_string(), Command::Activate),
        ]);
        assert!(matches!(dev.ccd_modes.main, CcdMode::WaitExposureProp(_)));

        // The driver announces the exposure property again after the restart
        dev.notify_exposure_new_prop(indi::CamCcd::Main);
        assert!(matches!(dev.ccd_modes.main, CcdMode::WaitAfterRestart(0)));

        let mut events: Vec<HalEvent> = Vec::new();
        for _ in 0..19 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(event_kind_and_id(&events[0]), ("restart", "cam".to_string()));
        assert_eq!(dev.ccd_modes.main, CcdMode::Waiting);
    }

    #[test]
    fn blob_arrival_cancels_the_wait() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);

        dev.notify_exposure_prop_changed(
            indi::CamCcd::Main,
            indi::PropState::Idle,
            indi::PropState::Busy,
            0.0,
        );
        assert!(matches!(dev.ccd_modes.main, CcdMode::WaitBlob(0)));

        // The BLOB arrives: the property becomes Ok
        dev.notify_exposure_prop_changed(
            indi::CamCcd::Main,
            indi::PropState::Busy,
            indi::PropState::Ok,
            0.0,
        );
        assert_eq!(dev.ccd_modes.main, CcdMode::Waiting);

        for _ in 0..130 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);
    }

    #[test]
    fn second_ccd_joins_the_ongoing_restart() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);

        // Both CCDs hang at the same time
        dev.notify_exposure_prop_changed(
            indi::CamCcd::Main,
            indi::PropState::Idle,
            indi::PropState::Busy,
            0.0,
        );
        dev.notify_exposure_prop_changed(
            indi::CamCcd::Guider,
            indi::PropState::Idle,
            indi::PropState::Busy,
            0.0,
        );

        for _ in 0..119 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();

        // Only one power cycle for the whole camera
        assert_eq!(stub.commands(), vec![("cam".to_string(), Command::Disable)]);
        assert!(matches!(dev.ccd_modes.main, CcdMode::Shutdown(_)));
        assert!(matches!(dev.ccd_modes.guider, CcdMode::WaitExposureProp(0)));

        for _ in 0..7 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        assert_eq!(stub.count(), 2); // Disable + Activate, no second cycle
        assert!(matches!(dev.ccd_modes.main, CcdMode::WaitExposureProp(_)));
        assert!(matches!(dev.ccd_modes.guider, CcdMode::WaitExposureProp(_)));

        dev.notify_exposure_new_prop(indi::CamCcd::Main);
        dev.notify_exposure_new_prop(indi::CamCcd::Guider);

        let mut events: Vec<HalEvent> = Vec::new();
        for _ in 0..19 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        let got: Vec<(&str, String)> = events.iter().map(event_kind_and_id).collect();
        assert_eq!(got, vec![
            ("restart", "cam".to_string()),
            ("restart", "cam_CCD2".to_string()),
        ]);
    }

    #[test]
    fn unreachable_camera_is_abandoned_and_rearms_on_connect() {
        let stub  = Stub::new().failing(&["cam"]);
        let mut dev = camera_entry("cam", DevState::Idle);
        dev.ccd_modes.main = CcdMode::WaitBlob(MAX_WAIT_BLOB_TIME * 1000);

        let err = dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("Cannot shutdown INDI camera cam"));
        assert_eq!(dev.ccd_modes.main, CcdMode::Inactive);
        assert_eq!(dev.ccd_modes.guider, CcdMode::Waiting);

        // Coverage returns when the device connects again
        dev.arm_ccd_modes();
        assert_eq!(dev.ccd_modes.main, CcdMode::Waiting);
    }

    #[test]
    fn arm_ccd_modes_waits_and_keeps_recovery() {
        let mut dev = entry("cam", DeviceType::CAMERA, DevState::Idle);
        dev.ccd_modes = CcdModes {
            main:   CcdMode::Inactive,
            guider: CcdMode::WaitExposureProp(0),
        };

        dev.arm_ccd_modes();

        assert_eq!(dev.ccd_modes.main, CcdMode::Waiting);
        assert_eq!(dev.ccd_modes.guider, CcdMode::WaitExposureProp(0));

        // Non cameras are never watched
        let mut tel = entry("tel", DeviceType::TELESCOPE, DevState::Idle);
        tel.arm_ccd_modes();
        assert_eq!(tel.ccd_modes.main, CcdMode::Inactive);
        assert_eq!(tel.ccd_modes.guider, CcdMode::Inactive);
    }

    #[test]
    fn deleted_device_keeps_only_idle_ccd_modes_disarmed() {
        let mut watchdog = new_watchdog();
        watchdog.devices.push(camera_entry("cam", DevState::Idle));
        watchdog.devices[0].ccd_modes.main = CcdMode::Shutdown(0);

        watchdog.notify_device_deleted("cam");

        // The restart in progress must survive the device removal
        assert_eq!(watchdog.devices[0].ccd_modes.main, CcdMode::Shutdown(0));
        assert_eq!(watchdog.devices[0].ccd_modes.guider, CcdMode::Inactive);
    }

    #[test]
    fn exposure_events_drive_the_recovery_machine_via_notify() {
        let mut watchdog = new_watchdog();
        watchdog.devices.push(camera_entry("cam", DevState::Idle));

        watchdog
            .notify_indi_prop_change(&exposure_change(
                "cam",
                indi::CamCcd::Guider,
                indi::PropState::Idle,
                indi::PropState::Busy,
                0.0,
            ))
            .unwrap();

        assert!(matches!(watchdog.devices[0].ccd_modes.guider, CcdMode::WaitBlob(0)));
        assert_eq!(watchdog.devices[0].ccd_modes.main, CcdMode::Waiting);
    }

    #[test]
    fn connection_event_does_not_disturb_recovery_or_pending_flags() {
        let mut watchdog = new_watchdog();
        watchdog
            .devices
            .push(camera_entry("cam", DevState::WaitingCameraInit { wait_ms: 0 }));
        watchdog.devices[0].cam_init.cooler = true;
        watchdog.devices[0].ccd_modes.main = CcdMode::Shutdown(500);

        // The recovery's own power cycle announces `CONNECTION=true`
        watchdog.notify_indi_prop_change(&conn_evt("cam", false, true)).unwrap();

        let dev = &watchdog.devices[0];
        // The restart in progress and the idle CCD are left alone
        assert!(matches!(dev.ccd_modes.main, CcdMode::Shutdown(500)));
        assert_eq!(dev.ccd_modes.guider, CcdMode::Waiting);
        // The bring-up machine re-queues, but the collected init flags survive
        assert_eq!(dev.state, DevState::WaitingForLoadOpts { wait_ms: 0 });
        assert!(dev.cam_init.cooler);
        // A driver unknown to the bus must not downgrade the known CAMERA type
        assert!(dev.type_.contains(DeviceType::CAMERA));
    }

    #[test]
    fn device_announced_before_connection_is_ignored() {
        let mut watchdog = new_watchdog();
        // `DRIVER_INFO` first: `notify_device_announced` fires but there is no
        // entry yet (the `CONNECTION` event creates it later, with the type
        // already resolvable), so this must be a harmless no-op
        watchdog.notify_device_announced("ghost", DeviceType::CAMERA);
        assert!(watchdog.devices.is_empty());
    }

    #[test]
    fn failed_switch_on_abandons_the_camera() {
        let mut sent: Vec<(String, Command)> = Vec::new();
        let mut dev = camera_entry("cam", DevState::Idle);
        dev.ccd_modes.main = CcdMode::Shutdown(0);

        let err = {
            // Only the switch-on step fails: the camera stays unreachable
            let mut command = |name: &str, cmd: Command| -> eyre::Result<()> {
                sent.push((name.to_string(), cmd));
                match cmd {
                    Command::Activate => Err(eyre::eyre!("stub: unreachable")),
                    _ => Ok(()),
                }
            };
            let mut result = Ok(());
            for _ in 0..8 {
                result = dev.process(PERIOD_MS, &mut command, &mut |_| {});
                if result.is_err() {
                    break;
                }
            }
            result.unwrap_err()
        };

        assert!(err.to_string().contains("Cannot switch on INDI camera cam"));
        assert_eq!(dev.ccd_modes.main, CcdMode::Inactive);
        assert_eq!(sent, vec![("cam".to_string(), Command::Activate)]);
    }

    #[test]
    fn non_zero_exposure_value_cancels_the_blob_wait() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);

        dev.notify_exposure_prop_changed(
            indi::CamCcd::Main,
            indi::PropState::Idle,
            indi::PropState::Busy,
            0.0,
        );
        assert!(matches!(dev.ccd_modes.main, CcdMode::WaitBlob(0)));

        // A non-zero exposure value means the driver accepted a real exposure
        dev.notify_exposure_prop_changed(
            indi::CamCcd::Main,
            indi::PropState::Busy,
            indi::PropState::Busy,
            2.5,
        );
        assert_eq!(dev.ccd_modes.main, CcdMode::Waiting);

        for _ in 0..130 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        assert_eq!(stub.count(), 0);
    }

    #[test]
    fn timer_tick_reports_first_camera_error_and_hides_bring_up_errors() {
        let mut watchdog = new_watchdog();
        // Two cameras whose restart property wait times out: both produce a
        // recovery error in the same tick
        for name in ["cam1", "cam2"] {
            let mut dev = camera_entry(name, DevState::Idle);
            dev.ccd_modes.main = CcdMode::WaitExposureProp(WAIT_EXPOSURE_TIME * 1000);
            watchdog.devices.push(dev);
        }

        let err = watchdog
            .timer_tick(PERIOD_MS, &indi::ConnState::Connected)
            .unwrap_err();
        assert!(err.to_string().contains("Waiting camera restart too long"));
        // Both were ticked despite the first error
        assert_eq!(watchdog.devices[0].ccd_modes.main, CcdMode::Waiting);
        assert_eq!(watchdog.devices[1].ccd_modes.main, CcdMode::Waiting);

        // A bring-up command failure on a real (disconnected) connection is
        // logged only, never returned
        let mut watchdog = new_watchdog();
        watchdog
            .devices
            .push(entry("dev", DeviceType::TELESCOPE, DevState::WaitingForConnect { wait_ms: 0 }));
        for _ in 0..10 {
            watchdog.timer_tick(PERIOD_MS, &indi::ConnState::Connected).unwrap();
        }
    }

    #[test]
    fn device_announced_refreshes_type_and_arms_ccds() {
        let mut watchdog = new_watchdog();
        // The `CONNECTION` event arrived before `DRIVER_INFO`: the entry was
        // created with an unresolvable type, nothing was armed
        watchdog.notify_indi_prop_change(&conn_evt("cam", true, true)).unwrap();
        assert!(!watchdog.devices[0].type_.contains(DeviceType::CAMERA));
        assert_eq!(watchdog.devices[0].ccd_modes.main, CcdMode::Inactive);

        // Then `DRIVER_INFO` arrives with the interface from the event
        watchdog.notify_device_announced("cam", DeviceType::CAMERA);

        assert!(watchdog.devices[0].type_.contains(DeviceType::CAMERA));
        assert_eq!(watchdog.devices[0].ccd_modes.main, CcdMode::Waiting);
        assert_eq!(watchdog.devices[0].ccd_modes.guider, CcdMode::Waiting);

        // A repeated announcement must not disturb a restart in progress
        watchdog.devices[0].ccd_modes.main = CcdMode::Shutdown(0);
        watchdog.notify_device_announced("cam", DeviceType::CAMERA);
        assert!(matches!(watchdog.devices[0].ccd_modes.main, CcdMode::Shutdown(0)));
    }

    #[test]
    fn reset_device_states_keeps_camera_recovery() {
        let mut watchdog = new_watchdog();
        watchdog
            .devices
            .push(camera_entry("cam", DevState::WaitingForLoadOpts { wait_ms: 0 }));
        watchdog.devices[0].ccd_modes.main = CcdMode::WaitBlob(1000);

        watchdog.reset_device_states();

        assert_eq!(watchdog.devices[0].state, DevState::Idle);
        assert_eq!(watchdog.devices[0].ccd_modes.main, CcdMode::WaitBlob(1000));

        watchdog.reset_all();
        assert!(watchdog.devices.is_empty());
    }

    #[test]
    fn ccd_joins_restart_after_the_exposure_property_appeared() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);
        dev.ccd_modes.main   = CcdMode::WaitAfterRestart(0);
        dev.ccd_modes.guider = CcdMode::WaitBlob(MAX_WAIT_BLOB_TIME * 1000 - PERIOD_MS);

        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();

        // Joined the late restart phase without a second power cycle
        assert_eq!(stub.count(), 0);
        assert_eq!(dev.ccd_modes.guider, CcdMode::WaitAfterRestart(0));
    }

    #[test]
    fn restart_property_timeout_is_reported() {
        let stub = Stub::new();
        let mut dev = camera_entry("cam", DevState::Idle);
        dev.ccd_modes.main = CcdMode::WaitExposureProp(WAIT_EXPOSURE_TIME * 1000);

        let err = dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap_err();

        assert!(err.to_string().contains("Waiting camera restart too long"));
        assert_eq!(dev.ccd_modes.main, CcdMode::Waiting);
        assert_eq!(stub.count(), 0);
    }

    #[test]
    fn failed_load_opts_still_inits_the_camera() {
        let stub  = Stub::new().failing(&["cam"]);
        let mut dev = camera_entry("cam", DevState::WaitingForLoadOpts { wait_ms: 0 });
        dev.cam_init.focal_len = true;

        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        }
        // The load fails, but the already enabled camera is due for its wave
        dev.process(PERIOD_MS, &mut stub.command(), &mut |_| {}).unwrap();
        assert_eq!(dev.state, DevState::WaitingCameraInit { wait_ms: 0 });

        let mut events: Vec<HalEvent> = Vec::new();
        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        }
        assert!(events.is_empty());
        dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        assert_eq!(
            events.iter().map(event_kind_and_id).collect::<Vec<_>>(),
            vec![("focal_len", "cam".to_string())]
        );
    }

    #[test]
    fn camera_init_fires_events_per_flag() {
        let stub  = Stub::new();
        let mut dev = camera_entry("cam", DevState::WaitingCameraInit { wait_ms: 0 });
        dev.cam_init.fan       = true;
        dev.cam_init.heater    = true;
        dev.cam_init.focal_len = true;

        let mut events: Vec<HalEvent> = Vec::new();
        for _ in 0..8 {
            dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();
        }
        assert!(events.is_empty());
        dev.process(PERIOD_MS, &mut stub.command(), &mut |e| events.push(e)).unwrap();

        assert_eq!(
            events.iter().map(event_kind_and_id).collect::<Vec<_>>(),
            vec![
                ("fan", "cam".to_string()),
                ("fan", "cam_CCD2".to_string()),
                ("heater", "cam".to_string()),
                ("heater", "cam_CCD2".to_string()),
                ("focal_len", "cam".to_string()),
            ]
        );
        // No cooler/resolution flags: no commands were sent
        assert_eq!(stub.count(), 0);
    }
}
