use std::sync::Arc;

use crate::hal::events::{HalEvent, HalEventHandlers};

const MAX_WAIT_BLOB_TIME: usize = 30; // in seconds
const MAX_SHUTDOWN_TIME: usize = 2; // in seconds
const WAIT_EXPOSURE_TIME: usize = 10; // in seconds
const MAX_SWITCHING_ON_TIME: usize = 5; // in seconds after exposure property appeared
const INIT_DELAY: usize = 2; // in seconds

#[derive(Debug, PartialEq)]
enum CcdMode {
    Waiting,
    WaitBlob(usize),
    Shutdown(usize),
    WaitExposureProp(usize),
    WaitAfterRestart(usize),
    /// Device command failed, the camera is considered lost.
    /// `CamWatchdog::timer_tick` drops such entries in the same tick.
    MustBeDeleted,
}

struct CcdToWatch {
    device_id:   Arc<String>,
    name:        Arc<String>,
    ccd:         indi::CamCcd,
    mode:        CcdMode,
}

impl CcdToWatch {
    fn notify_exposure_prop_changed(
        &mut self,
        prev_state: indi::PropState,
        new_state:  indi::PropState,
        value:      f64
    ) -> eyre::Result<()> {
        let is_ready =
            prev_state == indi::PropState::Busy &&
            new_state == indi::PropState::Ok;
        let is_zero = value == 0.0;
        let is_waiting_for_blob = is_zero && new_state == indi::PropState::Busy;
        if is_waiting_for_blob && matches!(self.mode, CcdMode::Waiting) {
            self.mode = CcdMode::WaitBlob(0);
        }
        if (is_ready || !is_zero) && matches!(self.mode, CcdMode::WaitBlob(_)) {
            self.mode = CcdMode::Waiting;
        }
        Ok(())
    }

    fn notify_exposure_new_prop(&mut self) -> eyre::Result<()> {
        if matches!(self.mode, CcdMode::WaitExposureProp(_)) {
            self.mode = CcdMode::WaitAfterRestart(0);
        }
        Ok(())
    }

    /// Advances the recovery machine. A failed device command marks the entry as
    /// `CcdMode::MustBeDeleted` and returns an error meant for the user.
    fn timer_tick(
        &mut self,
        timer_period_ms: usize,
        indi:            &Arc<indi::Connection>,
        events:          &Arc<HalEventHandlers>,
    ) -> eyre::Result<()> {
        match &mut self.mode {
            CcdMode::Waiting => {}
            CcdMode::WaitBlob(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms >= MAX_WAIT_BLOB_TIME * 1000 {
                    log::error!(
                        "Waiting for BLOB of INDI camera {} too long (> {}) seconds",
                        self.name, MAX_WAIT_BLOB_TIME
                    );
                    log::info!("Shutdown INDI camera {} ...", self.name);
                    if let Err(err) = indi.command_enable_device(&self.name, false, true, None) {
                        return Err(self.abandon("shutdown", err));
                    }
                    self.mode = CcdMode::Shutdown(0)
                }
            }
            CcdMode::Shutdown(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms >= MAX_SHUTDOWN_TIME * 1000 {
                    log::info!("Switching on camera {} ...", self.name);
                    // On failure the device stays powered off: switching it back on is
                    // up to `DevicesWatchdog`, which re-queues it on the next `CONNECTION`
                    if let Err(err) = indi.command_enable_device(&self.name, true, true, None) {
                        return Err(self.abandon("switch on", err));
                    }
                    self.mode = CcdMode::WaitExposureProp(0);
                }
            }

            CcdMode::WaitExposureProp(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms >= WAIT_EXPOSURE_TIME * 1000 {
                    self.mode = CcdMode::Waiting;
                    eyre::bail!("Waiting camera restart too long (>{}s)!", WAIT_EXPOSURE_TIME);
                }
            }

            CcdMode::WaitAfterRestart(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms >= MAX_SWITCHING_ON_TIME * 1000 {
                    self.mode = CcdMode::Waiting;
                    events.send(HalEvent::CameraNeedRestartExposure(Arc::clone(&self.device_id)));
                }
            }

            // Inert: purged by the caller in the same tick
            CcdMode::MustBeDeleted => {}
        }
        Ok(())
    }

    /// Gives up on an unreachable camera: marks the entry for removal and builds
    /// the error reported to the user. No retry: a device that cannot be commanded
    /// at all means a hardware fault and needs the user. Coverage returns when the
    /// device announces itself again (`notify_device_added`).
    fn abandon(&mut self, action: &str, err: indi::Error) -> eyre::Report {
        self.mode = CcdMode::MustBeDeleted;
        eyre::eyre!(
            "Cannot {action} INDI camera {name}, it is unreachable. \
             Camera recovery abandoned! Reason: {err}",
            name = self.name,
        )
    }

    /// Only an idle CCD may be dropped. Deleting an entry mid-recovery would
    /// abandon it: in `CcdMode::Shutdown` the power-off was already sent and
    /// only this entry's own tick switches the camera back on, so the exposure
    /// would never restart. Ignoring a CCD removal during recovery leaks the
    /// entry until `reset()` - deliberately simple, not strictly correct.
    fn can_be_deleted(&self) -> bool {
        self.mode == CcdMode::Waiting
    }
}

#[derive(Default)]
struct CameraInitFlags {
    cooler:    bool,
    fan:       bool,
    heater:    bool,
    max_res:   bool,
    focal_len: bool,
}

struct CameraToInit {
    device_id1: Arc<String>, // for CCD1
    device_id2: Arc<String>, // for CCD2
    name:       Arc<String>, // INDI device name
    init_flags: CameraInitFlags,
    init_timer: Option<usize>,
}

impl CameraToInit {
    fn notify_new_indi_prop(&mut self, prop_name: &str, elem_name: &str) -> eyre::Result<()> {
        let is_temperature_property =
            indi::Connection::camera_is_temperature_property(prop_name, elem_name);
        let is_fan_str_property =
            indi::Connection::camera_is_fan_str_property(prop_name);
        let is_heater_str_property =
            indi::Connection::camera_is_heater_str_property(prop_name);
        let is_resolution_property = prop_name == "CCD_RESOLUTION";
        let is_focal_len_property = prop_name == "SCOPE_INFO" && elem_name == "FOCAL_LENGTH";

        if is_temperature_property {
            self.init_flags.cooler = true;
            self.init_timer = Some(0);
        }

        if is_fan_str_property {
            self.init_flags.fan = true;
            self.init_timer = Some(0);
        }

        if is_heater_str_property {
            self.init_flags.heater = true;
            self.init_timer = Some(0);
        }

        if is_resolution_property {
            self.init_flags.max_res = true;
            self.init_timer = Some(0);
        }

        if is_focal_len_property {
            self.init_flags.focal_len = true;
            self.init_timer = Some(0);
        }

        Ok(())
    }

    fn timer_tick(
        &mut self,
        timer_period_ms: usize,
        indi:            &Arc<indi::Connection>,
        events:          &Arc<HalEventHandlers>,
    ) -> eyre::Result<()> {
        if let Some(init_timer_ms) = &mut self.init_timer {
            *init_timer_ms += timer_period_ms;
            if *init_timer_ms >= INIT_DELAY * 1000 {
                self.init_timer = None;
                if self.init_flags.cooler {
                    self.init_flags.cooler = false;
                    events.send(HalEvent::CameraIsReadyForCooling(Arc::clone(&self.device_id1)));
                    events.send(HalEvent::CameraIsReadyForCooling(Arc::clone(&self.device_id2)));
                }
                if self.init_flags.fan {
                    self.init_flags.fan = false;
                    events.send(HalEvent::CameraIsReadyForCtrlFan(Arc::clone(&self.device_id1)));
                    events.send(HalEvent::CameraIsReadyForCtrlFan(Arc::clone(&self.device_id2)));
                }
                if self.init_flags.heater {
                    self.init_flags.heater = false;
                    events.send(HalEvent::CameraIsReadyForCtrlHeater(Arc::clone(&self.device_id1)));
                    events.send(HalEvent::CameraIsReadyForCtrlHeater(Arc::clone(&self.device_id2)));
                }
                if self.init_flags.max_res {
                    self.init_flags.max_res = false;
                    self.select_maximum_resolution(indi)?;
                }
                if self.init_flags.focal_len {
                    self.init_flags.focal_len = false;
                    events.send(HalEvent::CameraNeedInitTelescopeFocalLen(Arc::clone(&self.name)));
                }
            }
        }

        Ok(())
    }

    fn select_maximum_resolution(&self, indi: &Arc<indi::Connection>) -> eyre::Result<()> {
        if self.name.contains(" Simulator") { // don't do it for simulators
            return Ok(());
        }

        if indi.camera_is_resolution_supported(&self.name).unwrap_or(false) {
            log::info!("Setting maximum CCD resolution for camera {}", self.name);
            indi.camera_select_max_resolution(&self.name, true, None)?;
        }
        Ok(())
    }
}

pub struct CamWatchdog {
    indi:      Arc<indi::Connection>,
    events:    Arc<HalEventHandlers>,
    ccd_list:  Vec<CcdToWatch>,
    init_list: Vec<CameraToInit>,
}

impl CamWatchdog {
    pub fn new(indi: &Arc<indi::Connection>, events: &Arc<HalEventHandlers>) -> Self {
        Self {
            indi:      Arc::clone(indi),
            events:    Arc::clone(events),
            init_list: Vec::new(),
            ccd_list:  Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.init_list.clear();
        self.ccd_list.clear();
    }

    /// `conn_state` is passed in (not read from `self.indi`) to keep the
    /// state -> watchdogs lock order convention, see the HAL tick caller
    ///
    /// All entries are ticked even if one fails. Doomed CCDs are dropped before
    /// returning, so a failure is reported once per camera instead of once per
    /// tick. Only the first error is returned, the rest are logged.
    pub fn timer_tick(
        &mut self,
        timer_period_ms: usize,
        conn_state:      &indi::ConnState,
    ) -> eyre::Result<()> {
        if *conn_state != indi::ConnState::Connected {
            return Ok(());
        }

        // CCD errors are collected first: they are the severe ones and only the
        // first error reaches the user
        let mut errors: Vec<eyre::Report> = Vec::new();

        for ccd in &mut self.ccd_list {
            if let Err(err) = ccd.timer_tick(timer_period_ms, &self.indi, &self.events) {
                errors.push(err);
            }
        }

        for camera in &mut self.init_list {
            if let Err(err) = camera.timer_tick(timer_period_ms, &self.indi, &self.events) {
                errors.push(err);
            }
        }

        self.ccd_list.retain(|ccd| {
            let doomed = matches!(ccd.mode, CcdMode::MustBeDeleted);
            if doomed {
                log::warn!(
                    "Camera watchdog gives up on INDI camera {} (CCD {:?}), \
                     no recovery until the device reconnects",
                    ccd.name, ccd.ccd
                );
            }
            !doomed
        });

        if errors.is_empty() {
            return Ok(());
        }
        for err in errors.iter().skip(1) {
            log::error!("Additional camera watchdog error: {:#}", err);
        }

        Err(errors.remove(0))
    }

    pub fn notify_device_added(&mut self, device_name: &Arc<String>) {
        self.add_ccd(device_name, indi::CamCcd::Main);
        self.add_ccd(device_name, indi::CamCcd::Guider);
        self.add_camera(device_name);
    }

    fn add_ccd(&mut self, device_name: &Arc<String>, ccd: indi::CamCcd) {
        let existing = self.ccd_list
            .iter()
            .find(|item| item.ccd == ccd && *item.name == **device_name);
        if existing.is_some() {
            return;
        }
        let device_id = if ccd == indi::CamCcd::Guider {
            super::ccd2_id(device_name)
        } else {
            device_name.to_string()
        };

        self.ccd_list.push(CcdToWatch {
            device_id: Arc::new(device_id),
            name:      Arc::clone(device_name),
            mode:      CcdMode::Waiting,
            ccd,
        });
    }

    fn add_camera(&mut self, device_name: &Arc<String>) {
        let existing = self.init_list.iter().find(|item| *item.name == **device_name);
        if existing.is_some() {
            return;
        }
        self.init_list.push(CameraToInit {
            device_id1:  Arc::clone(device_name),
            device_id2:  Arc::new(super::ccd2_id(device_name)),
            name:        Arc::clone(device_name),
            init_flags:  CameraInitFlags::default(),
            init_timer:  None,
        });
    }

    pub fn notify_device_deleted(&mut self, device_name: &Arc<String>) {
        self.delete_camera(device_name);
        self.delete_ccd(device_name, indi::CamCcd::Main);
        self.delete_ccd(device_name, indi::CamCcd::Guider);
    }

    fn delete_ccd(&mut self, device_name: &Arc<String>, ccd: indi::CamCcd) {
        let existing_pos = self.ccd_list
            .iter()
            .position(|item|
                item.can_be_deleted() &&
                item.ccd == ccd && *item.name == **device_name
            );
        let Some(existing_pos) = existing_pos else {
            return;
        };
        self.ccd_list.remove(existing_pos);
    }

    fn delete_camera(&mut self, device_name: &Arc<String>) {
        let existing_pos = self.init_list
            .iter()
            .position(|item| *item.name == **device_name);
        let Some(existing_pos) = existing_pos else {
            return;
        };
        self.init_list.remove(existing_pos);
    }

    fn notify_indi_prop_change_for_ccd(&mut self, prop_change: &indi::PropChangeEvent) -> eyre::Result<()> {
        if let indi::PropChange::Change { prop_name, elem_name, prev_state, new_state, value } = &prop_change.change {
            let cam_ccd = indi::Connection::camera_get_ccd_for_property(prop_name, elem_name);
            if let Some(cam_ccd) = cam_ccd {
                let item = self.ccd_list
                    .iter_mut()
                    .find(|ccd| ccd.ccd == cam_ccd && *ccd.name == *prop_change.device_name);
                if let Some(item) = item {
                    item.notify_exposure_prop_changed(*prev_state, *new_state, value.to_f64().unwrap_or(f64::NAN))?;
                }
            }
        }

        if let indi::PropChange::New { prop_name, elem_name, .. } = &prop_change.change {
            let cam_ccd = indi::Connection::camera_get_ccd_for_property(prop_name, elem_name);
            if let Some(cam_ccd) = cam_ccd {
                let item = self.ccd_list
                    .iter_mut()
                    .find(|ccd| ccd.ccd == cam_ccd && *ccd.name == *prop_change.device_name);
                if let Some(item) = item {
                    item.notify_exposure_new_prop()?;
                }
            }
        }

        Ok(())
    }

    fn notify_indi_prop_change_for_camera(&mut self, prop_change: &indi::PropChangeEvent) -> eyre::Result<()> {
        if let indi::PropChange::New { prop_name, elem_name, .. } = &prop_change.change {
            let item = self.init_list.iter_mut().find(|item| *item.name == *prop_change.device_name);

            if let Some(item) = item {
                item.notify_new_indi_prop(prop_name, elem_name)?;
            }
        }
        Ok(())
    }

    pub fn notify_indi_prop_change(&mut self, prop_change: &indi::PropChangeEvent) -> eyre::Result<()> {
        self.notify_indi_prop_change_for_ccd(prop_change)?;
        self.notify_indi_prop_change_for_camera(prop_change)?;

        Ok(())
    }
}
