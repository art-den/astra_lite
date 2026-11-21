use std::sync::Arc;

use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use super::{
    engine::{BusyDomain, Engine, ModeKind},
    events::Event,
    mode_darks_lib::{
        DarkLibMode, DarkLibSpec, MasterFileCreationProgramItem, build_program,
    },
    mode_goto::GotoConfig,
    mode_polar_align::{PolarAlignCommand, PolarAlignMode, PolarAlignState},
    snapshot::StateSnapshot,
};

/// Raw INDI property commands
use crate::{
    guiding::external_guider::ExtGuiderType,
    hal::{Camera, DeviceInfo, DeviceType, FrameType, TelescopeMoveDir, TelescopeSite},
    options::{
        Binning, Crop, Gain, GuidingMode, Options, PlateSolverType, PolarAlignDir,
        PreviewColorMode, PreviewScale, PreviewSource, StarRecognSensitivity,
    },
    sky_math::math::EqCoord,
    utils::io_utils::save_json_to_config,
};
#[cfg(windows)]
use crate::hal::HalImpl;

/// Device domain gated by the engine busy flag for this command.
/// Mode lifecycle and option commands below are fast and intentionally not
/// gated; new device-domain variants must be added here explicitly (no
/// wildcard arm).
fn busy_domain_of(cmd: &Command) -> Option<BusyDomain> {
    match cmd {
        Command::Mount(_)       => Some(BusyDomain::Mount),
        Command::Focuser(_)     => Some(BusyDomain::Focuser),
        Command::FilterWheel(_) => Some(BusyDomain::FilterWheel),
        Command::Guider(_)      => Some(BusyDomain::Guider),
        Command::Indi(_)        => Some(BusyDomain::Indi),
        Command::Connect(_)     => Some(BusyDomain::Connect),
        // Device selection synchronously deactivates/activates HAL devices
        Command::SetCameraDevice{..} | Command::SetMountDevice{..}
        | Command::SetFocuserDevice{..} | Command::SetFilterWheelDevice{..}
            => Some(BusyDomain::Connect),
        Command::Mode(_)
        | Command::Start(_)
        | Command::StopActiveMode
        | Command::ContinueAbortedMode
        | Command::Shutdown { .. } | Command::SaveOptions
        | Command::GetSnapshot
        | Command::Query(Query::Devices | Query::CameraCaps | Query::IndiProperties { .. }
            | Query::IndiDrivers | Query::PolarAlignCheck)
        | Command::OpenImageFile{..}
        | Command::Preview(_) => None,
        // Blocking `site()` HAL calls, serialized with mount commands
        Command::Query(Query::AllMountSites) => Some(BusyDomain::Mount),
        // Option commands:
        Command::SetCooling(..) | Command::SetFan{..} | Command::SetHeater{..}
        | Command::SetCamReadoutOptions(..) | Command::SetFrameType{..}
        | Command::SetAutoExpEnabled{..} | Command::SetExposure{..}
        | Command::SetGain{..} | Command::SetOffset{..} | Command::SetBinning{..}
        | Command::SetCrop{..} | Command::SetRawFramesCount{..}
        | Command::SetRawFramesOptions(..)
        | Command::SetLiveViewEnabled{..} | Command::SetLiveStackingOptions(..)
        | Command::SetQualityOptions(..) | Command::SetCalibrOptions(..)
        | Command::SetDarkLibraryPath{..}
        | Command::SetTelescopeFocalLen{..} | Command::SetBarlow{..}
        | Command::SetGuidingFocalLen{..} | Command::SetFocuserOptions(..)
        | Command::SetPlateSolverOptions(..) | Command::SetPolarAlignOptions(..)
        | Command::SetGuidingOptions(..)
        | Command::SetIndiSelection(..) | Command::SetAlpacaAddress{..}
        | Command::SetSiteLatitude{..} | Command::SetSiteLongitude{..}
        | Command::SetTelescopeFromPlateSolve{..} | Command::SetMountOptions(..)
        // Read-only query: pure computation + capability read, no camera exclusivity
        | Command::BuildDarkLibraryProgram{..} => None,
    }
}

/// New absolute target for `FocuserCommand::MoveBy` (clamp-and-move semantics):
/// `Ok(None)` when the focuser is already at the range boundary in the
/// requested direction. Guards `f64::clamp` against NaN/inverted-range panics.
fn focuser_move_by_target(
    pos:    f64,
    offset: f64,
    start:  f64,
    end:    f64,
) -> eyre::Result<Option<f64>> {
    if !pos.is_finite() || !start.is_finite() || !end.is_finite() || start > end {
        eyre::bail!("Focuser MoveBy: invalid position {pos} or range {start}..{end}");
    }
    let target = pos + offset;
    if target.is_nan() {
        eyre::bail!("Focuser MoveBy: NaN offset {offset}");
    }
    let target = f64::clamp(target, start, end);
    if target == pos {
        Ok(None)
    } else {
        Ok(Some(target))
    }
}

/// Read-only commands must not advance `seq` (polling would look like a change).
/// Applies on both the `exec_command` and the `submit_command` paths.
/// Exhaustive match (like `busy_domain_of`): new variants must be classified
/// explicitly.
pub(crate) fn is_read_only(cmd: &Command) -> bool {
    match cmd {
        Command::GetSnapshot
        | Command::Query(Query::Devices | Query::CameraCaps
            | Query::IndiProperties { .. } | Query::IndiDrivers | Query::AllMountSites
            | Query::PolarAlignCheck)
        | Command::BuildDarkLibraryProgram { .. }
        // Pure mode state read polled by the polar align panel
        | Command::Mode(ModeCommand::PolarAlign(PolarAlignCommand::GetState)) => true,
        Command::Mode(_) | Command::Start(_)
        | Command::StopActiveMode | Command::ContinueAbortedMode
        | Command::Shutdown { .. } | Command::SaveOptions
        | Command::Mount(_) | Command::Focuser(_) | Command::FilterWheel(_)
        | Command::Connect(_) | Command::Guider(_) | Command::Indi(_)
        | Command::SetCameraDevice{..} | Command::SetMountDevice{..}
        | Command::SetFocuserDevice{..} | Command::SetFilterWheelDevice{..}
        | Command::SetCooling(..) | Command::SetFan{..} | Command::SetHeater{..}
        | Command::SetCamReadoutOptions(..) | Command::SetFrameType{..}
        | Command::SetAutoExpEnabled{..} | Command::SetExposure{..}
        | Command::SetGain{..} | Command::SetOffset{..} | Command::SetBinning{..}
        | Command::SetCrop{..} | Command::SetRawFramesCount{..}
        | Command::SetRawFramesOptions(..)
        | Command::SetLiveViewEnabled{..} | Command::SetLiveStackingOptions(..)
        | Command::SetQualityOptions(..) | Command::SetCalibrOptions(..)
        | Command::SetDarkLibraryPath{..}
        | Command::SetTelescopeFocalLen{..} | Command::SetBarlow{..}
        | Command::SetGuidingFocalLen{..} | Command::SetFocuserOptions(..)
        | Command::SetPlateSolverOptions(..) | Command::SetPolarAlignOptions(..)
        | Command::SetGuidingOptions(..)
        | Command::SetIndiSelection(..) | Command::SetAlpacaAddress{..}
        | Command::SetSiteLatitude{..} | Command::SetSiteLongitude{..}
        | Command::SetTelescopeFromPlateSolve{..} | Command::SetMountOptions(..)
        | Command::OpenImageFile{..}
        | Command::Preview(_) => false,
    }
}

/// A command for the active mode (plain-data rule: see [`Command`]).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ModeCommand {
    PolarAlign(PolarAlignCommand),
}

/// Reply to a mode command.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ModeCommandReply {
    Empty,
    PolarAlignState(PolarAlignState),
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
    /// Preview widget allocation (runtime-only options, never persisted).
    /// The panel sends it only on a real change to avoid a `seq` bump on
    /// every size-allocate.
    SetWidgetSize { width: usize, height: usize },
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

/// Partial update of a group of options (PATCH semantics): a `None` field
/// leaves the corresponding option unchanged. Every patch derives `Default`,
/// so callers set only the fields they really changed:
/// `QualityPatch { use_max_fwhm: Some(true), ..Default::default() }`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CoolingPatch {
    pub enable_cooler: Option<bool>,
    pub temperature:   Option<f64>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CamReadoutPatch {
    /// Double `Option` patch field: absent in JSON = unchanged, `null` = set to `None`.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub conv_gain_str:  Option<Option<String>>,
    pub low_noise:      Option<bool>,
    pub high_fullwell:  Option<bool>,
}

/// Deserialize a patch field with PATCH semantics: `null` means "set to `None`";
/// a field absent from the payload means "unchanged" (handled by the container
/// `#[serde(default)]`).
fn de_patch_opt<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    struct OptOptVisitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for OptOptVisitor<T> {
        type Value = Option<Option<T>>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("null or a value")
        }
        fn visit_some<D: serde::Deserializer<'de>>(
            self, d: D,
        ) -> Result<Self::Value, D::Error> {
            Ok(Some(Option::<T>::deserialize(d)?))
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }
    }
    deserializer.deserialize_option(OptOptVisitor(std::marker::PhantomData))
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct LiveStackingPatch {
    pub remove_tracks: Option<bool>,
    pub frame_cnt:     Option<usize>,
    pub use_cnt:       Option<bool>,
    pub save_orig:     Option<bool>,
    pub save_enabled:  Option<bool>,
    pub save_minutes:  Option<usize>,
    pub out_dir:       Option<String>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct RawFramesPatch {
    pub use_cnt:       Option<bool>,
    pub out_path:      Option<String>,
    pub create_master: Option<bool>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct QualityPatch {
    pub use_max_fwhm:      Option<bool>,
    pub max_fwhm:          Option<f32>,
    pub use_max_ovality:   Option<bool>,
    pub max_ovality:       Option<f32>,
    pub ignore_3px_stars:  Option<bool>,
    pub star_recogn_sens:  Option<StarRecognSensitivity>,
    pub check_ccd_temp:    Option<bool>,
    pub max_ccd_temp_diff: Option<f64>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CalibrPatch {
    pub dark_frame_en:    Option<bool>,
    pub flat_frame_en:    Option<bool>,
    pub flat_frame_fname: Option<String>,
    pub hot_pixels:       Option<bool>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct FocuserPatch {
    pub exposure:            Option<f64>,
    pub gain:                Option<Gain>,
    pub on_temp_change:      Option<bool>,
    pub max_temp_change:     Option<f64>,
    pub on_fwhm_change:      Option<bool>,
    pub max_fwhm_change:     Option<u32>,
    pub periodically:        Option<bool>,
    pub period_minutes:      Option<u32>,
    pub measures:            Option<u32>,
    pub step:                Option<f64>,
    pub anti_backlash_steps: Option<usize>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct PlateSolverPatch {
    pub exposure:      Option<f64>,
    pub gain:          Option<Gain>,
    pub bin:           Option<Binning>,
    pub solver:        Option<PlateSolverType>,
    pub timeout:       Option<u32>,
    pub blind_timeout: Option<u32>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct PolarAlignPatch {
    pub angle:        Option<f64>,
    pub direction:    Option<PolarAlignDir>,
    /// Double `Option` patch field: absent in JSON = unchanged, `null` = set to `None`.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub speed:        Option<Option<String>>,
    pub auto_refresh: Option<bool>,
    /// Debug-only simulated mount errors (see the develop mode panel)
    pub sim_alt_err:  Option<f64>,
    pub sim_az_err:   Option<f64>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct GuidingPatch {
    pub calibr_exposure:    Option<f64>,
    pub calibr_gain:        Option<Gain>,
    pub mode:               Option<GuidingMode>,
    pub dith_period:        Option<u32>,
    pub ext_dith_dist:      Option<i32>,
    pub main_cam_dith_dist: Option<i32>,
    pub main_cam_max_error: Option<f64>,
}

/// See [`CoolingPatch`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct MountPatch {
    pub inv_ns: Option<bool>,
    pub inv_we: Option<bool>,
    /// Double `Option` patch field: absent in JSON = unchanged, `null` = set to `None`.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub speed:  Option<Option<String>>,
}

/// Partial update of the INDI connection selection (see [`CoolingPatch`]).
/// Driver fields are double `Option` (see [`de_patch_opt`]): `null` deselects.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct IndiSelPatch {
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub mount:     Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub camera:    Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub guid_cam:  Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub focuser:   Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub flt_wheel: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub aux1:      Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_patch_opt")]
    pub aux2:      Option<Option<String>>,
    pub remote:    Option<bool>,
    pub address:   Option<String>,
}

/// Single serializable entry point for engine state mutations issued by the UI
/// (and later by a future web server).
///
/// Rule: commands and replies and every type they reference must be plain
/// data (`Serialize + Deserialize`, no `Arc<dyn ...>`, Rc, closures, locks or
/// references; paths are `String`).
///
/// Mutations go through here, and also read-only queries that need engine
/// state the caller cannot access itself (device capabilities, derived
/// computations, e.g. `BuildDarkLibraryProgram`); such query commands take
/// no busy domain. Engine state reading goes through `GetSnapshot` (and
/// `Query` as it is added): no public in-process state accessors for the UI.
///
/// Command handlers must keep the global lock order modes -> options and must
/// not run long/blocking work while holding engine locks.
///
/// Threading contract:
/// - `exec_command` runs synchronously on the caller's thread. Option
///   commands are fast and may be called from the UI thread; device-selection
///   commands re-activate HAL devices synchronously (seconds on connect/
///   disconnect) and are gated by `BusyDomain::Connect`: while that domain is
///   busy they fail immediately with a "busy" error; device-domain commands
///   (Mount/Focuser/FilterWheel/Guider/Indi/Connect) block until the device
///   operation returns — UI code should use `submit_command` for those.
/// - Fast read commands (`GetSnapshot`, fast queries) must be executed with
///   `exec_command` only: through the submit queue they would serialize behind
///   blocking device commands. Blocking device queries must use
///   `submit_command`.
/// - `submit_command` enqueues onto a single FIFO worker thread; the reply is
///   delivered as `Event::CommandReply { id, result }` sent FROM THAT THREAD.
///   UI must consume it through the main-thread event bridge (`ui_main`
///   relays core events via a channel); handlers subscribed directly to
///   `events` must not touch GTK widgets when handling `CommandReply`.
/// - Commands of the same device domain are serialized by the engine busy
///   flags: submitting a second command while one is in progress fails with
///   a "busy" error (see `BusyDomain`).
///
/// Shutdown contract: `Shutdown` disconnects all event subscribers, so its
/// `CommandReply` may never be delivered to an async client (expected; a
/// synchronous `exec_command` caller gets the reply directly). A graceful
/// exit sequence is `Shutdown { force }` then `SaveOptions` — saving AFTER
/// shutdown is what captures the per-camera options written during it.
///
/// Known exceptions (intentional non-facade UI accesses; the live device
/// reads below are marked with a comment in the panel and to be closed by
/// follow-up queries, the subscription sites are inherent until remote
/// clients get their own protocols):
/// - Live device state reads: mount coords / slew speed (`Query::MountNav`,
///   `ui_mount`, `ui_skymap`, `ui_polar_align`), live CCD/pixel-size reads
///   (`ui_skymap`), plate-solve camera reads (with `Query::CameraCaps`
///   cache, `ui_plate_solve`), focuser panel camera exposure-range read
///   (`Query::FocuserCaps`, `ui_focuser`), focuser position/range/
///   temperature (`Query::FocuserState`, `ui_focuser`), filter wheel
///   list/active (`Query::FilterWheelState`, `ui_flt_wheel`).
/// - HAL/INDI/PHD2 event subscriptions kept by `ui_main` (the event bridge
///   itself), `ui_hardware` and `indi_panel_widget`: device lifecycle and
///   INDI registry panels need direct subscriptions until a subscription
///   protocol for remote clients exists.
/// - `FileNameUtils::init(&Arc<dyn Camera>)` is a direct HAL read used
///   inside the engine (not the UI); the UI path is `init_from_data` fed by
///   `CameraCaps`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Command {
    Mode(ModeCommand),
    Start(StartMode),
    /// Aborts the active mode (it may leave a continuable aborted mode behind).
    StopActiveMode,
    ContinueAbortedMode,
    /// Full engine shutdown (see `Engine::shutdown` for the sequence).
    /// `force: false` refuses while a mode other than `Waiting` is active.
    Shutdown { force: bool },
    /// Persist engine `Options` to the config dir (see the shutdown contract above).
    SaveOptions,
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
    // the GTK panels sent before the facade). Multi-field groups take a partial
    // update patch (see [`CoolingPatch`]).
    SetCooling(CoolingPatch),
    SetFan      { enable_fan: bool },
    SetHeater   { heater_str: Option<String> },
    SetCamReadoutOptions(CamReadoutPatch),
    SetFrameType { frame_type: FrameType },
    SetAutoExpEnabled { enabled: bool },
    SetExposure { exposure: f64 },
    SetGain     { gain: f64 },
    SetOffset   { offset: i32 },
    SetBinning  { binning: Binning },
    SetCrop     { crop: Crop },
    SetRawFramesCount { frame_cnt: usize },
    SetRawFramesOptions(RawFramesPatch),
    SetLiveViewEnabled { enabled: bool },
    SetLiveStackingOptions(LiveStackingPatch),
    SetQualityOptions(QualityPatch),
    SetCalibrOptions(CalibrPatch),
    SetDarkLibraryPath { path: String },
    // Optics
    SetTelescopeFocalLen { focal_len: f64 },
    SetBarlow { barlow: f64 },
    SetGuidingFocalLen { foc_len: f64 },
    // Preview and misc options
    Preview(PreviewCommand),
    SetFocuserOptions(FocuserPatch),
    SetPlateSolverOptions(PlateSolverPatch),
    SetPolarAlignOptions(PolarAlignPatch),
    SetGuidingOptions(GuidingPatch),
    // Hardware panel: connection selection, site, misc telescope options
    SetIndiSelection(IndiSelPatch),
    SetAlpacaAddress     { address: String },
    SetSiteLatitude      { sexagesimal: String },
    SetSiteLongitude     { sexagesimal: String },
    SetTelescopeFromPlateSolve { enabled: bool },
    SetMountOptions(MountPatch),
    OpenImageFile { path: String },
    /// Build the master file creation program for a dark library spec
    /// (read-only query, see `busy_domain_of`).
    BuildDarkLibraryProgram { spec: DarkLibSpec },
    /// Serializable engine state snapshot (mode, options, busy domains).
    /// Cheap read from engine memory only, no device access; synchronous
    /// execution only (see the threading contract above).
    GetSnapshot,
    /// Expensive/derived reads (see [`Query`]). Fast variants execute like
    /// `GetSnapshot` (synchronous only); blocking query variants must use
    /// `submit_command`.
    Query(Query),
}

/// Read commands for state the caller cannot access itself and which is too
/// expensive or blocking for `GetSnapshot`.
/// New variants must be registered in `busy_domain_of` explicitly.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Query {
    /// All HAL devices in one reply (from HAL caches); callers filter by
    /// `type_` themselves.
    Devices,
    /// Capabilities of the currently selected camera (the capability calls
    /// the camera panel made on the camera object directly before this API).
    CameraCaps,
    /// INDI device/property registry for the INDI GUI panel (`since` =
    /// incremental fetch by change_id).
    IndiProperties { since: Option<u64> },
    /// Grouped INDI driver registry (device driver lists).
    /// Note: in the reply's `DriverInterface` bitflags the `GENERAL` flag is
    /// the zero value and serializes as an empty string (see its docs).
    IndiDrivers,
    /// Sites of all connected telescopes. Makes blocking `site()` HAL
    /// calls: must go through `submit_command`, not the caller's thread.
    /// Gated by `BusyDomain::Mount`: fails immediately with a "busy" error
    /// while a mount command is in progress.
    AllMountSites,
    /// Pre-start polar alignment check (see `PolarAlignMode::check_before_start`).
    /// Makes blocking `eq_coord()` HAL reads: must go through
    /// `submit_command`, not the caller's thread.
    PolarAlignCheck,
}

/// Site of one telescope, for the `Query::AllMountSites` reply.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MountSite {
    pub id:   String,
    pub name: String,
    pub site: TelescopeSite,
}

/// INDI device with the panel's sort keys (camera caps); zeros when unavailable.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndiDeviceEntry {
    pub device:         indi::ExportDevice,
    pub pixel_size_um:  (f64, f64),
    pub max_frame_size: (u32, u32),
}

/// Payload of `QueryReply::IndiProperties`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndiPropertiesData {
    pub devices:    Vec<IndiDeviceEntry>,
    pub properties: Vec<indi::Property>,
}

/// Truncate BLOB payloads at the query API boundary (the full bytes would be
/// a base64/integer-array explosion over JSON): the reply carries an empty
/// `data` and the payload size in `data_len`.
fn truncate_blob_payload(mut prop: indi::Property) -> indi::Property {
    for elem in &mut prop.elements {
        if let indi::PropValue::Blob(blob) = &mut elem.value {
            *blob = Arc::new(indi::BlobPropValue {
                format:   blob.format.clone(),
                data:     Vec::new(),
                data_len: blob.data.len(),
                dl_time:  blob.dl_time,
            });
        }
    }
    prop
}

/// Answer to a [`Query`].
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum QueryReply {
    Devices(Vec<DeviceInfo>),
    CameraCaps(CameraCaps),
    IndiProperties(IndiPropertiesData),
    IndiDrivers(indi::Drivers),
    AllMountSites(Vec<MountSite>),
    /// Warning text for `Query::PolarAlignCheck`; empty when the check passed
    /// without warnings. A failed check arrives as `Err` in the command reply.
    PolarAlignCheck(String),
}

/// Serializable `RangeInclusive<f64>`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct FloatRange {
    pub start: f64,
    pub end:   f64,
}

/// Capability snapshot of the current camera (see [`Query::CameraCaps`]).
/// A `*_supported` flag mirrors the trait `is_*_supported()` result; a range
/// is `None` when unsupported, the read failed (logged), or the capability
/// gate is false.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CameraCaps {
    /// Camera display name (file naming in the panel)
    pub name:                      String,
    /// Sensor size in pixels (0,0 when the read failed)
    pub ccd_size:                  (usize, usize),
    /// Max supported binning factor (min of x/y; 0 when the read failed)
    pub max_binning:               usize,
    /// (id, label) heater list; empty when unsupported
    pub heater_ctrl_list:          Vec<(String, String)>,
    /// (id, label) conversion gain list; empty when unsupported
    pub conversion_gain_list:      Vec<(String, String)>,
    /// `None` also means 'exposure not supported' (mirrors the panel check)
    pub exposure_range:            Option<FloatRange>,
    pub cooler_supported:          bool,
    pub temperature_range:         Option<FloatRange>,
    pub gain_supported:            bool,
    pub gain_range:                Option<FloatRange>,
    pub offset_supported:          bool,
    pub offset_range:              Option<FloatRange>,
    pub frame_supported:           bool,
    pub binning_supported:         bool,
    pub fan_ctrl_supported:        bool,
    pub heater_supported:          bool,
    pub low_noise_supported:       bool,
    pub conversion_gain_supported: bool,
    pub high_fullwell_supported:   bool,
}

fn caps_supported(result: eyre::Result<bool>, what: &str) -> bool {
    super::snapshot::opt_logged(what, result).unwrap_or(false)
}

fn caps_range(
    result: eyre::Result<std::ops::RangeInclusive<f64>>,
    what:   &str,
) -> Option<FloatRange> {
    super::snapshot::opt_logged(what, result)
        .map(|r| FloatRange { start: *r.start(), end: *r.end() })
}

fn caps_list(
    result: eyre::Result<Vec<(String, String)>>,
    what:   &str,
) -> Vec<(String, String)> {
    super::snapshot::opt_logged(what, result).unwrap_or_default()
}

impl CameraCaps {
    /// Reads all capabilities from the camera object. Ranges are only queried
    /// when the capability is supported (same order as the camera panel).
    fn of(camera: &(dyn Camera + Send + Sync)) -> Self {
        let mut caps = Self {
            name:                      camera.name().to_string(),
            ccd_size:                  super::snapshot::opt_logged(
                                           "ccd_size", camera.ccd_size())
                                       .unwrap_or((0, 0)),
            max_binning:               super::snapshot::opt_logged(
                                           "max_binning", camera.max_binning())
                                       .map(|(x, y)| usize::min(x, y))
                                       .unwrap_or(0),
            heater_ctrl_list:          Vec::new(),
            conversion_gain_list:      Vec::new(),
            exposure_range:            caps_range(camera.exposure_range(), "exposure_range"),
            cooler_supported:          caps_supported(camera.is_cooler_supported(), "is_cooler_supported"),
            temperature_range:         None,
            gain_supported:            caps_supported(camera.is_gain_supported(), "is_gain_supported"),
            gain_range:                None,
            offset_supported:          caps_supported(camera.is_offset_supported(), "is_offset_supported"),
            offset_range:              None,
            frame_supported:           caps_supported(camera.is_frame_supported(), "is_frame_supported"),
            binning_supported:         caps_supported(camera.is_binning_supported(), "is_binning_supported"),
            fan_ctrl_supported:        caps_supported(camera.is_fan_ctrl_supported(), "is_fan_ctrl_supported"),
            heater_supported:          caps_supported(camera.is_heater_supported(), "is_heater_supported"),
            low_noise_supported:       caps_supported(camera.is_low_noise_supported(), "is_low_noise_supported"),
            conversion_gain_supported: caps_supported(camera.is_conversion_gain_supported(), "is_conversion_gain_supported"),
            high_fullwell_supported:   caps_supported(camera.is_high_fullwell_supported(), "is_high_fullwell_supported"),
        };
        if caps.cooler_supported {
            caps.temperature_range = caps_range(camera.temperature_range(), "temperature_range");
        }
        if caps.gain_supported {
            caps.gain_range = caps_range(camera.gain_range(), "gain_range");
        }
        if caps.offset_supported {
            caps.offset_range = caps_range(camera.offset_range(), "offset_range");
        }
        if caps.heater_supported {
            caps.heater_ctrl_list = caps_list(
                camera.heater_ctrl_list(), "heater_ctrl_list");
        }
        if caps.conversion_gain_supported {
            caps.conversion_gain_list = caps_list(
                camera.conversion_gain_list(), "conversion_gain_list");
        }
        caps
    }
}

/// Reply to a `Command`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Reply {
    Empty,
    Mode(ModeCommandReply),
    /// New absolute position after `FocuserCommand::MoveBy` (the target is
    /// clamped to the device range);
    /// `None` when already at the range boundary in the requested direction.
    FocuserMove { new_position: Option<f64> },
    /// Answer to `Command::BuildDarkLibraryProgram` (`mode` echoes the spec variant).
    DarkLibraryProgram {
        mode:               DarkLibMode,
        program:            Vec<MasterFileCreationProgramItem>,
        total_frames:       usize,
        total_duration_sec: f64,
    },
    /// Answer to `Command::GetSnapshot` (boxed to keep the enum small).
    Snapshot(Box<StateSnapshot>),
    /// Answer to `Command::Query` (boxed to keep the enum small).
    Query(Box<QueryReply>),
}

impl Engine {
    /// Enqueues the command onto the engine's FIFO command-queue worker
    /// thread (see the `Command` docs threading contract). The reply arrives
    /// as `Event::CommandReply` with the returned id.
    pub fn submit_command(&self, cmd: Command) -> eyre::Result<u64> {
        log::debug!("Engine submit: {:?}", cmd);
        self.enqueue_command(cmd)
    }

    /// Executes a command synchronously on the caller's thread (see the
    /// `Command` docs threading contract).
    pub fn exec_command(self: &Arc<Self>, cmd: Command) -> eyre::Result<Reply> {
        log::debug!("Engine command: {:?}", cmd);
        let read_only = is_read_only(&cmd);
        let reply = self.exec_command_inner(cmd)?;
        if !read_only {
            self.bump_seq();
        }
        Ok(reply)
    }

    fn exec_command_inner(self: &Arc<Self>, cmd: Command) -> eyre::Result<Reply> {
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
            Command::Shutdown { force } => {
                if !force {
                    let kind = self.modes().active.kind();
                    if kind != ModeKind::Waiting {
                        eyre::bail!("Shutdown: mode {kind:?} is active");
                    }
                }
                self.shutdown();
                Ok(Reply::Empty)
            }
            Command::SaveOptions => {
                let options = self.options.read().unwrap();
                save_json_to_config::<Options>(&options, "options")?;
                Ok(Reply::Empty)
            }
            Command::SetCooling(patch) => {
                let mut options = self.options.write().unwrap();
                let mut changed = false;
                if let Some(v) = patch.enable_cooler
                    && options.cam.ctrl.enable_cooler != v {
                        options.cam.ctrl.enable_cooler = v;
                        changed = true;
                }
                if let Some(v) = patch.temperature
                    && options.cam.ctrl.temperature != v {
                        options.cam.ctrl.temperature = v;
                        changed = true;
                }
                if !changed {
                    return Ok(Reply::Empty);
                }
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
            Command::SetCamReadoutOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.conv_gain_str { options.cam.ctrl.conv_gain_str = v; }
                if let Some(v) = patch.low_noise     { options.cam.ctrl.low_noise = v; }
                if let Some(v) = patch.high_fullwell { options.cam.ctrl.high_fullwell = v; }
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
            Command::SetRawFramesOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.use_cnt       { options.raw_frames.use_cnt = v; }
                if let Some(v) = patch.out_path      {
                    options.raw_frames.out_path = PathBuf::from(v);
                }
                if let Some(v) = patch.create_master { options.raw_frames.create_master = v; }
                Ok(Reply::Empty)
            }
            Command::SetLiveStackingOptions(patch) => {
                if let Some(v) = patch.save_minutes
                    && v < 1 {
                        return Err(eyre::eyre!("live stacking save interval must be >= 1 minute"));
                    }
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.remove_tracks { options.live.remove_tracks = v; }
                if let Some(v) = patch.frame_cnt     { options.live.frame_cnt = v; }
                if let Some(v) = patch.use_cnt       { options.live.use_cnt = v; }
                if let Some(v) = patch.save_orig     { options.live.save_orig = v; }
                if let Some(v) = patch.save_enabled  { options.live.save_enabled = v; }
                if let Some(v) = patch.save_minutes  { options.live.save_minutes = v; }
                if let Some(v) = patch.out_dir {
                    options.live.out_dir = PathBuf::from(v);
                }
                Ok(Reply::Empty)
            }
            Command::SetQualityOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.use_max_fwhm      { options.quality.use_max_fwhm = v; }
                if let Some(v) = patch.max_fwhm          { options.quality.max_fwhm = v; }
                if let Some(v) = patch.use_max_ovality   { options.quality.use_max_ovality = v; }
                if let Some(v) = patch.max_ovality       { options.quality.max_ovality = v; }
                if let Some(v) = patch.ignore_3px_stars  { options.quality.ignore_3px_stars = v; }
                if let Some(v) = patch.star_recogn_sens  { options.quality.star_recogn_sens = v; }
                if let Some(v) = patch.check_ccd_temp    { options.quality.check_ccd_temp = v; }
                if let Some(v) = patch.max_ccd_temp_diff { options.quality.max_ccd_temp_diff = v; }
                Ok(Reply::Empty)
            }
            Command::SetCalibrOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.dark_frame_en    { options.calibr.dark_frame_en = v; }
                if let Some(v) = patch.flat_frame_en    { options.calibr.flat_frame_en = v; }
                if let Some(v) = patch.flat_frame_fname {
                    options.calibr.flat_frame_fname = PathBuf::from(v);
                }
                if let Some(v) = patch.hot_pixels       { options.calibr.hot_pixels = v; }
                Ok(Reply::Empty)
            }
            Command::SetDarkLibraryPath { path } => {
                self.options.write().unwrap().calibr.dark_library_path = PathBuf::from(path);
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
                let mut options = self.options.write().unwrap();
                if options.telescope.barlow == barlow {
                    return Ok(Reply::Empty);
                }
                options.telescope.barlow = barlow;
                drop(options);
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
                    PreviewCommand::SetWbBlue { wb_blue }           => options.preview.wb_blue = wb_blue,
                    PreviewCommand::SetStars { stars }              => options.preview.stars = stars,
                    PreviewCommand::SetWidgetSize { width, height } => {
                        if options.preview.widget_width != width {
                            options.preview.widget_width = width;
                        }
                        if options.preview.widget_height != height {
                            options.preview.widget_height = height;
                        }
                    }
                }
                Ok(Reply::Empty)
            }
            Command::SetFocuserOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.exposure            { options.focuser.exposure = v; }
                if let Some(v) = patch.gain                { options.focuser.gain = v; }
                if let Some(v) = patch.on_temp_change      { options.focuser.on_temp_change = v; }
                if let Some(v) = patch.max_temp_change     { options.focuser.max_temp_change = v; }
                if let Some(v) = patch.on_fwhm_change      { options.focuser.on_fwhm_change = v; }
                if let Some(v) = patch.max_fwhm_change     { options.focuser.max_fwhm_change = v; }
                if let Some(v) = patch.periodically        { options.focuser.periodically = v; }
                if let Some(v) = patch.period_minutes      { options.focuser.period_minutes = v; }
                if let Some(v) = patch.measures            { options.focuser.measures = v; }
                if let Some(v) = patch.step                { options.focuser.step = v; }
                if let Some(v) = patch.anti_backlash_steps { options.focuser.anti_backlash_steps = v; }
                Ok(Reply::Empty)
            }
            Command::SetPlateSolverOptions(patch) => {
                if patch.timeout.is_some_and(|v| v < 1)
                    || patch.blind_timeout.is_some_and(|v| v < 1) {
                        return Err(eyre::eyre!("plate solve timeout must be >= 1 sec"));
                    }
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.exposure      { options.plate_solver.exposure = v; }
                if let Some(v) = patch.gain          { options.plate_solver.gain = v; }
                if let Some(v) = patch.bin           { options.plate_solver.bin = v; }
                if let Some(v) = patch.solver        { options.plate_solver.solver = v; }
                if let Some(v) = patch.timeout       { options.plate_solver.timeout = v; }
                if let Some(v) = patch.blind_timeout { options.plate_solver.blind_timeout = v; }
                Ok(Reply::Empty)
            }
            Command::SetPolarAlignOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.angle        { options.polar_align.angle = v; }
                if let Some(v) = patch.direction    { options.polar_align.direction = v; }
                if let Some(v) = patch.speed        { options.polar_align.speed = v; }
                if let Some(v) = patch.auto_refresh { options.polar_align.auto_refresh = v; }
                if let Some(v) = patch.sim_alt_err  { options.polar_align.sim_alt_err = v; }
                if let Some(v) = patch.sim_az_err   { options.polar_align.sim_az_err = v; }
                Ok(Reply::Empty)
            }
            Command::SetGuidingOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.calibr_exposure {
                    options.guiding.main_cam.calibr_exposure = v;
                }
                if let Some(v) = patch.calibr_gain {
                    options.guiding.main_cam.calibr_gain = v;
                }
                if let Some(v) = patch.mode {
                    options.guiding.mode = v;
                }
                if let Some(v) = patch.dith_period {
                    options.guiding.dith_period = v;
                }
                if let Some(v) = patch.ext_dith_dist {
                    options.guiding.ext_guider.dith_dist = v;
                }
                if let Some(v) = patch.main_cam_dith_dist {
                    options.guiding.main_cam.dith_dist = v;
                }
                if let Some(v) = patch.main_cam_max_error {
                    options.guiding.main_cam.max_error = v;
                }
                Ok(Reply::Empty)
            }
            Command::SetIndiSelection(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.mount     { options.indi.mount = v; }
                if let Some(v) = patch.camera    { options.indi.camera = v; }
                if let Some(v) = patch.guid_cam  { options.indi.guid_cam = v; }
                if let Some(v) = patch.focuser   { options.indi.focuser = v; }
                if let Some(v) = patch.flt_wheel { options.indi.flt_wheel = v; }
                if let Some(v) = patch.aux1      { options.indi.aux1 = v; }
                if let Some(v) = patch.aux2      { options.indi.aux2 = v; }
                if let Some(v) = patch.remote    { options.indi.remote = v; }
                if let Some(v) = patch.address   { options.indi.address = v; }
                Ok(Reply::Empty)
            }
            Command::SetAlpacaAddress { address } => {
                self.options.write().unwrap().ascom_alpaca.address = address;
                Ok(Reply::Empty)
            }
            Command::SetSiteLatitude { sexagesimal } => {
                let v = ::indi::sexagesimal_to_value(&sexagesimal)
                    .ok_or_else(|| eyre::eyre!("Unparseable latitude: {sexagesimal:?}"))?;
                self.options.write().unwrap().site.latitude = v;
                Ok(Reply::Empty)
            }
            Command::SetSiteLongitude { sexagesimal } => {
                let v = ::indi::sexagesimal_to_value(&sexagesimal)
                    .ok_or_else(|| eyre::eyre!("Unparseable longitude: {sexagesimal:?}"))?;
                self.options.write().unwrap().site.longitude = v;
                Ok(Reply::Empty)
            }
            Command::SetTelescopeFromPlateSolve { enabled } => {
                self.options.write().unwrap().telescope.from_platesolve = enabled;
                Ok(Reply::Empty)
            }
            Command::SetMountOptions(patch) => {
                let mut options = self.options.write().unwrap();
                if let Some(v) = patch.inv_ns  { options.mount.inv_ns = v; }
                if let Some(v) = patch.inv_we  { options.mount.inv_we = v; }
                if let Some(v) = patch.speed   { options.mount.speed = v; }
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
                        // Best-effort: a per-device failure must not leave the
                        // remaining devices unset (e.g. half-applied CONFIG_SAVE)
                        let mut errors = Vec::new();
                        for device in indi.get_devices_list() {
                            if let Err(err) = indi.command_set_switch_property(
                                &device.name, &prop, &[(&element, true)]
                            ) {
                                errors.push(format!("{}: {err:#}", device.name));
                            }
                        }
                        if !errors.is_empty() {
                            eyre::bail!("Set switch on all devices failed: [{}]",
                                errors.join("; "));
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
                        let pos = focuser.abs_position()?;
                        let range = focuser.abs_position_range()?;
                        let target = focuser_move_by_target(
                            pos, offset, *range.start(), *range.end())?;
                        match target {
                            None => Ok(Reply::FocuserMove { new_position: None }),
                            Some(target) => {
                                focuser.set_abs_position(target)?;
                                Ok(Reply::FocuserMove { new_position: Some(target) })
                            }
                        }
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
            Command::BuildDarkLibraryProgram { spec } => {
                let mode = spec.mode();
                // `is_cooler_supported()` is resolved before taking the `options` lock
                // (lock order `modes -> options`: no `modes` lock here at all)
                let camera = self.cur_devices.camera_or_err()?;
                let cooler_supported = camera.is_cooler_supported()?;
                let cam_opts = self.options.read().unwrap().cam.clone();
                let program = build_program(&spec, &cam_opts, cooler_supported);
                let total_frames = program.iter().map(|item| item.count).sum();
                let total_duration_sec = program.iter()
                    .map(|item| item.count as f64 * item.exposure)
                    .sum();
                Ok(Reply::DarkLibraryProgram {
                    mode,
                    program,
                    total_frames,
                    total_duration_sec,
                })
            }
            Command::GetSnapshot => Ok(Reply::Snapshot(Box::new(self.snapshot()))),
            Command::Query(query) => {
                let reply = match query {
                    // `DeviceType::empty()` passes every device type (`contains` semantics)
                    Query::Devices =>
                        QueryReply::Devices(self.hal.devices(DeviceType::empty())?),
                    Query::CameraCaps => {
                        let camera = self.cur_devices.camera_or_err()?;
                        QueryReply::CameraCaps(CameraCaps::of(&*camera))
                    }
                    Query::IndiProperties { since } => {
                        let indi = self.hal.indi_impl().indi();
                        let (devices, properties) = indi.get_properties_list(since);
                        // Sort keys (pixel size / max frame) are read here for the panel; non-CCD devices yield zeros.
                        let devices = devices
                            .into_iter()
                            .map(|dev| IndiDeviceEntry {
                                pixel_size_um: indi.camera_get_pixel_size_um(&dev.name, indi::CamCcd::Main).unwrap_or_default(),
                                max_frame_size: indi.camera_get_max_frame_size(&dev.name, indi::CamCcd::Main)
                                    .map(|(w, h)| (w as u32, h as u32))
                                    .unwrap_or_default(),
                                device: dev,
                            })
                            .collect();
                        let properties = properties
                            .into_iter()
                            .map(truncate_blob_payload)
                            .collect();
                        QueryReply::IndiProperties(IndiPropertiesData { devices, properties })
                    }
                    Query::IndiDrivers => QueryReply::IndiDrivers(self.hal.indi_impl().drivers().clone()),
                    Query::AllMountSites => {
                        let mut sites = Vec::new();
                        for dev in self.hal.devices(DeviceType::TELESCOPE)? {
                            if let Ok(telescope) = self.hal.telescope(&dev.id)
                                && let Ok(site) = telescope.site() {
                                    sites.push(MountSite { id: dev.id, name: telescope.name().to_string(), site });
                                }
                        }
                        QueryReply::AllMountSites(sites)
                    }
                    Query::PolarAlignCheck => QueryReply::PolarAlignCheck(
                        PolarAlignMode::check_before_start(&self.hal, &self.options)?
                    ),
                };
                Ok(Reply::Query(Box::new(reply)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::core::mode_darks_lib::{
        DefectPixelsOptions, FramesCountMode, MasterBiasesOptions,
        MasterDarksOptions, ValuesItem,
    };

    use crate::core::engine::ModeKind;

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
            Command::Shutdown { force: true },
            Command::Shutdown { force: false },
            Command::SaveOptions,
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
            Command::SetCooling(CoolingPatch { enable_cooler: Some(true), temperature: Some(-10.5) }),
            Command::SetCooling(CoolingPatch { temperature: Some(-5.0), ..Default::default() }),
            Command::SetFan { enable_fan: false },
            Command::SetHeater { heater_str: Some("auto".to_string()) },
            Command::SetHeater { heater_str: None },
            Command::SetCamReadoutOptions(CamReadoutPatch {
                conv_gain_str: Some(Some("x1".to_string())),
                low_noise: Some(true), high_fullwell: Some(false),
            }),
            Command::SetCamReadoutOptions(CamReadoutPatch {
                conv_gain_str: Some(None), ..Default::default()
            }),
            Command::SetCamReadoutOptions(CamReadoutPatch::default()),
            Command::SetFrameType { frame_type: FrameType::Flats },
            Command::SetAutoExpEnabled { enabled: true },
            Command::SetExposure { exposure: 3.25 },
            Command::SetGain { gain: 100.0 },
            Command::SetOffset { offset: -10 },
            Command::SetBinning { binning: Binning::Bin2 },
            Command::SetCrop { crop: Crop::P50 },
            Command::SetRawFramesCount { frame_cnt: 10 },
            Command::SetLiveViewEnabled { enabled: true },
            Command::SetLiveStackingOptions(LiveStackingPatch {
                remove_tracks: Some(true), frame_cnt: Some(42), use_cnt: Some(true),
                save_orig: Some(true), save_enabled: Some(false), save_minutes: Some(5),
                out_dir: Some("/tmp/live".to_string()),
            }),
            Command::SetRawFramesOptions(RawFramesPatch {
                use_cnt: Some(false), out_path: Some("/tmp/raw".to_string()),
                create_master: Some(true),
            }),
            Command::SetQualityOptions(QualityPatch {
                use_max_fwhm: Some(true), max_fwhm: Some(4.5), use_max_ovality: Some(false),
                max_ovality: Some(2.25), ignore_3px_stars: Some(true),
                star_recogn_sens: Some(StarRecognSensitivity::High),
                check_ccd_temp: Some(false), max_ccd_temp_diff: Some(1.5),
            }),
            Command::SetQualityOptions(QualityPatch {
                max_fwhm: Some(3.75), ..Default::default()
            }),
            Command::SetCalibrOptions(CalibrPatch {
                dark_frame_en: Some(true), flat_frame_en: Some(false),
                flat_frame_fname: Some("/tmp/flat.fits".to_string()), hot_pixels: Some(true),
            }),
            Command::SetCalibrOptions(CalibrPatch {
                flat_frame_fname: Some("/tmp/flat2.fits".to_string()), ..Default::default()
            }),
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
            Command::Preview(PreviewCommand::SetWidgetSize { width: 800, height: 600 }),
            Command::SetFocuserOptions(FocuserPatch {
                exposure: Some(2.5), gain: Some(Gain::P50), on_temp_change: Some(true),
                max_temp_change: Some(1.5), on_fwhm_change: Some(false),
                max_fwhm_change: Some(20), periodically: Some(true),
                period_minutes: Some(120), measures: Some(11), step: Some(50.0),
                anti_backlash_steps: Some(500),
            }),
            Command::SetFocuserOptions(FocuserPatch { gain: Some(Gain::P50), ..Default::default() }),
            Command::SetPlateSolverOptions(PlateSolverPatch {
                exposure: Some(5.0), gain: Some(Gain::Same), bin: Some(Binning::Bin2),
                solver: Some(PlateSolverType::Astrometry),
                timeout: Some(10), blind_timeout: Some(30),
            }),
            Command::SetPolarAlignOptions(PolarAlignPatch {
                angle: Some(30.0), direction: Some(PolarAlignDir::East),
                speed: Some(Some("2".to_string())), auto_refresh: Some(false),
                sim_alt_err: Some(1.25), sim_az_err: Some(-3.5),
            }),
            Command::SetPolarAlignOptions(PolarAlignPatch { speed: Some(None), ..Default::default() }),
            Command::SetGuidingOptions(GuidingPatch {
                calibr_exposure: Some(3.5), calibr_gain: Some(Gain::Max),
                mode: Some(GuidingMode::MainCamera), dith_period: Some(2),
                ext_dith_dist: Some(10), main_cam_dith_dist: Some(50),
                main_cam_max_error: Some(3.0),
            }),
            Command::OpenImageFile { path: "/tmp/test.fits".to_string() },
            Command::BuildDarkLibraryProgram {
                spec: DarkLibSpec::DefectPixels(DefectPixelsOptions {
                    frm_cnt_mode: FramesCountMode::Count, frames_count: 10,
                    ..Default::default()
                }),
            },
            Command::BuildDarkLibraryProgram {
                spec: DarkLibSpec::MasterDarks(MasterDarksOptions {
                    temperature: ValuesItem { used: true, values: vec![-10.0, -5.0] },
                    ..Default::default()
                }),
            },
            Command::BuildDarkLibraryProgram {
                spec: DarkLibSpec::MasterBiases(MasterBiasesOptions::default()),
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
            Reply::DarkLibraryProgram {
                mode: DarkLibMode::MasterBias,
                program: vec![MasterFileCreationProgramItem {
                    count: 60, temperature: Some(-10.0), exposure: 0.0001,
                    gain: 100.0, offset: 10, binning: Binning::Orig, crop: Crop::None,
                }],
                total_frames: 60,
                total_duration_sec: 0.006,
            },
            Reply::DarkLibraryProgram {
                mode: DarkLibMode::MasterDark,
                program: Vec::new(),
                total_frames: 0,
                total_duration_sec: 0.0,
            },
        ] {
            roundtrip(reply);
        }
    }

    #[test]
    fn indi_properties_truncate_blob_payload() {
        let prop = indi::Property {
            device:     Arc::new("ccd".to_string()),
            name:       Arc::new("CCD_FRAME".to_string()),
            type_:      indi::PropType::Blob,
            label:      None,
            group:      None,
            permission: indi::PropPermission::RO,
            state:      indi::PropState::Ok,
            timeout:    None,
            timestamp:  None,
            message:    None,
            elements:   vec![indi::PropElement {
                name:  Arc::new("BINARY".to_string()),
                label: None,
                value: indi::PropValue::Blob(Arc::new(indi::BlobPropValue {
                    format:   ".fits".to_string(),
                    data:     vec![1, 2, 3, 4],
                    data_len: 4,
                    dl_time:  0.5,
                })),
            }],
            change_id:  1,
        };
        let out = truncate_blob_payload(prop);
        let indi::PropValue::Blob(blob) = &out.elements[0].value else {
            panic!("expected a blob value")
        };
        assert!(blob.data.is_empty());
        assert_eq!(blob.data_len, 4);
        assert_eq!(blob.format, ".fits");
        assert_eq!(blob.dl_time, 0.5);
    }

    #[test]
    fn polar_align_get_state_is_read_only() {
        assert!(is_read_only(&Command::Mode(
            ModeCommand::PolarAlign(PolarAlignCommand::GetState))));
        assert!(!is_read_only(&Command::Mode(
            ModeCommand::PolarAlign(PolarAlignCommand::Restart))));
    }

    #[test]
    fn device_selection_is_connect_gated() {
        let engine = Engine::new();
        let guard = engine.lock_device_busy(BusyDomain::Connect).unwrap();
        let err = engine.exec_command(Command::SetCameraDevice {
            device_id: "cam1".to_string()
        }).unwrap_err();
        assert!(format!("{err:#}").contains("busy"), "unexpected error: {err:#}");
        drop(guard);
    }

    #[test]
    fn focuser_move_by_target_clamps_and_errors() {
        // Normal moves inside the range
        assert_eq!(focuser_move_by_target(100.0, 20.0, 0.0, 1000.0).unwrap(),
            Some(120.0));
        assert_eq!(focuser_move_by_target(100.0, -20.0, 0.0, 1000.0).unwrap(),
            Some(80.0));
        // Clamped to the boundary and driven there
        assert_eq!(focuser_move_by_target(990.0, 50.0, 0.0, 1000.0).unwrap(),
            Some(1000.0));
        assert_eq!(focuser_move_by_target(-10.0, -5.0, -5.0, 1000.0).unwrap(),
            Some(-5.0));
        // Already at the boundary in the requested direction -> skip
        assert_eq!(focuser_move_by_target(1000.0, 50.0, 0.0, 1000.0).unwrap(), None);
        assert_eq!(focuser_move_by_target(0.0, -50.0, 0.0, 1000.0).unwrap(), None);
        // Bad device state must be an error, never a `f64::clamp` panic
        assert!(focuser_move_by_target(f64::NAN, 10.0, 0.0, 1000.0).is_err());
        assert!(focuser_move_by_target(100.0, 10.0, 500.0, 100.0).is_err());
        assert!(focuser_move_by_target(100.0, f64::NAN, 0.0, 1000.0).is_err());
    }

    #[test]
    fn patch_serde_semantics() {
        // Field absent from the payload means "unchanged".
        let p: CamReadoutPatch = serde_json::from_str(r#"{"low_noise": true}"#).unwrap();
        assert_eq!(p.conv_gain_str, None);
        assert_eq!(p.low_noise, Some(true));
        assert_eq!(p.high_fullwell, None);
        // JSON `null` means "set to None".
        let p: CamReadoutPatch = serde_json::from_str(r#"{"conv_gain_str": null}"#).unwrap();
        assert_eq!(p.conv_gain_str, Some(None));
        // A present value means "set to Some(value)".
        let p: CamReadoutPatch = serde_json::from_str(r#"{"conv_gain_str": "x2"}"#).unwrap();
        assert_eq!(p.conv_gain_str, Some(Some("x2".to_string())));
        // Only the double-Option field skips serialization when untouched;
        // plain fields serialize as `null`, which deserializes back to "unchanged".
        let p = CamReadoutPatch { low_noise: Some(true), ..Default::default() };
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"low_noise":true,"high_fullwell":null}"#);
        // Container `#[serde(default)]` keeps plain fields optional on the wire too.
        let p: QualityPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(p.max_fwhm, None);
        let p: QualityPatch =
            serde_json::from_str(r#"{"use_max_fwhm": true, "max_fwhm": null}"#).unwrap();
        assert_eq!(p.use_max_fwhm, Some(true));
        assert_eq!(p.max_fwhm, None);
        // Empty patch roundtrips to an empty patch.
        let p = QualityPatch::default();
        roundtrip(p);
        // Container `#[serde(default)]` also applies to the new patches.
        let p: LiveStackingPatch = serde_json::from_str(r#"{"out_dir": "/x"}"#).unwrap();
        assert_eq!(p.out_dir.as_deref(), Some("/x"));
        assert_eq!(p.save_minutes, None);
        let p: RawFramesPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(p.use_cnt, None);
        // `null` clears the optional mount speed, a value sets it
        let p: MountPatch = serde_json::from_str(r#"{"speed": null}"#).unwrap();
        assert_eq!(p.speed, Some(None));
        let p: MountPatch = serde_json::from_str(r#"{"speed": "1"}"#).unwrap();
        assert_eq!(p.speed, Some(Some("1".to_string())));
    }

    #[test]
    fn set_live_stacking_options_validation() {
        let engine = Engine::new();

        let bad = Command::SetLiveStackingOptions(LiveStackingPatch {
            save_minutes: Some(0),
            ..Default::default()
        });
        assert!(engine.exec_command(bad).is_err());
        // Rejected patch must leave the default untouched
        assert_eq!(engine.options.read().unwrap().live.save_minutes, 5);

        let good = Command::SetLiveStackingOptions(LiveStackingPatch {
            save_minutes: Some(7),
            use_cnt:      Some(true),
            out_dir:      Some("/tmp/live_out".to_string()),
            ..Default::default()
        });
        assert!(engine.exec_command(good).is_ok());
        let options = engine.options.read().unwrap();
        assert_eq!(options.live.save_minutes, 7);
        assert!(options.live.use_cnt);
        assert_eq!(options.live.out_dir, PathBuf::from("/tmp/live_out"));
    }

    #[test]
    fn site_and_indi_selection_commands() {
        let engine = Engine::new();

        // Unparseable sexagesimal text is an error; the old value is kept
        assert!(engine.exec_command(Command::SetSiteLatitude {
            sexagesimal: "nonsense".to_string()
        }).is_err());
        assert_eq!(engine.options.read().unwrap().site.latitude, 0.0);
        assert!(engine.exec_command(Command::SetSiteLongitude {
            sexagesimal: "-10:30".to_string()
        }).is_ok());
        assert_eq!(engine.options.read().unwrap().site.longitude, -10.5);

        // JSON `null` deselects a driver, a value selects it
        let p: IndiSelPatch = serde_json::from_str(r#"{"mount": null}"#).unwrap();
        assert_eq!(p.mount, Some(None));
        assert!(engine.exec_command(Command::SetIndiSelection(IndiSelPatch {
            mount: Some(Some("indi_simulator_mount".to_string())),
            ..Default::default()
        })).is_ok());
        assert_eq!(
            engine.options.read().unwrap().indi.mount.as_deref(),
            Some("indi_simulator_mount")
        );
        assert!(engine.exec_command(Command::SetIndiSelection(IndiSelPatch {
            mount: Some(None),
            ..Default::default()
        })).is_ok());
        assert_eq!(engine.options.read().unwrap().indi.mount, None);
    }

    #[test]
    fn plate_solver_timeout_validation() {
        let engine = Engine::new();
        assert!(engine.exec_command(Command::SetPlateSolverOptions(PlateSolverPatch {
            timeout: Some(0),
            ..Default::default()
        })).is_err());
        assert_eq!(engine.options.read().unwrap().plate_solver.timeout, 10);
        assert!(engine.exec_command(Command::SetPlateSolverOptions(PlateSolverPatch {
            blind_timeout: Some(0),
            ..Default::default()
        })).is_err());
        assert_eq!(engine.options.read().unwrap().plate_solver.blind_timeout, 30);
        assert!(engine.exec_command(Command::SetPlateSolverOptions(PlateSolverPatch {
            timeout: Some(15),
            ..Default::default()
        })).is_ok());
        assert_eq!(engine.options.read().unwrap().plate_solver.timeout, 15);
    }

    fn snapshot_of(engine: &Arc<Engine>) -> StateSnapshot {
        match engine.exec_command(Command::GetSnapshot).unwrap() {
            Reply::Snapshot(snap) => *snap,
            _ => panic!("expected Reply::Snapshot"),
        }
    }

    #[test]
    fn snapshot_roundtrip_seq_and_mode() {
        let engine = Engine::new();

        let snap = snapshot_of(&engine);
        assert_eq!(snap.mode.kind, ModeKind::Waiting);

        // UiPermissions must be consistent with the Waiting mode
        let perms = &snap.permissions;
        assert!(perms.waiting && perms.can_change_mode);
        assert!(perms.can_change_cam_opts);
        assert!(perms.can_change_frame_opts);
        assert!(perms.can_change_live_stacking_opts);
        assert!(perms.can_change_cal_ops);
        assert!(!perms.single_shot && !perms.liveview_active);
        assert!(!perms.saving_frames && !perms.live_active);
        assert!(!perms.saving_frames_paused && !perms.live_stacking_paused);
        assert!(!perms.focusing && !perms.polar_align && !perms.dither_calibr);

        // Full state survives a JSON round-trip
        let json = serde_json::to_string(&snap).unwrap();
        let back: StateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);

        let seq0 = snap.seq;

        // A mutating command that sends an event advances seq
        assert!(engine.exec_command(Command::SetExposure { exposure: 2.5 }).is_ok());
        let seq1 = snapshot_of(&engine).seq;
        assert!(seq1 > seq0, "seq must grow after an evented mutation");

        // A mutating command that sends NO event also advances seq
        // (seq is not event-coupled)
        assert!(engine.exec_command(Command::SetCamReadoutOptions(CamReadoutPatch {
            low_noise: Some(true),
            ..Default::default()
        })).is_ok());
        let seq2 = snapshot_of(&engine).seq;
        assert!(seq2 > seq1, "seq must grow after an eventless mutation");

        // Polling must not look like a change
        assert_eq!(snapshot_of(&engine).seq, seq2);

        engine.stop();
    }

    #[test]
    fn preview_color_serialization_and_old_config_default() {
        use crate::options::Options;

        let mut options = Options::default();
        options.preview.color = PreviewColorMode::Blue;
        let json = serde_json::to_string(&options).unwrap();
        assert!(json.contains(r#""color":"Blue""#), "color must be serialized");

        // Old configs lack the field: it must fall back to the `Rgb` default
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["preview"].as_object_mut().unwrap().remove("color");
        let back: Options = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(back.preview.color, PreviewColorMode::Rgb);
    }

    #[test]
    fn query_devices_and_camera_caps() {
        let engine = Engine::new();

        // INDI not connected: empty device list, not an error
        match engine.exec_command(Command::Query(Query::Devices)).unwrap() {
            Reply::Query(q) => match *q {
                QueryReply::Devices(devices) => assert!(devices.is_empty()),
                _ => panic!("expected QueryReply::Devices"),
            },
            _ => panic!("expected Reply::Query"),
        }

        // No camera: `CameraCaps` fails explicitly
        assert!(engine.exec_command(Command::Query(Query::CameraCaps)).is_err());

        // INDI not connected: empty device/property lists, not an error
        match engine.exec_command(Command::Query(Query::IndiProperties { since: None })).unwrap() {
            Reply::Query(q) => match *q {
                QueryReply::IndiProperties(data) => {
                    assert!(data.devices.is_empty());
                    assert!(data.properties.is_empty());
                }
                _ => panic!("expected QueryReply::IndiProperties"),
            },
            _ => panic!("expected Reply::Query"),
        }

        // No telescopes: empty site list, not an error
        match engine.exec_command(Command::Query(Query::AllMountSites)).unwrap() {
            Reply::Query(q) => match *q {
                QueryReply::AllMountSites(sites) => assert!(sites.is_empty()),
                _ => panic!("expected QueryReply::AllMountSites"),
            },
            _ => panic!("expected Reply::Query"),
        }

        // Driver registry read must not fail even with INDI not connected
        // (content depends on the local indiserver, so not asserted)
        match engine.exec_command(Command::Query(Query::IndiDrivers)).unwrap() {
            Reply::Query(q) => match *q {
                QueryReply::IndiDrivers(_) => {}
                _ => panic!("expected QueryReply::IndiDrivers"),
            },
            _ => panic!("expected Reply::Query"),
        }

        engine.stop();
    }

    #[test]
    fn query_variants_serde_roundtrip() {
        roundtrip(Command::GetSnapshot);
        roundtrip(Command::Query(Query::Devices));
        roundtrip(Command::Query(Query::CameraCaps));
        roundtrip(Command::Query(Query::IndiProperties { since: Some(7) }));
        roundtrip(Command::Query(Query::IndiDrivers));
        roundtrip(Command::Query(Query::AllMountSites));
        roundtrip(Command::Query(Query::PolarAlignCheck));
        roundtrip(QueryReply::PolarAlignCheck("warn".to_string()));
        roundtrip(QueryReply::AllMountSites(vec![MountSite {
            id:   "indi_sim_mount".to_string(),
            name: "INDI Simulator Mount".to_string(),
            site: TelescopeSite { latitude: 55.7, longitude: 37.6, elevation: 150.0 },
        }]));
        roundtrip(QueryReply::IndiDrivers(indi::Drivers { groups: Vec::new() }));
        roundtrip(QueryReply::IndiProperties(IndiPropertiesData { devices: Vec::new(), properties: Vec::new() }));
        roundtrip(QueryReply::Devices(vec![DeviceInfo {
            id:    "indi_simulator_ccd".to_string(),
            name:  "indi_simulator_ccd".to_string(),
            type_: DeviceType::CAMERA,
        }]));
        roundtrip(QueryReply::CameraCaps(CameraCaps {
            name:                      "ZWO CCD".to_string(),
            ccd_size:                  (4656, 3520),
            max_binning:               4,
            heater_ctrl_list:          vec![("h1".to_string(), "Heater 1".to_string())],
            conversion_gain_list:      vec![("g1".to_string(), "Gain 1".to_string())],
            exposure_range:            Some(FloatRange { start: 0.00003, end: 1800.0 }),
            cooler_supported:          true,
            temperature_range:         Some(FloatRange { start: -35.0, end: 15.0 }),
            gain_supported:            true,
            gain_range:                Some(FloatRange { start: 0.0, end: 400.0 }),
            offset_supported:          false,
            offset_range:              None,
            frame_supported:           true,
            binning_supported:         true,
            fan_ctrl_supported:        false,
            heater_supported:          false,
            low_noise_supported:       false,
            conversion_gain_supported: false,
            high_fullwell_supported:   false,
        }));
    }
}
