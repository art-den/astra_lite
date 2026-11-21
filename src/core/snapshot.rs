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

use crate::guiding::external_guider::ExtGuiderState;
use crate::hal::{FocuserState, HalState, TelescopeState};
use crate::options::Options;

use super::engine::{BusyDomain, EngineModes, ModeBox, ModeKind};
use super::events::Progress;

/// Maps a failed trait-object state read (`state()`/`is_active()` under ASCOM
/// can fail) into `None` with a log line, as required by the snapshot DTOs.
pub(crate) fn opt_logged<T>(what: &str, result: eyre::Result<T>) -> Option<T> {
    result.map_err(|err| log::warn!("Snapshot: {what}: {err:#}")).ok()
}

/// Everything the UI needs to render engine state, read atomically enough for
/// display (see the module docs about snapshot tearing and `seq`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StateSnapshot {
    /// Monotonic engine change counter; consumers may refetch only when it
    /// differs from the previously seen value. Opaque, only monotonicity is
    /// guaranteed (double counting is allowed).
    ///
    /// Invariant: every change of snapshot-visible state must be accompanied
    /// by a seq increment. Channels: commands, HAL events, busy-flag
    /// transitions, frame-processing replies, `apply_notify_result`,
    /// `abort_active_mode`, guider events. The latter four bump strictly
    /// AFTER the write; the bump at the head of the HAL handler is a
    /// conservative grandfathered exception (bump-before-write is covered
    /// by the periodic polling and by later events).
    pub seq:     u64,
    pub mode:    ModeState,
    /// Device busy domains currently held by in-progress commands.
    pub busy:    Vec<BusyDomain>,
    pub options: Options,
    pub devices: ConnectedDevicesState,
    pub live_stacking: LiveStackingNums,
    pub permissions: UiPermissions,
}

/// A connected (selected into `cur_devices`) device of the engine.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceRef {
    pub id:        String,
    pub is_active: bool,
    /// Device-specific state; `None` for devices without a `state()` API
    /// (camera, filter wheel) or when the read failed (logged).
    pub state:     Option<DeviceState>,
}

/// Serializable union of the HAL trait `state()` results.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DeviceState {
    Telescope(TelescopeDeviceState),
    Focuser(FocuserState),
}

/// Mount state plus the flags the mount panel shows. Each field maps a
/// separate trait call (`state()`/`is_parked()`/`is_tracking()`): a failed
/// read becomes `None`/`false`, same as the panel's `unwrap_or` fallbacks.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TelescopeDeviceState {
    pub state:       Option<TelescopeState>,
    pub is_parked:   bool,
    pub is_tracking: bool,
}

/// Connected devices plus the per-backend connection states and guider state.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConnectedDevicesState {
    pub camera:       Option<DeviceRef>,
    pub mount:        Option<DeviceRef>,
    pub focuser:      Option<DeviceRef>,
    pub filter_wheel: Option<DeviceRef>,
    pub indi_state:   HalState,
    #[cfg(windows)]
    pub alpaca_state: HalState,
    pub guider:           Option<ExtGuiderState>,
    pub guider_connected: bool,
    pub phd2_working:     bool,
}

/// Numeric live stacking state (image data stays on the direct image channel).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LiveStackingNums {
    pub total_exposure: f64,
    pub time_cnt:       f64,
}

/// Mode-level enable flags for the UI, derived from `ModeState` with the same
/// predicates the panels used to compute inline. The final `caps && perms`
/// combination and the GTK action names stay in the panels.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UiPermissions {
    pub waiting:           bool,
    pub single_shot:       bool,
    pub liveview_active:   bool,
    pub saving_frames:     bool,
    pub live_active:       bool,
    /// An aborted `SavingRawFrames` mode is available to continue
    pub saving_frames_paused: bool,
    /// An aborted `LiveStacking` mode is available to continue
    pub live_stacking_paused: bool,
    pub focusing:          bool,
    pub polar_align:       bool,
    pub dither_calibr:     bool,
    pub saving_defect_pixels: bool,
    pub saving_master_darks:  bool,
    pub saving_master_biases: bool,
    /// Another mode can be started from the current one
    pub can_change_mode:   bool,
    pub can_change_cam_opts: bool,
    pub can_change_frame_opts: bool,
    pub can_change_live_stacking_opts: bool,
    pub can_change_cal_ops: bool,
}

impl UiPermissions {
    pub(crate) fn from_mode(mode: &ModeState) -> Self {
        let kind = mode.kind;
        let waiting = kind == ModeKind::Waiting;
        let single_shot = kind == ModeKind::SingleShot;
        let liveview_active = kind == ModeKind::LiveView;
        let saving_frames = kind == ModeKind::SavingRawFrames;
        let live_active = kind == ModeKind::LiveStacking;
        let is_aborted = |k| mode.aborted.as_ref().is_some_and(|m| m.kind == k);
        let can_change_mode = waiting || single_shot || liveview_active;
        Self {
            waiting,
            single_shot,
            liveview_active,
            saving_frames,
            live_active,
            saving_frames_paused: is_aborted(ModeKind::SavingRawFrames),
            live_stacking_paused: is_aborted(ModeKind::LiveStacking),
            focusing: kind == ModeKind::Focusing,
            polar_align: kind == ModeKind::PolarAlignment,
            dither_calibr: kind == ModeKind::DitherCalibr,
            saving_defect_pixels: matches!(kind,
                ModeKind::DefectPixels | ModeKind::CreatingDefectPixels),
            saving_master_darks: matches!(kind,
                ModeKind::MasterDark | ModeKind::CreatingMasterDarks),
            saving_master_biases: matches!(kind,
                ModeKind::MasterBias | ModeKind::CreatingMasterBiases),
            can_change_mode,
            can_change_cam_opts: !saving_frames && !live_active,
            can_change_frame_opts: waiting || liveview_active,
            can_change_live_stacking_opts: waiting || liveview_active,
            can_change_cal_ops: !liveview_active,
        }
    }
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
    /// Camera remaining exposure time, filled only while an exposure is in
    /// progress (device read at snapshot build time)
    pub remaining_time:  Option<f64>,
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
            remaining_time:  None, // filled by `Engine::snapshot_once()`
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
