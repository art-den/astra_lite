use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{commands::{ModeCommand, ModeCommandReply}, engine::Engine};

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
}

/// Reply to a `Command`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Reply {
    Empty,
    Mode(ModeCommandReply),
}

impl Engine {
    /// Executes a command synchronously on the caller's thread (a command
    /// queue for blocking device operations is planned).
    pub fn exec_command(self: &Arc<Self>, cmd: Command) -> eyre::Result<Reply> {
        log::debug!("Engine command: {:?}", cmd);
        match cmd {
            Command::Mode(mode_cmd) => Ok(Reply::Mode(self.exec_mode_command(mode_cmd)?)),
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
        ] {
            roundtrip(reply);
        }
    }
}
