use std::sync::Arc;

use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use super::{
    commands::{ModeCommand, ModeCommandReply},
    engine::{BusyDomain, Engine},
    events::Event,
    mode_darks_lib::{DarkLibMode, MasterFileCreationProgramItem},
    mode_goto::GotoConfig,
};

/// Raw INDI property commands
use crate::{
    guiding::external_guider::ExtGuiderType,
    hal::{FrameType, TelescopeMoveDir},
    options::{
        Binning, Crop, Gain, PreviewColorMode, PreviewScale, PreviewSource,
        StarRecognSensitivity,
    },
    sky_math::math::EqCoord,
};
#[cfg(windows)]
use crate::hal::HalImpl;

/// Device domain gated by the engine busy flag for this command.
/// Device-selection, mode lifecycle and option commands below are fast and
/// intentionally not gated; new device-domain variants must be added here
/// explicitly (no wildcard arm).
fn busy_domain_of(cmd: &Command) -> Option<BusyDomain> {
    match cmd {
        Command::Mount(_)       => Some(BusyDomain::Mount),
        Command::Focuser(_)     => Some(BusyDomain::Focuser),
        Command::FilterWheel(_) => Some(BusyDomain::FilterWheel),
        Command::Guider(_)      => Some(BusyDomain::Guider),
        Command::Indi(_)        => Some(BusyDomain::Indi),
        Command::Connect(_)     => Some(BusyDomain::Connect),
        Command::Mode(_)
        | Command::Start(_)
        | Command::StopActiveMode
        | Command::ContinueAbortedMode
        | Command::SetCameraDevice{..} | Command::SetMountDevice{..}
        | Command::SetFocuserDevice{..} | Command::SetFilterWheelDevice{..}
        | Command::OpenImageFile{..}
        | Command::Preview(_) => None,
        // Option commands:
        Command::SetCooling{..} | Command::SetFan{..} | Command::SetHeater{..}
        | Command::SetCamReadoutOptions{..} | Command::SetFrameType{..}
        | Command::SetAutoExpEnabled{..} | Command::SetExposure{..}
        | Command::SetGain{..} | Command::SetOffset{..} | Command::SetBinning{..}
        | Command::SetCrop{..} | Command::SetRawFramesCount{..}
        | Command::SetLiveViewEnabled{..} | Command::SetLiveStackingOptions{..}
        | Command::SetQualityOptions{..} | Command::SetCalibrOptions{..}
        | Command::SetTelescopeFocalLen{..} | Command::SetBarlow{..}
        | Command::SetGuidingFocalLen{..} | Command::SetFocuserOptions{..}
        | Command::SetPlateSolverOptions{..} | Command::SetGuidingOptions{..} => None,
    }
}

/// Telescope (mount) control commands.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MountCommand {
    Track(bool),
    Park,
    Unpark,
    /// Facade order: `revert_motion` -> `set_slew_speed` (if `speed_id`) -> `move_`.
    MoveStart {
        dir:      TelescopeMoveDir,
        inv_ns:   bool,
        inv_we:   bool,
        speed_id: Option<String>,
    },
    AbortMotion,
}

/// Focuser control commands.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FocuserCommand {
    MoveBy(f64),
    SetAbs(f64),
}

/// Filter wheel control commands.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FilterWheelCommand {
    SetSlot { slot: usize },
}

/// Raw INDI property commands (device control panel and device-wide actions).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum IndiCommand {
    SetSwitchProperty { device: String, prop: String, elements: Vec<(String, bool)> },
    SetNumberProperty { device: String, prop: String, elements: Vec<(String, f64)> },
    SetTextProperty   { device: String, prop: String, elements: Vec<(String, String)> },
    /// Fan-out: set one switch element on all connected INDI devices.
    SetSwitchElementForAllDevices { prop: String, element: String },
}

/// External auto-guider control commands.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum GuiderCommand {
    ConnectPhd2,
    DisconnectPhd2,
}

/// Image preview display options (one command per independent control).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PreviewCommand {
    SetSource     { source: PreviewSource },
    SetScale      { scale: PreviewScale },
    SetColor      { color: PreviewColorMode },
    SetDarkLvl    { dark_lvl: f64 },
    SetLightLvl   { light_lvl: f64 },
    SetGamma      { gamma: f64 },
    SetRemoveGrad { remove_grad: bool },
    SetWbAuto     { wb_auto: bool },
    SetWbRed      { wb_red: f64 },
    SetWbGreen    { wb_green: f64 },
    SetWbBlue     { wb_blue: f64 },
    SetStars      { stars: bool },
}

/// Working modes that can be started via `Command::Start`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StartMode {
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
    Focusing,
    MountCalibr,
    Goto { coord: EqCoord, config: GotoConfig },
    /// Goto to the currently shown preview image; the image data is resolved
    /// by the facade from the engine's own preview state.
    GotoImage,
    CaptureAndPlatesolve,
    PolarAlignment,
    CreatingDarkLibrary { mode: DarkLibMode, program: Vec<MasterFileCreationProgramItem> },
}

/// Hardware connection management commands.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ConnectCommand {
    ConnectIndi {
        remote:    bool,
        address:   String,
        mount:     Option<String>,
        camera:    Option<String>,
        guid_cam:  Option<String>,
        focuser:   Option<String>,
        flt_wheel: Option<String>,
        aux1:      Option<String>,
        aux2:      Option<String>,
    },
    DisconnectIndi { disable_all: bool },
    #[cfg(windows)]
    ConnectAlpaca { address: String },
    #[cfg(windows)]
    DisconnectAlpaca,
}

/// Single serializable entry point for engine state mutations issued by the UI
/// (and later by a future web server).
///
/// Rule (same as in `commands.rs`): commands and replies and every type they
/// reference must be plain data (`Serialize + Deserialize`, no `Arc<dyn ...>`,
/// Rc, closures, locks or references; paths are `String`).
///
/// Only mutations go through here. Read-only access (HAL device lists,
/// `options.read()`, mode queries) stays as is until the snapshot/query API.
///
/// Command handlers must keep the global lock order modes -> options and must
/// not run long/blocking work while holding engine locks.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    Mode(ModeCommand),
    Start(StartMode),
    /// Aborts the active mode (it may leave a continuable aborted mode behind).
    StopActiveMode,
    ContinueAbortedMode,
    Mount(MountCommand),
    Focuser(FocuserCommand),
    FilterWheel(FilterWheelCommand),
    Connect(ConnectCommand),
    Guider(GuiderCommand),
    Indi(IndiCommand),
    SetCameraDevice      { device_id: String },
    SetMountDevice       { device_id: String },
    SetFocuserDevice     { device_id: String },
    SetFilterWheelDevice { device_id: String },
    // Camera options: one command per "field group -> event" pair (same events
    // the GTK panels sent before the facade).
    SetCooling  { enable_cooler: bool, temperature: f64 },
    SetFan      { enable_fan: bool },
    SetHeater   { heater_str: Option<String> },
    SetCamReadoutOptions { conv_gain_str: Option<String>, low_noise: bool, high_fullwell: bool },
    SetFrameType { frame_type: FrameType },
    SetAutoExpEnabled { enabled: bool },
    SetExposure { exposure: f64 },
    SetGain     { gain: f64 },
    SetOffset   { offset: i32 },
    SetBinning  { binning: Binning },
    SetCrop     { crop: Crop },
    SetRawFramesCount { frame_cnt: usize },
    SetLiveViewEnabled { enabled: bool },
    SetLiveStackingOptions { remove_tracks: bool, frame_cnt: usize },
    SetQualityOptions {
        use_max_fwhm:      bool,
        max_fwhm:          f32,
        use_max_ovality:   bool,
        max_ovality:       f32,
        ignore_3px_stars:  bool,
        star_recogn_sens:  StarRecognSensitivity,
        check_ccd_temp:    bool,
        max_ccd_temp_diff: f64,
    },
    SetCalibrOptions {
        dark_frame_en:    bool,
        flat_frame_en:    bool,
        flat_frame_fname: String,
        hot_pixels:       bool,
    },
    // Optics
    SetTelescopeFocalLen { focal_len: f64 },
    SetBarlow { barlow: f64 },
    SetGuidingFocalLen { foc_len: f64 },
    // Preview and misc options
    Preview(PreviewCommand),
    SetFocuserOptions { exposure: f64, gain: Gain },
    SetPlateSolverOptions { exposure: f64, gain: Gain, bin: Binning },
    SetGuidingOptions { calibr_exposure: f64, calibr_gain: Gain },
    OpenImageFile { path: String },
}

/// Reply to a `Command`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Reply {
    Empty,
    Mode(ModeCommandReply),
    /// New absolute position after `FocuserCommand::MoveBy`;
    /// `None` when the move was skipped.
    FocuserMove { new_position: Option<f64> },
}

impl Engine {
    /// Executes a command synchronously on the caller's thread (a command
    /// queue for blocking device operations is planned).
    pub fn exec_command(self: &Arc<Self>, cmd: Command) -> eyre::Result<Reply> {
        log::debug!("Engine command: {:?}", cmd);
        let _busy = match busy_domain_of(&cmd) {
            Some(domain) => Some(self.lock_device_busy(domain)?),
            None => None,
        };
        match cmd {
            Command::Mode(mode_cmd) => Ok(Reply::Mode(self.exec_mode_command(mode_cmd)?)),
            Command::Start(start_mode) => {
                match start_mode {
                    StartMode::SingleShot        => self.start_single_shot()?,
                    StartMode::LiveView          => self.start_live_view()?,
                    StartMode::SavingRawFrames   => self.start_saving_raw_frames()?,
                    StartMode::LiveStacking      => self.start_live_stacking()?,
                    StartMode::Focusing          => self.start_focusing()?,
                    StartMode::MountCalibr       => self.start_mount_calibr()?,
                    StartMode::Goto { coord, config } => self.start_goto_coord(&coord, config)?,
                    StartMode::GotoImage         => self.start_goto_image()?,
                    StartMode::CaptureAndPlatesolve => self.start_capture_and_platesolve()?,
                    StartMode::PolarAlignment    => self.start_polar_alignment()?,
                    StartMode::CreatingDarkLibrary { mode, program } =>
                        self.start_creating_dark_library(mode, &program)?,
                }
                Ok(Reply::Empty)
            }
            Command::StopActiveMode => {
                self.abort_active_mode();
                Ok(Reply::Empty)
            }
            Command::ContinueAbortedMode => {
                self.continue_aborted_mode()?;
                Ok(Reply::Empty)
            }
            Command::SetCooling { enable_cooler, temperature } => {
                let mut options = self.options.write().unwrap();
                if options.cam.ctrl.enable_cooler == enable_cooler
                    && options.cam.ctrl.temperature == temperature {
                    return Ok(Reply::Empty);
                }
                options.cam.ctrl.enable_cooler = enable_cooler;
                options.cam.ctrl.temperature = temperature;
                drop(options);
                self.events.send(Event::CameraCoolingOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetFan { enable_fan } => {
                let mut options = self.options.write().unwrap();
                if options.cam.ctrl.enable_fan == enable_fan {
                    return Ok(Reply::Empty);
                }
                options.cam.ctrl.enable_fan = enable_fan;
                drop(options);
                self.events.send(Event::CameraFanOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetHeater { heater_str } => {
                let mut options = self.options.write().unwrap();
                if options.cam.ctrl.heater_str == heater_str {
                    return Ok(Reply::Empty);
                }
                options.cam.ctrl.heater_str = heater_str;
                drop(options);
                self.events.send(Event::CameraHeaterOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetCamReadoutOptions { conv_gain_str, low_noise, high_fullwell } => {
                let mut options = self.options.write().unwrap();
                options.cam.ctrl.conv_gain_str = conv_gain_str;
                options.cam.ctrl.low_noise = low_noise;
                options.cam.ctrl.high_fullwell = high_fullwell;
                Ok(Reply::Empty)
            }
            Command::SetFrameType { frame_type } => {
                let mut options = self.options.write().unwrap();
                if options.cam.frame.frame_type == frame_type {
                    return Ok(Reply::Empty);
                }
                options.cam.frame.frame_type = frame_type;
                drop(options);
                self.events.send(Event::CameraFrameTypeChanged);
                Ok(Reply::Empty)
            }
            Command::SetAutoExpEnabled { enabled } => {
                self.options.write().unwrap().cam.frame.auto_exp = enabled;
                Ok(Reply::Empty)
            }
            Command::SetExposure { exposure } => {
                let mut options = self.options.write().unwrap();
                if options.cam.frame.exposure() == exposure {
                    return Ok(Reply::Empty);
                }
                options.cam.frame.set_exposure(exposure);
                drop(options);
                self.events.send(Event::CameraExposureChanged);
                Ok(Reply::Empty)
            }
            Command::SetGain { gain } => {
                let mut options = self.options.write().unwrap();
                if options.cam.frame.gain == gain {
                    return Ok(Reply::Empty);
                }
                options.cam.frame.gain = gain;
                drop(options);
                self.events.send(Event::CameraFrameOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetOffset { offset } => {
                let mut options = self.options.write().unwrap();
                if options.cam.frame.offset == offset {
                    return Ok(Reply::Empty);
                }
                options.cam.frame.offset = offset;
                drop(options);
                self.events.send(Event::CameraFrameOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetBinning { binning } => {
                let mut options = self.options.write().unwrap();
                if options.cam.frame.binning == binning {
                    return Ok(Reply::Empty);
                }
                options.cam.frame.binning = binning;
                drop(options);
                self.events.send(Event::CameraFrameOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetCrop { crop } => {
                let mut options = self.options.write().unwrap();
                if options.cam.frame.crop == crop {
                    return Ok(Reply::Empty);
                }
                options.cam.frame.crop = crop;
                drop(options);
                self.events.send(Event::CameraFrameOptionsChanged);
                Ok(Reply::Empty)
            }
            Command::SetRawFramesCount { frame_cnt } => {
                self.options.write().unwrap().raw_frames.frame_cnt = frame_cnt;
                Ok(Reply::Empty)
            }
            Command::SetLiveViewEnabled { enabled } => {
                self.options.write().unwrap().cam.live_view = enabled;
                Ok(Reply::Empty)
            }
            Command::SetLiveStackingOptions { remove_tracks, frame_cnt } => {
                let mut options = self.options.write().unwrap();
                options.live.remove_tracks = remove_tracks;
                options.live.frame_cnt = frame_cnt;
                Ok(Reply::Empty)
            }
            Command::SetQualityOptions {
                use_max_fwhm, max_fwhm, use_max_ovality, max_ovality,
                ignore_3px_stars, star_recogn_sens, check_ccd_temp, max_ccd_temp_diff,
            } => {
                let mut options = self.options.write().unwrap();
                options.quality.use_max_fwhm       = use_max_fwhm;
                options.quality.max_fwhm           = max_fwhm;
                options.quality.use_max_ovality    = use_max_ovality;
                options.quality.max_ovality        = max_ovality;
                options.quality.ignore_3px_stars   = ignore_3px_stars;
                options.quality.star_recogn_sens   = star_recogn_sens;
                options.quality.check_ccd_temp     = check_ccd_temp;
                options.quality.max_ccd_temp_diff  = max_ccd_temp_diff;
                Ok(Reply::Empty)
            }
            Command::SetCalibrOptions { dark_frame_en, flat_frame_en, flat_frame_fname, hot_pixels } => {
                let mut options = self.options.write().unwrap();
                options.calibr.dark_frame_en    = dark_frame_en;
                options.calibr.flat_frame_en    = flat_frame_en;
                options.calibr.flat_frame_fname = PathBuf::from(flat_frame_fname);
                options.calibr.hot_pixels       = hot_pixels;
                Ok(Reply::Empty)
            }
            Command::SetTelescopeFocalLen { focal_len } => {
                let mut options = self.options.write().unwrap();
                if f64::abs(options.telescope.focal_len - focal_len) < 0.1 {
                    return Ok(Reply::Empty);
                }
                options.telescope.focal_len = focal_len;
                drop(options);
                self.events.send(Event::TelescopeFocalLenChanged(focal_len));
                Ok(Reply::Empty)
            }
            Command::SetBarlow { barlow } => {
                self.options.write().unwrap().telescope.barlow = barlow;
                self.events.send(Event::TelescopeBarlowChanged);
                Ok(Reply::Empty)
            }
            Command::SetGuidingFocalLen { foc_len } => {
                let mut options = self.options.write().unwrap();
                if f64::abs(options.guiding.foc_len - foc_len) < 0.1 {
                    return Ok(Reply::Empty);
                }
                options.guiding.foc_len = foc_len;
                drop(options);
                self.events.send(Event::GuiderFocalLenChanged(foc_len));
                Ok(Reply::Empty)
            }
            Command::Preview(preview_cmd) => {
                let mut options = self.options.write().unwrap();
                match preview_cmd {
                    PreviewCommand::SetSource { source }          => options.preview.source = source,
                    PreviewCommand::SetScale { scale }            => options.preview.scale = scale,
                    PreviewCommand::SetColor { color }            => options.preview.color = color,
                    PreviewCommand::SetDarkLvl { dark_lvl }       => options.preview.dark_lvl = dark_lvl,
                    PreviewCommand::SetLightLvl { light_lvl }     => options.preview.light_lvl = light_lvl,
                    PreviewCommand::SetGamma { gamma }            => options.preview.gamma = gamma,
                    PreviewCommand::SetRemoveGrad { remove_grad } => options.preview.remove_grad = remove_grad,
                    PreviewCommand::SetWbAuto { wb_auto }         => options.preview.wb_auto = wb_auto,
                    PreviewCommand::SetWbRed { wb_red }           => options.preview.wb_red = wb_red,
                    PreviewCommand::SetWbGreen { wb_green }       => options.preview.wb_green = wb_green,
                    PreviewCommand::SetWbBlue { wb_blue }         => options.preview.wb_blue = wb_blue,
                    PreviewCommand::SetStars { stars }            => options.preview.stars = stars,
                }
                Ok(Reply::Empty)
            }
            Command::SetFocuserOptions { exposure, gain } => {
                let mut options = self.options.write().unwrap();
                options.focuser.exposure = exposure;
                options.focuser.gain = gain;
                Ok(Reply::Empty)
            }
            Command::SetPlateSolverOptions { exposure, gain, bin } => {
                let mut options = self.options.write().unwrap();
                options.plate_solver.exposure = exposure;
                options.plate_solver.gain = gain;
                options.plate_solver.bin = bin;
                Ok(Reply::Empty)
            }
            Command::SetGuidingOptions { calibr_exposure, calibr_gain } => {
                let mut options = self.options.write().unwrap();
                options.guiding.main_cam.calibr_exposure = calibr_exposure;
                options.guiding.main_cam.calibr_gain = calibr_gain;
                Ok(Reply::Empty)
            }
            Command::OpenImageFile { path } => {
                self.open_image_from_file(&PathBuf::from(path))?;
                Ok(Reply::Empty)
            }
            Command::SetCameraDevice { device_id } => {
                self.cur_devices.change_camera(&device_id);
                Ok(Reply::Empty)
            }
            Command::SetMountDevice { device_id } => {
                self.cur_devices.change_telescope(&device_id);
                Ok(Reply::Empty)
            }
            Command::SetFocuserDevice { device_id } => {
                self.cur_devices.change_focuser(&device_id);
                Ok(Reply::Empty)
            }
            Command::SetFilterWheelDevice { device_id } => {
                self.cur_devices.change_filter_wheel(&device_id);
                Ok(Reply::Empty)
            }
            Command::Indi(indi_cmd) => {
                let indi = self.hal.indi_impl().indi();
                match indi_cmd {
                    IndiCommand::SetSwitchProperty { device, prop, elements } => {
                        let elements: Vec<(&str, bool)> = elements
                            .iter().map(|(n, v)| (n.as_str(), *v)).collect();
                        indi.command_set_switch_property(&device, &prop, &elements)?;
                    }
                    IndiCommand::SetNumberProperty { device, prop, elements } => {
                        let elements: Vec<(&str, f64)> = elements
                            .iter().map(|(n, v)| (n.as_str(), *v)).collect();
                        indi.command_set_num_property(&device, &prop, &elements)?;
                    }
                    IndiCommand::SetTextProperty { device, prop, elements } => {
                        let elements: Vec<(&str, &str)> = elements
                            .iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
                        indi.command_set_text_property(&device, &prop, &elements)?;
                    }
                    IndiCommand::SetSwitchElementForAllDevices { prop, element } => {
                        for device in indi.get_devices_list() {
                            indi.command_set_switch_property(
                                &device.name, &prop, &[(&element, true)]
                            )?;
                        }
                    }
                }
                Ok(Reply::Empty)
            }
            Command::Guider(guider_cmd) => {
                match guider_cmd {
                    GuiderCommand::ConnectPhd2 => {
                        self.ext_guider.create_and_connect(ExtGuiderType::Phd2)?;
                    }
                    GuiderCommand::DisconnectPhd2 => {
                        self.ext_guider.disconnect()?;
                    }
                }
                Ok(Reply::Empty)
            }
            Command::Connect(connect_cmd) => {
                match connect_cmd {
                    ConnectCommand::ConnectIndi {
                        remote, address, mount, camera, guid_cam, focuser, flt_wheel, aux1, aux2,
                    } => {
                        self.hal.indi_impl().connect(
                            remote, &address, &mount, &camera, &guid_cam,
                            &focuser, &flt_wheel, &aux1, &aux2,
                        )?;
                    }
                    ConnectCommand::DisconnectIndi { disable_all } => {
                        let indi = self.hal.indi_impl().indi();
                        if disable_all {
                            log::info!("Disabling all INDI devices before disconnect...");
                            indi.command_enable_all_devices(false, true, Some(2000))?;
                            log::info!("Done");
                        }
                        log::info!("Disconnecting INDI...");
                        indi.disconnect_and_wait()?;
                        log::info!("Done");
                    }
                    #[cfg(windows)]
                    ConnectCommand::ConnectAlpaca { address } => {
                        self.hal.ascom_alpaca_impl().connect(&address)?;
                    }
                    #[cfg(windows)]
                    ConnectCommand::DisconnectAlpaca => {
                        self.hal.ascom_alpaca_impl().disconnect()?;
                    }
                }
                Ok(Reply::Empty)
            }
            Command::FilterWheel(filter_wheel_cmd) => {
                let filter_wheel = self.cur_devices.filter_wheel_or_err()?;
                match filter_wheel_cmd {
                    FilterWheelCommand::SetSlot { slot } => filter_wheel.set_active(slot)?,
                }
                Ok(Reply::Empty)
            }
            Command::Focuser(focuser_cmd) => {
                let focuser = self.cur_devices.focuser_or_err()?;
                match focuser_cmd {
                    FocuserCommand::MoveBy(offset) => {
                        let mut value = focuser.abs_position()?;
                        let range = focuser.abs_position_range()?;
                        value += offset;
                        if f64::clamp(value, *range.start(), *range.end()) == value {
                            return Ok(Reply::FocuserMove { new_position: None });
                        }
                        focuser.set_abs_position(value)?;
                        Ok(Reply::FocuserMove { new_position: Some(value) })
                    }
                    FocuserCommand::SetAbs(value) => {
                        focuser.set_abs_position(value)?;
                        Ok(Reply::Empty)
                    }
                }
            }
            Command::Mount(mount_cmd) => {
                let telescope = self.cur_devices.telescope_or_err()?;
                match mount_cmd {
                    MountCommand::Track(enabled) => telescope.track(enabled)?,
                    MountCommand::Park => telescope.park()?,
                    MountCommand::Unpark => telescope.unpark()?,
                    MountCommand::MoveStart { dir, inv_ns, inv_we, speed_id } => {
                        telescope.revert_motion(inv_ns, inv_we)?;
                        if let Some(speed_id) = speed_id {
                            telescope.set_slew_speed(&speed_id)?;
                        }
                        telescope.move_(dir)?;
                    }
                    MountCommand::AbortMotion => telescope.abort_motion()?,
                }
                Ok(Reply::Empty)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::core::mode_polar_align::{PolarAlignCommand, PolarAlignState};

    fn roundtrip<T: Serialize + DeserializeOwned + std::fmt::Debug>(value: T) {
        let json = serde_json::to_string(&value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2, "roundtrip failed for {:?}", value);
    }

    #[test]
    fn command_reply_serde_roundtrip() {
        for cmd in [
            Command::Mode(ModeCommand::PolarAlign(PolarAlignCommand::Restart)),
            Command::Mode(ModeCommand::PolarAlign(PolarAlignCommand::ManualRefresh)),
            Command::Mode(ModeCommand::PolarAlign(PolarAlignCommand::GetState)),
        ] {
            roundtrip(cmd);
        }

        for cmd in [
            Command::Start(StartMode::SingleShot),
            Command::Start(StartMode::LiveView),
            Command::Start(StartMode::SavingRawFrames),
            Command::Start(StartMode::LiveStacking),
            Command::Start(StartMode::Focusing),
            Command::Start(StartMode::MountCalibr),
            Command::Start(StartMode::Goto {
                coord: EqCoord { dec: 0.5, ra: 1.5 },
                config: GotoConfig::OnlyGoto,
            }),
            Command::Start(StartMode::Goto {
                coord: EqCoord { dec: -0.25, ra: 3.0 },
                config: GotoConfig::GotoPlateSolveAndCorrect,
            }),
            Command::Start(StartMode::GotoImage),
            Command::Start(StartMode::CaptureAndPlatesolve),
            Command::Start(StartMode::PolarAlignment),
            Command::Start(StartMode::CreatingDarkLibrary {
                mode: DarkLibMode::MasterDark,
                program: vec![MasterFileCreationProgramItem {
                    count: 3, temperature: Some(-10.0), exposure: 60.0,
                    gain: 100.0, offset: 30, binning: Binning::Bin2, crop: Crop::P50,
                }],
            }),
            Command::Start(StartMode::CreatingDarkLibrary {
                mode: DarkLibMode::DefectPixels,
                program: Vec::new(),
            }),
            Command::StopActiveMode,
            Command::ContinueAbortedMode,
        ] {
            roundtrip(cmd);
        }

        for cmd in [
            Command::SetCameraDevice { device_id: "cam1".to_string() },
            Command::SetMountDevice { device_id: "mnt1".to_string() },
            Command::SetFocuserDevice { device_id: "foc1".to_string() },
            Command::SetFilterWheelDevice { device_id: "fw1".to_string() },
            Command::Mount(MountCommand::Track(true)),
            Command::Mount(MountCommand::Park),
            Command::Mount(MountCommand::Unpark),
            Command::Mount(MountCommand::MoveStart {
                dir:      TelescopeMoveDir::NorthWest,
                inv_ns:   true,
                inv_we:   false,
                speed_id: Some("2".to_string()),
            }),
            Command::Mount(MountCommand::MoveStart {
                dir:      TelescopeMoveDir::SouthEast,
                inv_ns:   false,
                inv_we:   false,
                speed_id: None,
            }),
            Command::Mount(MountCommand::AbortMotion),
            Command::Focuser(FocuserCommand::MoveBy(-25.5)),
            Command::Focuser(FocuserCommand::SetAbs(1000.0)),
            Command::FilterWheel(FilterWheelCommand::SetSlot { slot: 3 }),
            Command::SetCooling { enable_cooler: true, temperature: -10.5 },
            Command::SetFan { enable_fan: false },
            Command::SetHeater { heater_str: Some("auto".to_string()) },
            Command::SetHeater { heater_str: None },
            Command::SetCamReadoutOptions {
                conv_gain_str: Some("x1".to_string()), low_noise: true, high_fullwell: false,
            },
            Command::SetFrameType { frame_type: FrameType::Flats },
            Command::SetAutoExpEnabled { enabled: true },
            Command::SetExposure { exposure: 3.25 },
            Command::SetGain { gain: 100.0 },
            Command::SetOffset { offset: -10 },
            Command::SetBinning { binning: Binning::Bin2 },
            Command::SetCrop { crop: Crop::P50 },
            Command::SetRawFramesCount { frame_cnt: 10 },
            Command::SetLiveViewEnabled { enabled: true },
            Command::SetLiveStackingOptions { remove_tracks: true, frame_cnt: 42 },
            Command::SetQualityOptions {
                use_max_fwhm: true, max_fwhm: 4.5, use_max_ovality: false,
                max_ovality: 2.25, ignore_3px_stars: true,
                star_recogn_sens: StarRecognSensitivity::High,
                check_ccd_temp: false, max_ccd_temp_diff: 1.5,
            },
            Command::SetCalibrOptions {
                dark_frame_en: true, flat_frame_en: false,
                flat_frame_fname: "/tmp/flat.fits".to_string(), hot_pixels: true,
            },
            Command::Connect(ConnectCommand::ConnectIndi {
                remote: true, address: "localhost".to_string(),
                mount: Some(" monta".to_string()), camera: None, guid_cam: None,
                focuser: Some("foc".to_string()), flt_wheel: None,
                aux1: None, aux2: None,
            }),
            Command::Connect(ConnectCommand::DisconnectIndi { disable_all: true }),
            Command::Connect(ConnectCommand::DisconnectIndi { disable_all: false }),
            Command::Indi(IndiCommand::SetSwitchProperty {
                device: "dev".to_string(), prop: "CONNECTION".to_string(),
                elements: vec![("CONNECT".to_string(), true)],
            }),
            Command::Indi(IndiCommand::SetNumberProperty {
                device: "dev".to_string(), prop: "GAIN".to_string(),
                elements: vec![("GAIN".to_string(), 5.5)],
            }),
            Command::Indi(IndiCommand::SetTextProperty {
                device: "dev".to_string(), prop: "ACTIVE_SETTINGS".to_string(),
                elements: vec![("SETTING_NAME".to_string(), "x".to_string())],
            }),
            Command::Indi(IndiCommand::SetSwitchElementForAllDevices {
                prop: "CONFIG_PROCESS".to_string(), element: "CONFIG_SAVE".to_string(),
            }),
            Command::Guider(GuiderCommand::ConnectPhd2),
            Command::Guider(GuiderCommand::DisconnectPhd2),
            #[cfg(windows)]
            Command::Connect(ConnectCommand::ConnectAlpaca {
                address: "http://localhost:11111".to_string(),
            }),
            #[cfg(windows)]
            Command::Connect(ConnectCommand::DisconnectAlpaca),
            Command::SetTelescopeFocalLen { focal_len: 550.0 },
            Command::SetBarlow { barlow: 1.5 },
            Command::SetGuidingFocalLen { foc_len: 240.0 },
            Command::Preview(PreviewCommand::SetSource { source: PreviewSource::LiveStacking }),
            Command::Preview(PreviewCommand::SetScale { scale: PreviewScale::P200 }),
            Command::Preview(PreviewCommand::SetColor { color: PreviewColorMode::Blue }),
            Command::Preview(PreviewCommand::SetDarkLvl { dark_lvl: 0.1 }),
            Command::Preview(PreviewCommand::SetLightLvl { light_lvl: 0.9 }),
            Command::Preview(PreviewCommand::SetGamma { gamma: 2.2 }),
            Command::Preview(PreviewCommand::SetRemoveGrad { remove_grad: true }),
            Command::Preview(PreviewCommand::SetWbAuto { wb_auto: false }),
            Command::Preview(PreviewCommand::SetWbRed { wb_red: 1.0 }),
            Command::Preview(PreviewCommand::SetWbGreen { wb_green: 1.5 }),
            Command::Preview(PreviewCommand::SetWbBlue { wb_blue: 0.5 }),
            Command::Preview(PreviewCommand::SetStars { stars: true }),
            Command::SetFocuserOptions { exposure: 2.5, gain: Gain::P50 },
            Command::SetPlateSolverOptions { exposure: 5.0, gain: Gain::Same, bin: Binning::Bin2 },
            Command::SetGuidingOptions { calibr_exposure: 3.5, calibr_gain: Gain::Max },
            Command::OpenImageFile { path: "/tmp/test.fits".to_string() },
        ] {
            roundtrip(cmd);
        }

        for reply in [
            Reply::Empty,
            Reply::Mode(ModeCommandReply::Empty),
            Reply::Mode(ModeCommandReply::PolarAlignState(PolarAlignState::Undefined)),
            Reply::Mode(ModeCommandReply::PolarAlignState(PolarAlignState::Goto {
                time_ms:         1234,
                goto_ok_time_ms: 567,
                target:          crate::sky_math::math::EqCoord { dec: -0.5, ra: 1.25 },
            })),
            Reply::Mode(ModeCommandReply::PolarAlignState(PolarAlignState::Capture)),
            Reply::Mode(ModeCommandReply::PolarAlignState(PolarAlignState::PlateSolve)),
            Reply::Mode(ModeCommandReply::PolarAlignState(PolarAlignState::WaitForManualRefresh)),
            Reply::FocuserMove { new_position: None },
            Reply::FocuserMove { new_position: Some(123.5) },
        ] {
            roundtrip(reply);
        }
    }
}
