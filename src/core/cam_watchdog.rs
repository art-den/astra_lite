use std::sync::Arc;

use crate::{core::consts::*, indi, options::{DeviceAndProp, Options}};

const MAX_WAIT_BLOB_TIME: usize = 30; // in seconds
const MAX_SHUTDOWN_TIME: usize = 2; // in seconds
const WAIT_EXPOSURE_TIME: usize = 10; // in seconds
const MAX_SWITCHING_ON_TIME: usize = 5; // in seconds after exposure property apeared
const INIT_DELAY: usize = 2; // is seconds

#[derive(Debug)]
enum Mode {
    Waiting,
    WaitBlob(usize),
    Shutdown(usize),
    WaitExposureProp(usize),
    WaitAfterRestart(usize),
}

#[derive(Default)]
struct InitFlags {
    cooler: bool,
    fan: bool,
    heater: bool,
    max_res: bool,
}

pub enum CamWatchdogResult {
    Ok,
    Waiting,
    RestartCameraShot,
}

pub struct CameraWatchdog {
    mode:       Mode,
    init_flags: InitFlags,
    init_timer: usize,
}

impl CameraWatchdog {
    pub fn new() -> Self {
        Self {
            mode:       Mode::Waiting,
            init_flags: InitFlags::default(),
            init_timer: 0,
        }
    }

    pub fn clear(&mut self) {
        self.mode = Mode::Waiting;
        self.init_flags = InitFlags::default();
        self.init_timer = 0;
    }

    pub fn notify_timer_1s(
        &mut self,
        indi:              &Arc<indi::Connection>,
        options:           &Options,
        cam_device:        &DeviceAndProp,
        debug_blob_frozen: bool,
    ) -> anyhow::Result<CamWatchdogResult> {
        if indi.state() != indi::ConnState::Connected {
            return Ok(CamWatchdogResult::Ok);
        }

        let is_waiting_for_blob_now = || -> anyhow::Result<bool> {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_device.prop);
            let Ok((exp_prop, exp_prop_elem)) = indi.camera_get_exposure_property(&cam_device.name, cam_ccd) else {
                return Ok(false);
            };
            let indi::PropValue::Num(expr_prop_num) = &exp_prop_elem.value else {
                return Ok(false)
            };
            Ok(
                (debug_blob_frozen || exp_prop.state == indi::PropState::Busy) &&
                expr_prop_num.value/*exposure*/ == 0.0
            )
        };

        match &mut self.mode {
            Mode::Waiting => {
                if is_waiting_for_blob_now()? {
                    self.mode = Mode::WaitBlob(0)
                }
            }
            Mode::WaitBlob(cnt) => {
                if !is_waiting_for_blob_now()? {
                    self.mode = Mode::Waiting;
                    return Ok(CamWatchdogResult::Ok);
                }
                *cnt += 1;
                if *cnt == MAX_WAIT_BLOB_TIME * CORE_TIMER_FREQ {
                    log::info!("Waiting BLOB of camera {} too logn time (> {}) seconds", cam_device.name, MAX_WAIT_BLOB_TIME);
                    log::info!("Shutdown camera {} ...", cam_device.name);
                    indi.command_enable_device(&cam_device.name, false, true, None)?;
                    self.mode = Mode::Shutdown(0)
                }
            }
            Mode::Shutdown(cnt) => {
                *cnt += 1;
                if *cnt == MAX_SHUTDOWN_TIME * CORE_TIMER_FREQ {
                    log::info!("Switching-on camera {} ...", cam_device.name);
                    indi.command_enable_device(&cam_device.name, true, true, None)?;
                    self.mode = Mode::WaitExposureProp(0);
                }
            }

            Mode::WaitExposureProp(cnt) => {
                *cnt += 1;
                if *cnt >= WAIT_EXPOSURE_TIME * CORE_TIMER_FREQ {
                    anyhow::bail!("Waiting exposure property too long time!");
                }
            }

            Mode::WaitAfterRestart(cnt) => {
                *cnt += 1;
                if *cnt == MAX_SWITCHING_ON_TIME * CORE_TIMER_FREQ {
                    self.mode = Mode::Waiting;
                    return Ok(CamWatchdogResult::RestartCameraShot);
                }
            }
        }

        if self.init_timer != 0 {
            self.init_timer -= 1;
            if self.init_timer == 0 {
                if self.init_flags.cooler {
                    self.init_flags.cooler = false;
                    self.control_camera_cooling(indi, &cam_device.name, options, true)?;
                }
                if self.init_flags.fan {
                    self.init_flags.fan = false;
                    self.control_camera_fan(indi, &cam_device.name, options, true)?;
                }
                if self.init_flags.heater {
                    self.init_flags.heater = false;
                    self.control_camera_heater(indi, &cam_device.name, options, true)?;
                }
                if self.init_flags.max_res {
                    self.init_flags.max_res = false;
                    self.select_maximum_resolution(indi, &cam_device.name)?;
                }
            }
        }

        match self.mode {
            Mode::WaitAfterRestart(_) | Mode::Shutdown(_) =>
                Ok(CamWatchdogResult::Waiting),
            _ =>
                Ok(CamWatchdogResult::Ok),
        }
    }

    pub fn notify_indi_prop_change(
        &mut self,
        mode_cam_device: &DeviceAndProp,
        prop_change: &indi::PropChangeEvent
    ) -> anyhow::Result<()> {
        if mode_cam_device.name != *prop_change.device_name {
            return Ok(());
        }

        if let indi::PropChange::Change { value, prev_state, new_state } = &prop_change.change {
            let is_exposure_property =
                indi::Connection::camera_is_exposure_property(
                    &prop_change.prop_name,
                    &value.elem_name,
                    indi::CamCcd::from_ccd_prop_name(&mode_cam_device.prop)
                );

            let is_ready =
                *prev_state == indi::PropState::Busy &&
                *new_state == indi::PropState::Ok;

            let mode_is_waiting_blob = matches!(self.mode, Mode::WaitBlob(_));

            if is_ready && is_exposure_property && mode_is_waiting_blob {
                // switch mode from WaitBlob to Waiting
                self.mode = Mode::Waiting;
            }
        }

        if let indi::PropChange::New(new_prop) = &prop_change.change {
            let is_temperature_property =
                indi::Connection::camera_is_temperature_property(&prop_change.prop_name, &new_prop.elem_name);
            if is_temperature_property {
                self.init_flags.cooler = true;
                self.init_timer = INIT_DELAY * CORE_TIMER_FREQ;
            }

            let is_fan_str_property =
                indi::Connection::camera_is_fan_str_property(&prop_change.prop_name);
            if is_fan_str_property {
                self.init_flags.fan = true;
                self.init_timer = INIT_DELAY * CORE_TIMER_FREQ;
            }

            let is_heater_str_property =
                indi::Connection::camera_is_heater_str_property(&prop_change.prop_name);
            if is_heater_str_property {
                self.init_flags.heater = true;
                self.init_timer = INIT_DELAY * CORE_TIMER_FREQ;
            }

            if prop_change.prop_name.as_str() == "CCD_RESOLUTION" {
                self.init_flags.max_res = true;
                self.init_timer = INIT_DELAY * CORE_TIMER_FREQ;
            }

            let is_exposure_property =
                indi::Connection::camera_is_exposure_property(
                    &prop_change.prop_name,
                    &new_prop.elem_name,
                    indi::CamCcd::from_ccd_prop_name(&mode_cam_device.prop)
                );
            if is_exposure_property && matches!(self.mode, Mode::WaitExposureProp(_)) {
                self.mode = Mode::WaitAfterRestart(0);
            }
        }

        Ok(())
    }

    pub fn control_camera_cooling(
        &self,
        indi:       &Arc<indi::Connection>,
        cam_device: &str,
        options:    &Options,
        force_set:  bool,
    ) -> anyhow::Result<()> {
        if indi.camera_is_cooler_supported(cam_device)? {
            if options.cam.ctrl.enable_cooler {
                log::info!("Setting camera temperature = {}", options.cam.ctrl.temperature);
                indi.camera_set_temperature(
                    cam_device,
                    options.cam.ctrl.temperature
                )?;
            }
            indi.camera_enable_cooler(
                cam_device,
                options.cam.ctrl.enable_cooler,
                force_set,
                INDI_SET_PROP_TIMEOUT
            )?;
        }
        Ok(())
    }

    pub fn control_camera_fan(
        &self,
        indi:       &Arc<indi::Connection>,
        cam_device: &str,
        options:    &Options,
        force_set:  bool,
    ) -> anyhow::Result<()> {
        if indi.camera_is_fan_supported(cam_device)? {
            let fan_enabled = options.cam.ctrl.enable_fan || options.cam.ctrl.enable_cooler;
            log::info!("Setting camera fan = {}", fan_enabled);
            indi.camera_control_fan(
                cam_device,
                fan_enabled,
                force_set,
                INDI_SET_PROP_TIMEOUT
            )?;
        }
        Ok(())
    }

    pub fn control_camera_heater(
        &self,
        indi:       &Arc<indi::Connection>,
        cam_device: &str,
        options:    &Options,
        force_set:  bool,
    ) -> anyhow::Result<()> {
        if indi.camera_is_heater_str_supported(cam_device)? {
            if let Some(heater_str) = &options.cam.ctrl.heater_str {
                log::info!("Setting camera heater = {}", heater_str);
                indi.camera_set_heater_str(
                    cam_device,
                    heater_str,
                    force_set,
                    INDI_SET_PROP_TIMEOUT
                )?;
            }
        }
        Ok(())
    }

    fn select_maximum_resolution(
        &self,
        indi:       &Arc<indi::Connection>,
        cam_device: &str,
    ) -> anyhow::Result<()> {
        if cam_device.contains(" Simulator") // don't do it for simulators
        || cam_device.is_empty() {
            return Ok(());
        }

        if indi.camera_is_resolution_supported(cam_device).unwrap_or(false) {
            log::info!("Setting maximum CCD resolution for camera {}", cam_device);
            indi.camera_select_max_resolution(
                cam_device,
                true,
                None
            )?;
        }
        Ok(())
    }


}