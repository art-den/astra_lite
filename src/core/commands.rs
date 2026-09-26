use serde::Serialize;

use crate::core::mode_polar_align::{PolarAlignCommand, PolarAlignState};

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
