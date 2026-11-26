use serde::{Serialize, Deserialize};

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

use crate::options::OptionPatch;

/// App-level command facade for UI clients (GTK now, web later).
/// Rule: plain data only (serializable).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    ChangeOptions(OptionPatch),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CommandReply {
    /// `changed` = false when the patch equalled current options (UI must suppress follow-ups).
    OptionsChanged { changed: bool },
}
