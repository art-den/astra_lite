use serde::{Serialize, Deserialize};

use crate::core::mode_darks_lib::{DarkLibMode, MasterFileCreationProgramItem};
use crate::core::mode_goto::GotoConfig;
use crate::core::mode_polar_align::{PolarAlignCommand, PolarAlignState};
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
        ];
        for cmd in cmds {
            let json    = serde_json::to_string(&cmd).unwrap();
            let restored: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(json, serde_json::to_string(&restored).unwrap());
        }
    }
}
