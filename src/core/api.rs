use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{commands::{ModeCommand, ModeCommandReply}, engine::Engine};
use crate::hal::TelescopeMoveDir;

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
    SetCameraDevice      { device_id: String },
    SetMountDevice       { device_id: String },
    SetFocuserDevice     { device_id: String },
    SetFilterWheelDevice { device_id: String },
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
