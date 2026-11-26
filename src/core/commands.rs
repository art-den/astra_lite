use serde::{Serialize, Deserialize};

use crate::core::mode_darks_lib::{DarkLibMode, MasterFileCreationProgramItem};
use crate::core::mode_goto::GotoConfig;
use crate::core::mode_polar_align::{PolarAlignCommand, PolarAlignState};
use crate::hal::TelescopeMoveDir;
use crate::sky_math::math::EqCoord;

/// A command for the active mode.
///
/// Rule: the command, its reply and every type they reference must be plain
/// data (no Arc<dyn ...>, Rc, closures, locks or references). This keeps them
/// serializable for the future web UI.
#[derive(Serialize, Debug, Clone)]
pub enum ModeCommand {
    PolarAlign(PolarAlignCommand),
}

/// Reply to a mode command.
#[derive(Serialize, Debug, Clone)]
pub enum ModeCommandReply {
    Empty,
    PolarAlignState(PolarAlignState),
}

use crate::options::OptionPatch;

/// Which work mode to start (cf. `ModeCommand`: a command for the already
/// active mode).
/// Rule: plain data only (serializable).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ModeStart {
    SingleShot,
    LiveView,
    SavingRawFrames,
    LiveStacking,
    Focusing,
    MountCalibr,
    CreatingDarkLibrary {
        dark_lib_mode: DarkLibMode,
        program:       Vec<MasterFileCreationProgramItem>,
    },
    GotoCoord {
        eq_coord: EqCoord,
        config:   GotoConfig,
    },
    GotoImage,
    CaptureAndPlatesolve,
    PolarAlign,
}

/// Plain data (serializable), cf. `Command` rule.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DeviceKind { Camera, Mount, Focuser, FilterWheel }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DeviceCommand {
    MountTrack(bool),
    MountPark,
    MountUnpark,
    /// Press of a nav button: applies inversions/slew speed from options, then moves
    MountMoveStart { dir: TelescopeMoveDir },
    /// Release of a nav button or press of the Stop button
    MountMoveStop,
    FocuserSetPosition { value: f64 },
    /// Relative move; clamping to the device range is done by the executor
    FocuserMoveRelative { offset: i32 },
    FilterWheelSelect { index: usize },
    /// Device selection from a combo box (replaces direct `change_*`, direct
    /// `options.<dev>.device` writes, and get_options re-syncs).
    /// `force` mirrors apply_* vs change_* (UI currently uses only force:false).
    SelectDevice { kind: DeviceKind, device_id: String, force: bool },
}

/// Shorthand for the `SelectDevice` command with `force: false` (UI combo selects)
pub fn select_device_cmd(kind: DeviceKind, device_id: &str) -> Command {
    Command::Device(DeviceCommand::SelectDevice {
        kind,
        device_id: device_id.to_string(),
        force: false,
    })
}

/// App-level command facade for UI clients (GTK now, web later).
/// Commands are queued and executed asynchronously by the engine command
/// thread; the caller gets no reply (the executor handles errors).
/// Rule: plain data only (serializable).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    ChangeOptions(OptionPatch),
    StartMode(ModeStart),
    AbortActiveMode,
    ContinueAbortedMode,
    Device(DeviceCommand),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Binning, Crop};

    /// The command queue is the future web API: JSON round trip is its contract
    #[test]
    fn command_json_round_trip() {
        let cmds = vec![
            Command::StartMode(ModeStart::LiveStacking),
            Command::StartMode(ModeStart::GotoCoord {
                eq_coord: EqCoord { ra: 1.5, dec: 0.5 },
                config:   GotoConfig::GotoPlateSolveAndCorrect,
            }),
            Command::StartMode(ModeStart::CreatingDarkLibrary {
                dark_lib_mode: DarkLibMode::MasterDark,
                program:       vec![MasterFileCreationProgramItem {
                    count:       3,
                    temperature: Some(-10.0),
                    exposure:    30.0,
                    gain:        100.0,
                    offset:      20,
                    binning:     Binning::Bin2,
                    crop:        Crop::P50,
                }],
            }),
            Command::AbortActiveMode,
            Command::ContinueAbortedMode,
            Command::Device(DeviceCommand::MountTrack(true)),
            Command::Device(DeviceCommand::MountPark),
            Command::Device(DeviceCommand::MountUnpark),
            Command::Device(DeviceCommand::MountMoveStart { dir: TelescopeMoveDir::NorthWest }),
            Command::Device(DeviceCommand::MountMoveStop),
            Command::Device(DeviceCommand::FocuserSetPosition { value: 123.0 }),
            Command::Device(DeviceCommand::FocuserMoveRelative { offset: -10 }),
            Command::Device(DeviceCommand::FilterWheelSelect { index: 2 }),
            Command::Device(DeviceCommand::SelectDevice {
                kind: DeviceKind::Mount,
                device_id: "mount-1".to_string(),
                force: false,
            }),
        ];
        for cmd in cmds {
            let json    = serde_json::to_string(&cmd).unwrap();
            let restored: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(json, serde_json::to_string(&restored).unwrap());
        }
    }
}
