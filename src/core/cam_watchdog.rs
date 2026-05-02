use std::sync::Arc;

use crate::{core::{cam_starter::CamStarter, consts::*, core::ModeData}, indi, options::{DeviceAndProp, Options}};

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

pub struct CameraWatchdog {
    cam_starter: Arc<CamStarter>,
    mode:        Mode,
    init_flags:  InitFlags,
    init_timer:  Option<usize>,
}

impl CameraWatchdog {
    pub fn new(cam_starter: &Arc<CamStarter>) -> Self {
        Self {
            cam_starter: Arc::clone(cam_starter),
            mode:        Mode::Waiting,
            init_flags:  InitFlags::default(),
            init_timer:  None,
        }
    }

    pub fn reset(&mut self) {
        self.mode = Mode::Waiting;
        self.init_flags = InitFlags::default();
        self.init_timer = None;
    }

    pub fn notify_timer(
        &mut self,
        timer_period_ms: usize,
        mode:            &mut ModeData,
        indi:            &Arc<indi::Connection>,
        options:         &Options,
    ) -> anyhow::Result<()> {
        if indi.state() != indi::ConnState::Connected {
            return Ok(());
        }

        let Some(cam_device) = &options.cam.device else { return Ok(()); };

        let is_waiting_for_blob_now = || -> anyhow::Result<bool> {
            let cam_ccd = indi::CamCcd::from_ccd_prop_name(&cam_device.prop);
            let Ok((exp_prop, exp_prop_elem)) = indi.camera_get_exposure_property(&cam_device.name, cam_ccd) else {
                return Ok(false);
            };
            let indi::PropValue::Num(expr_prop_num) = &exp_prop_elem.value else {
                return Ok(false)
            };
            Ok(
                (exp_prop.state == indi::PropState::Busy) &&
                expr_prop_num.value/*exposure*/ == 0.0
            )
        };

        match &mut self.mode {
            Mode::Waiting => {
                if is_waiting_for_blob_now()? {
                    self.mode = Mode::WaitBlob(0)
                }
            }
            Mode::WaitBlob(time_ms) => {
                if !is_waiting_for_blob_now()? {
                    self.mode = Mode::Waiting;
                    return Ok(());
                }
                *time_ms += timer_period_ms;
                if *time_ms >= MAX_WAIT_BLOB_TIME * 1000 {
                    log::info!(
                        "Waiting BLOB of camera {} too logn time (> {}) seconds",
                        cam_device.name, MAX_WAIT_BLOB_TIME
                    );
                    log::info!("Shutdown camera {} ...", cam_device.name);
                    indi.command_enable_device(&cam_device.name, false, true, None)?;
                    self.mode = Mode::Shutdown(0)
                }
            }
            Mode::Shutdown(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms >= MAX_SHUTDOWN_TIME * 1000 {
                    log::info!("Switching-on camera {} ...", cam_device.name);
                    indi.command_enable_device(&cam_device.name, true, true, None)?;
                    self.mode = Mode::WaitExposureProp(0);
                }
            }

            Mode::WaitExposureProp(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms >= WAIT_EXPOSURE_TIME * 1000 {
                    anyhow::bail!("Waiting exposure property too long time!");
                }
            }

            Mode::WaitAfterRestart(time_ms) => {
                *time_ms += timer_period_ms;
                if *time_ms == MAX_SWITCHING_ON_TIME * 1000 {
                    self.mode = Mode::Waiting;
                    self.restart_camera_exposure(mode, options)?;
                }
            }
        }

        if let Some(init_timer_ms) = &mut self.init_timer {
            *init_timer_ms += timer_period_ms;
            if *init_timer_ms >= INIT_DELAY * 1000 {
                self.init_timer = None;
                if self.init_flags.cooler {
                    self.init_flags.cooler = false;
                    Self::control_camera_cooling(indi, &cam_device.name, options, true)?;
                }
                if self.init_flags.fan {
                    self.init_flags.fan = false;
                    Self::control_camera_fan(indi, &cam_device.name, options, true)?;
                }
                if self.init_flags.heater {
                    self.init_flags.heater = false;
                    Self::control_camera_heater(indi, &cam_device.name, options, true)?;
                }
                if self.init_flags.max_res {
                    self.init_flags.max_res = false;
                    self.select_maximum_resolution(indi, &cam_device.name)?;
                }
            }
        }
        Ok(())
    }

    pub fn notify_indi_prop_change(
        &mut self,
        cur_cam_device: &Option<DeviceAndProp>,
        prop_change:    &indi::PropChangeEvent
    ) -> anyhow::Result<()> {
        let Some(cur_cam_device) = cur_cam_device else { return Ok(()); };

        if cur_cam_device.name != *prop_change.device_name {
            return Ok(());
        }

        if let indi::PropChange::Change { prop_name, elem_name, prev_state, new_state, .. } = &prop_change.change {
            let is_exposure_property =
                indi::Connection::camera_is_exposure_property(
                    prop_name,
                    elem_name,
                    indi::CamCcd::from_ccd_prop_name(&cur_cam_device.prop)
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

        if let indi::PropChange::New { prop_name, elem_name, .. } = &prop_change.change {
            let is_temperature_property =
                indi::Connection::camera_is_temperature_property(prop_name, elem_name);
            if is_temperature_property {
                self.init_flags.cooler = true;
                self.init_timer = Some(0);
            }

            let is_fan_str_property =
                indi::Connection::camera_is_fan_str_property(prop_name);
            if is_fan_str_property {
                self.init_flags.fan = true;
                self.init_timer = Some(0);
            }

            let is_heater_str_property =
                indi::Connection::camera_is_heater_str_property(prop_name);
            if is_heater_str_property {
                self.init_flags.heater = true;
                self.init_timer = Some(0);
            }

            if **prop_name == "CCD_RESOLUTION" {
                self.init_flags.max_res = true;
                self.init_timer = Some(0);
            }

            let is_exposure_property =
                indi::Connection::camera_is_exposure_property(
                    prop_name,
                    elem_name,
                    indi::CamCcd::from_ccd_prop_name(&cur_cam_device.prop)
                );
            if is_exposure_property && matches!(self.mode, Mode::WaitExposureProp(_)) {
                self.mode = Mode::WaitAfterRestart(0);
            }
        }

        Ok(())
    }

    pub fn control_camera_cooling(
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

    fn restart_camera_exposure(&self, mode: &mut ModeData, options: &Options) -> anyhow::Result<()> {
        let Some(cam_device) = mode.active.cam_device().cloned() else { return Ok(()); };
        log::error!("Begin restart exposure of camera {}...", cam_device.name);

        // Try to restart exposure by current mode
        let restarted_by_mode = mode.active.restart_cam_exposure()?;

        if !restarted_by_mode {
            // Mode not restarted the camera exposure. Do it itself

            self.cam_starter.abort(&cam_device)?;

            let mode_cam_opts =
                if let Some(frame_opts) = mode.active.frame_options_to_restart_exposure() {
                    frame_opts
                } else {
                    &options.cam.frame
                };

            self.cam_starter.take_shot(
                mode.active.get_type(),
                &cam_device,
                mode_cam_opts,
                &options.cam.ctrl
            )?;
        }
        log::error!("Exposure of camera {} restarted!", &cam_device.name);
        Ok(())
    }
}