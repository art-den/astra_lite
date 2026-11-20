use std::sync::Arc;

use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use super::{commands::{ModeCommand, ModeCommandReply}, engine::Engine, events::Event};
use crate::{
    hal::{FrameType, TelescopeMoveDir},
    options::{Binning, Crop, StarRecognSensitivity},
};

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
    Mount(MountCommand),
    Focuser(FocuserCommand),
    FilterWheel(FilterWheelCommand),
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
        match cmd {
            Command::Mode(mode_cmd) => Ok(Reply::Mode(self.exec_mode_command(mode_cmd)?)),
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
