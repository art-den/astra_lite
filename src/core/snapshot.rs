//! Serializable engine state snapshot (`Command::GetSnapshot`).
//!
//! This module contains DTOs only; `Engine::snapshot()` (the collecting side)
//! lives in `engine.rs` because it needs the private engine fields.
//!
//! Snapshot consumers (GTK panels today, a future web server) get a cheap
//! on-demand view of everything readable from engine memory without touching
//! devices. Expensive/derived reads go through `Command::Query` instead.
//!
//! Lock discipline of `Engine::snapshot()` — sequential scopes, never one
//! nested hold:
//! 1. `modes.read()` + `options.read()` together (base order modes -> options),
//!    then released;
//! 2. `cur_devices.data` mutex only long enough to clone the device `Arc`s;
//!    `state()`/`is_active()` on the trait objects run with NO engine locks
//!    held (under ASCOM these are COM calls);
//! 3. `live_stacking`: `stacker.read()` before `time_cnt.lock()` (same order
//!    as `LiveStacking::prepare_for_work`; the reverse order could deadlock);
//! 4. `ext_guider`: `state()`/`is_connected()` in a separate scope WITHOUT
//!    holding `modes` — `ExternalGuiderCtrl::disconnect()` legitimately holds
//!    the guider mutex while joining the PHD2 thread, which dispatches engine
//!    events taking `modes.write()`; nesting `modes -> ext_guider` here would
//!    create a real deadlock against that `ext_guider -> modes` order.

use serde::{Deserialize, Serialize};

use crate::options::Options;

use super::engine::{BusyDomain, EngineModes, ModeBox, ModeKind};
use super::events::Progress;

/// Everything the UI needs to render engine state, read atomically enough for
/// display (see the module docs about snapshot tearing and `seq`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StateSnapshot {
    /// Monotonic engine change counter; consumers may refetch only when it
    /// differs from the previously seen value. Opaque, only monotonicity is
    /// guaranteed (double counting is allowed).
    pub seq:     u64,
    pub mode:    ModeState,
    /// Device busy domains currently held by in-progress commands.
    pub busy:    Vec<BusyDomain>,
    pub options: Options,
}

/// The active mode plus the `finished`/`aborted` summaries the panels show.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModeState {
    pub kind:            ModeKind,
    pub progress:        Option<Progress>,
    pub progress_string: Option<String>,
    pub can_stop:        bool,
    pub can_continue:    bool,
    pub cur_exposure:    Option<f64>,
    pub finished:        Option<ModeSummary>,
    pub aborted:         Option<ModeSummary>,
}

/// A no-longer-active mode (`finished` or `aborted`), display data only.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModeSummary {
    pub kind:            ModeKind,
    pub progress_string: Option<String>,
    pub can_continue:    bool,
}

impl ModeState {
    pub(crate) fn from_modes(modes: &EngineModes) -> Self {
        let active = &*modes.active;
        Self {
            kind:            active.kind(),
            progress:        active.progress(),
            progress_string: Some(active.progress_string()),
            can_stop:        active.can_be_stopped(),
            can_continue:    active.can_be_continued_after_stop(),
            cur_exposure:    active.get_cur_exposure(),
            finished:        modes.finished.as_ref().map(ModeSummary::from_mode),
            aborted:         modes.aborted.as_ref().map(ModeSummary::from_mode),
        }
    }
}

impl ModeSummary {
    fn from_mode(mode: &ModeBox) -> Self {
        Self {
            kind:            mode.kind(),
            progress_string: Some(mode.progress_string()),
            can_continue:    mode.can_be_continued_after_stop(),
        }
    }
}
