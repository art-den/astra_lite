use std::{collections::VecDeque, sync::{Arc, Mutex}};

use serde::{Deserialize, Serialize};

use crate::{
    core::{ mode_camera::{CameraMode, TakingPicturesMode}, mode_waiting::WaitingMode, raw_calibration::RawCalibration}, hal::Camera, options::*
};

use super::{engine::*, events::Progress};

const WAIT_TEMPERATURE_TIME: usize = 20; // seconds

enum State {
    Undefined,
    WaitingForTemperature(f64),
    WaitingForDarkCreation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum DarkLibMode {
    DefectPixels,
    MasterDark,
    MasterBias,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MasterFileCreationProgramItem {
    pub count:       usize,
    pub temperature: Option<f64>,
    pub exposure:    f64,
    pub gain:        f64,
    pub offset:      i32,
    pub binning:     Binning,
    pub crop:        Crop,
}

/// Dark library build request spectrum: one variant per dark library tab.
/// Serialized field names must stay intact (persisted in the `ui_darks_lib`
/// config as the `UiOptions` fields of the UI panel).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum DarkLibSpec {
    DefectPixels(DefectPixelsOptions),
    MasterDarks(MasterDarksOptions),
    MasterBiases(MasterBiasesOptions),
}

impl DarkLibSpec {
    pub fn mode(&self) -> DarkLibMode {
        match self {
            Self::DefectPixels(_) => DarkLibMode::DefectPixels,
            Self::MasterDarks(_)  => DarkLibMode::MasterDark,
            Self::MasterBiases(_) => DarkLibMode::MasterBias,
        }
    }
}

#[derive(Serialize, Default, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum FramesCountMode {
    Count,
    #[default]
    Time,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct BinningOptions {
    pub used:   bool,
    pub bin1x1: bool,
    pub bin2x2: bool,
    pub bin4x4: bool,
}

impl Default for BinningOptions {
    fn default() -> Self {
        Self {
            used:   false,
            bin1x1: true,
            bin2x2: false,
            bin4x4: false
        }
    }
}

impl BinningOptions {
    fn get_binnings(&self) -> Vec<Binning> {
        let mut result = Vec::new();
        if self.used {
            if self.bin1x1 { result.push(Binning::Orig); }
            if self.bin2x2 { result.push(Binning::Bin2); }
            if self.bin4x4 { result.push(Binning::Bin4); }
        }
        result
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct CropOptions {
    pub used:    bool,
    pub crop100: bool,
    pub crop75:  bool,
    pub crop50:  bool,
    pub crop33:  bool,
    pub crop25:  bool,
}

impl Default for CropOptions {
    fn default() -> Self {
        Self {
            used:    false,
            crop100: true,
            crop75:  false,
            crop50:  false,
            crop33:  false,
            crop25:  false
        }
    }
}

impl CropOptions {
    fn get_crops(&self) -> Vec<Crop> {
        let mut result = Vec::new();
        if self.used {
            if self.crop100 { result.push(Crop::None); }
            if self.crop75 { result.push(Crop::P75); }
            if self.crop50 { result.push(Crop::P50); }
            if self.crop33 { result.push(Crop::P33); }
            if self.crop25 { result.push(Crop::P25); }
        }
        result
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct DefectPixelsOptions {
    pub frm_cnt_mode:     FramesCountMode,
    pub frames_count:     usize,
    pub integr_time:      f64, // minutes
    pub temperature_used: bool,
    pub temperature:      f64,
    pub exposure_used:    bool,
    pub exposure:         f64,
    pub gain_used:        bool,
    pub gain:             f64,
    pub offset_used:      bool,
    pub offset:           f64,
    pub binning:          BinningOptions,
    pub crop:             CropOptions,
}

impl Default for DefectPixelsOptions {
    fn default() -> Self {
        Self {
            frm_cnt_mode:     FramesCountMode::default(),
            frames_count:     18,
            integr_time:      15.0,
            temperature_used: false,
            temperature:      5.0,
            exposure_used:    false,
            exposure:         30.0,
            gain_used:        false,
            gain:             100.0,
            offset_used:      false,
            offset:           100.0,
            binning:          BinningOptions::default(),
            crop:             CropOptions::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct ValuesItem {
    pub used:   bool,
    pub values: Vec<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct MasterDarksOptions {
    pub frm_cnt_mode: FramesCountMode,
    pub frames_count: usize,
    pub integr_time:  f64, // minutes
    pub temperature:  ValuesItem,
    pub exposure:     ValuesItem,
    pub gain:         ValuesItem,
    pub offset:       ValuesItem,
    pub binning:      BinningOptions,
    pub crop:         CropOptions,
    pub min_count_en: bool,
    pub min_count:    usize,
}

impl Default for MasterDarksOptions {
    fn default() -> Self {
        Self {
            frm_cnt_mode: FramesCountMode::default(),
            frames_count: 30,
            integr_time:  60.0,
            temperature:  ValuesItem::default(),
            exposure:     ValuesItem::default(),
            gain:         ValuesItem::default(),
            offset:       ValuesItem::default(),
            binning:      BinningOptions::default(),
            crop:         CropOptions::default(),
            min_count_en: true,
            min_count:    60,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct MasterBiasesOptions {
    pub frames_count: usize,
    pub temperature:  ValuesItem,
    pub exposure:     f64,
    pub gain:         ValuesItem,
    pub offset:       ValuesItem,
    pub binning:      BinningOptions,
    pub crop:         CropOptions,
}

impl Default for MasterBiasesOptions {
    fn default() -> Self {
        Self {
            frames_count: 60,
            temperature:  ValuesItem::default(),
            exposure:     0.0001,
            gain:         ValuesItem::default(),
            offset:       ValuesItem::default(),
            binning:      BinningOptions::default(),
            crop:         CropOptions::default(),
        }
    }
}

/// Round up to the next multiple of 5; saturates instead of overflowing on
/// absurd values (hand-edited config / web input, `panic = "abort"`).
pub(crate) fn multiple_of_5(v: usize) -> usize {
    usize::saturating_add(v, 4) / 5 * 5
}

/// Values list when the item is used and non-empty, otherwise the default.
fn get_values(item: &ValuesItem, default: f64) -> Vec<f64> {
    if item.used && !item.values.is_empty() {
        item.values.clone()
    } else {
        vec![default]
    }
}

/// Temperature list resolution: cooler off -> `[None]`; own values -> spec
/// values -> camera ctrl temperature (when the cooler is enabled) -> `[None]`.
fn resolve_temperatures(
    values:           &ValuesItem,
    cam_opts:         &CamOptions,
    cooler_supported: bool,
) -> Vec<Option<f64>> {
    if cooler_supported {
        if values.used && !values.values.is_empty() {
            return values.values.iter().copied().map(Some).collect();
        }
        if cam_opts.ctrl.enable_cooler {
            return vec![Some(cam_opts.ctrl.temperature)];
        }
    }
    vec![None]
}

/// Scalar variant of [`resolve_temperatures`] (defect pixels tab).
fn resolve_temperature(
    used:             bool,
    temperature:      f64,
    cam_opts:         &CamOptions,
    cooler_supported: bool,
) -> Option<f64> {
    if !cooler_supported {
        return None;
    }
    if used {
        return Some(temperature);
    }
    if cam_opts.ctrl.enable_cooler {
        return Some(cam_opts.ctrl.temperature);
    }
    None
}

fn resolve_binnings(opts: &BinningOptions, cam_opts: &CamOptions) -> Vec<Binning> {
    let mut binnings = opts.get_binnings();
    if binnings.is_empty() {
        binnings.push(cam_opts.frame.binning);
    }
    binnings
}

fn resolve_crops(opts: &CropOptions, cam_opts: &CamOptions) -> Vec<Crop> {
    let mut crops = opts.get_crops();
    if crops.is_empty() {
        crops.push(cam_opts.frame.crop);
    }
    crops
}

/// Builds the master file creation program for a dark library spec.
/// `cooler_supported` is the camera capability (the caller resolves it).
/// All frame counts are normalized to multiples of 5 here.
pub fn build_program(
    spec:             &DarkLibSpec,
    cam_opts:         &CamOptions,
    cooler_supported: bool,
) -> Vec<MasterFileCreationProgramItem> {
    match spec {
        DarkLibSpec::DefectPixels(opts) =>
            build_defect_pixels_program(opts, cam_opts, cooler_supported),
        DarkLibSpec::MasterDarks(opts) =>
            build_master_darks_program(opts, cam_opts, cooler_supported),
        DarkLibSpec::MasterBiases(opts) =>
            build_master_biases_program(opts, cam_opts, cooler_supported),
    }
}

fn build_defect_pixels_program(
    opts:             &DefectPixelsOptions,
    cam_opts:         &CamOptions,
    cooler_supported: bool,
) -> Vec<MasterFileCreationProgramItem> {
    let mut result = Vec::new();

    let binnings = resolve_binnings(&opts.binning, cam_opts);
    let crops = resolve_crops(&opts.crop, cam_opts);

    let temperature = resolve_temperature(
        opts.temperature_used, opts.temperature, cam_opts, cooler_supported);

    let exposure = if opts.exposure_used { opts.exposure } else { cam_opts.frame.exp_main };
    let gain = if opts.gain_used { opts.gain } else { cam_opts.frame.gain };
    let offset = if opts.offset_used { opts.offset as i32 } else { cam_opts.frame.offset };

    for bin in &binnings {
        for crop in &crops {
            let frame_count = match opts.frm_cnt_mode {
                FramesCountMode::Count => multiple_of_5(opts.frames_count),
                FramesCountMode::Time => {
                    if !f64::is_finite(exposure) || exposure <= 0.0
                        || !f64::is_finite(opts.integr_time) {
                        continue;
                    }
                    multiple_of_5((60.0 * opts.integr_time / exposure) as usize)
                }
            };
            if frame_count == 0 { continue; }
            result.push(MasterFileCreationProgramItem {
                count:       frame_count,
                temperature,
                exposure,
                gain,
                offset,
                binning:     *bin,
                crop:        *crop,
            });
        }
    }

    result
}

fn build_master_darks_program(
    opts:             &MasterDarksOptions,
    cam_opts:         &CamOptions,
    cooler_supported: bool,
) -> Vec<MasterFileCreationProgramItem> {
    let mut result = Vec::new();

    let temperatures = resolve_temperatures(&opts.temperature, cam_opts, cooler_supported);

    let exposures = get_values(&opts.exposure, cam_opts.frame.exp_main);
    let gains = get_values(&opts.gain, cam_opts.frame.gain);
    let offsets = get_values(&opts.offset, cam_opts.frame.offset as f64);
    let binnings = resolve_binnings(&opts.binning, cam_opts);
    let crops = resolve_crops(&opts.crop, cam_opts);

    for t in &temperatures {
        for bin in &binnings {
            for crop in &crops {
                for exp in &exposures {
                    for gain in &gains {
                        for offset in &offsets {
                            let mut frame_count = match opts.frm_cnt_mode {
                                FramesCountMode::Count => opts.frames_count,
                                FramesCountMode::Time => {
                                    if !f64::is_finite(*exp) || *exp <= 0.0
                                        || !f64::is_finite(opts.integr_time) {
                                        continue;
                                    }
                                    (60.0 * opts.integr_time / *exp) as usize
                                }
                            };
                            if frame_count == 0 { continue; }

                            if opts.frm_cnt_mode == FramesCountMode::Time
                            && opts.min_count_en
                            && frame_count < opts.min_count {
                                frame_count = opts.min_count;
                            }

                            result.push(MasterFileCreationProgramItem {
                                count:       multiple_of_5(frame_count),
                                temperature: *t,
                                exposure:    *exp,
                                gain:        *gain,
                                offset:      *offset as i32,
                                binning:     *bin,
                                crop:        *crop,
                            });
                        }
                    }
                }
            }
        }
    }

    result
}

fn build_master_biases_program(
    opts:             &MasterBiasesOptions,
    cam_opts:         &CamOptions,
    cooler_supported: bool,
) -> Vec<MasterFileCreationProgramItem> {
    let mut result = Vec::new();

    let temperatures = resolve_temperatures(&opts.temperature, cam_opts, cooler_supported);

    let gains = get_values(&opts.gain, cam_opts.frame.gain);
    let offsets = get_values(&opts.offset, cam_opts.frame.offset as f64);
    let binnings = resolve_binnings(&opts.binning, cam_opts);
    let crops = resolve_crops(&opts.crop, cam_opts);

    let count = multiple_of_5(opts.frames_count);
    // Zero count would build a master from a partial stack (see dp/darks skips)
    if count == 0 {
        return result;
    }

    for t in &temperatures {
        for bin in &binnings {
            for crop in &crops {
                for gain in &gains {
                    for offset in &offsets {
                        result.push(MasterFileCreationProgramItem {
                            count,
                            temperature: *t,
                            exposure:    opts.exposure,
                            gain:        *gain,
                            offset:      *offset as i32,
                            binning:     *bin,
                            crop:        *crop,
                        });
                    }
                }
            }
        }
    }

    result
}

pub struct DarkCreationMode {
    camera:      Arc<dyn Camera + Send + Sync>,
    mode:        DarkLibMode,
    calibr_data: Arc<Mutex<RawCalibration>>,
    program:     Vec<MasterFileCreationProgramItem>,
    index:       usize,
    state:       State,
    temperature: VecDeque<f64>,
}

impl DarkCreationMode {
    pub fn new(
        engine:      &Engine,
        mode:        DarkLibMode,
        raw_calibr:  &Arc<Mutex<RawCalibration>>,
        program:     &[MasterFileCreationProgramItem]
    ) -> eyre::Result<Self> {
        let camera = engine.cur_devices.camera_or_err()?;
        Ok(Self {
            camera,
            mode,
            calibr_data: Arc::clone(raw_calibr),
            program:     program.to_vec(),
            index:       0,
            state:       State::Undefined,
            temperature: VecDeque::new(),
        })
    }

    fn clear_calibr_data(&self) {
        let mut calibr_data = self.calibr_data.lock().unwrap();
        calibr_data.clear();
    }

    pub fn create_notify_result_for_starting_mode(
        program_item: MasterFileCreationProgramItem,
        cam_mode:     CameraMode,
    ) -> NotifyResult {
        let start_focusing_fun = move |engine: &Arc<Engine>, mode: &mut EngineModes| -> eyre::Result<()> {
            mode.active.abort()?;
            let prev_mode = std::mem::replace(&mut mode.active, Box::new(WaitingMode));
            let mut new_mode = TakingPicturesMode::new(cam_mode, engine)?;
            new_mode.set_dark_creation_program_item(&program_item);
            new_mode.set_next_mode(Some(prev_mode));
            new_mode.start()?;
            mode.active = Box::new(new_mode);
            Ok(())
        };
        NotifyResult::Exec(Box::new(start_focusing_fun))
    }

}

impl Mode for DarkCreationMode {
    fn kind(&self) -> ModeKind {
        match self.mode {
            DarkLibMode::DefectPixels =>
                ModeKind::CreatingDefectPixels,
            DarkLibMode::MasterDark =>
                ModeKind::CreatingMasterDarks,
            DarkLibMode::MasterBias =>
                ModeKind::CreatingMasterBiases,
        }
    }

    fn progress_string(&self) -> String {
        match (&self.state, &self.mode) {
            (State::WaitingForTemperature(value), _) =>
                format!("Waiting for temperature ({:.1}°C) to stabilize...", value),
            (_, DarkLibMode::DefectPixels) =>
                "Creating defect pixel files...".to_string(),
            (_, DarkLibMode::MasterDark) =>
                "Creating master dark files...".to_string(),
            (_, DarkLibMode::MasterBias) =>
                "Creating master bias files...".to_string(),
        }
    }

    fn can_be_stopped(&self) -> bool {
        true
    }

    fn progress(&self) -> Option<Progress> {
        Some(Progress {
            cur: self.index,
            total: self.program.len(),
        })
    }

    fn start(&mut self) -> eyre::Result<()> {
        self.state = State::Undefined;
        self.clear_calibr_data();
        Ok(())
    }

    fn timer_tick(&mut self, timer_period_ms: usize) -> eyre::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        let mut have_to_start = false;
        match self.state {
            State::Undefined => {
                let Some(item) = self.program.get(self.index) else {
                    return Ok(NotifyResult::Finished { next_mode: None });
                };

                if let Some(temperature) = item.temperature {
                    self.temperature.clear();
                    self.camera.set_temperature(Some(temperature))?;
                    self.state = State::WaitingForTemperature(temperature);
                    result = NotifyResult::ProgressChanges;
                } else {
                    have_to_start = true;
                }
            }

            State::WaitingForTemperature(desired_temperature) => {
                let temperature = self.camera.temperature()?;

                self.temperature.push_back(temperature);
                if self.temperature.len() * timer_period_ms > WAIT_TEMPERATURE_TIME * 1000 {
                    self.temperature.pop_front();

                    let min_temperature = self.temperature.iter()
                        .copied()
                        .min_by(f64::total_cmp)
                        .unwrap_or_default();
                    let max_temperature = self.temperature.iter()
                        .copied()
                        .max_by(f64::total_cmp)
                        .unwrap_or_default();

                    let average_temperature = self.temperature.iter().sum::<f64>() / self.temperature.len() as f64;

                    let temperature_drift = max_temperature - min_temperature;

                    if temperature_drift < 2.0
                    && f64::abs(average_temperature - desired_temperature) < 1.0 {
                        have_to_start = true;
                    }
                }
            }

            State::WaitingForDarkCreation => {
                self.index += 1;
                self.state = State::Undefined;
                result = NotifyResult::ProgressChanges;
            }
        }

        if have_to_start {
            self.state = State::WaitingForDarkCreation;
            let program_item = self.program[self.index].clone();

            result = match self.mode {
                DarkLibMode::DefectPixels =>
                    Self::create_notify_result_for_starting_mode(
                        program_item,
                        CameraMode::DefectPixels
                    ),
                DarkLibMode::MasterDark =>
                    Self::create_notify_result_for_starting_mode(
                        program_item,
                        CameraMode::MasterDark
                    ),
                DarkLibMode::MasterBias =>
                    Self::create_notify_result_for_starting_mode(
                        program_item,
                        CameraMode::MasterBias
                    ),
            };
        }

        Ok(result)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam_opts() -> CamOptions {
        CamOptions {
            device_id: String::new(),
            live_view: false,
            ctrl:      CamCtrlOptions::default(),
            frame:     FrameOptions {
                exp_main: 30.0,
                gain:     100.0,
                offset:   10,
                binning:  Binning::Bin2,
                crop:     Crop::P50,
                ..Default::default()
            },
        }
    }

    fn cam_opts_cooler() -> CamOptions {
        CamOptions {
            ctrl: CamCtrlOptions {
                enable_cooler: true,
                temperature:   -10.0,
                ..Default::default()
            },
            ..cam_opts()
        }
    }

    #[test]
    fn spec_mode() {
        assert_eq!(DarkLibSpec::DefectPixels(Default::default()).mode(),
            DarkLibMode::DefectPixels);
        assert_eq!(DarkLibSpec::MasterDarks(Default::default()).mode(),
            DarkLibMode::MasterDark);
        assert_eq!(DarkLibSpec::MasterBiases(Default::default()).mode(),
            DarkLibMode::MasterBias);
    }

    // Defect pixels

    #[test]
    fn dp_count_mode_normalizes_frames_count() {
        let spec = DarkLibSpec::DefectPixels(DefectPixelsOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 7,
            ..Default::default()
        });
        // Empty binning/crop selections fall back to cam_opts.frame.*
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program, vec![MasterFileCreationProgramItem {
            count: 10, temperature: None, exposure: 30.0, gain: 100.0, offset: 10,
            binning: Binning::Bin2, crop: Crop::P50,
        }]);
    }

    #[test]
    fn dp_time_mode_count() {
        let spec = DarkLibSpec::DefectPixels(DefectPixelsOptions {
            integr_time:   60.0, // minutes, frm_cnt_mode default is Time
            exposure_used: true,
            exposure:      45.0,
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program.len(), 1);
        assert_eq!(program[0].count, 80); // 60 * 60 / 45
    }

    #[test]
    fn dp_temperature_resolution() {
        let used = DefectPixelsOptions {
            temperature_used: true,
            temperature:      5.0,
            ..Default::default()
        };

        // Cooler unsupported -> None even with temperature_used
        let program = build_program(&DarkLibSpec::DefectPixels(used.clone()),
            &cam_opts_cooler(), false);
        assert_eq!(program[0].temperature, None);

        // Cooler + temperature_used -> spec temperature
        let program = build_program(&DarkLibSpec::DefectPixels(used),
            &cam_opts_cooler(), true);
        assert_eq!(program[0].temperature, Some(5.0));

        // Cooler + !used + enable_cooler -> camera ctrl temperature
        let program = build_program(&DarkLibSpec::DefectPixels(Default::default()),
            &cam_opts_cooler(), true);
        assert_eq!(program[0].temperature, Some(-10.0));

        // Cooler + !used + !enable_cooler -> None
        let program = build_program(&DarkLibSpec::DefectPixels(Default::default()),
            &cam_opts(), true);
        assert_eq!(program[0].temperature, None);
    }

    #[test]
    fn dp_binning_crop_cartesian() {
        let spec = DarkLibSpec::DefectPixels(DefectPixelsOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 5,
            binning:      BinningOptions {
                used: true, bin1x1: true, bin2x2: true, ..Default::default()
            },
            crop:         CropOptions {
                used: true, crop100: true, crop25: true, ..Default::default()
            },
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        let pairs: Vec<(Binning, Crop)> = program.iter()
            .map(|i| (i.binning, i.crop))
            .collect();
        assert_eq!(pairs, vec![
            (Binning::Orig, Crop::None),
            (Binning::Orig, Crop::P25),
            (Binning::Bin2, Crop::None),
            (Binning::Bin2, Crop::P25),
        ]);
    }

    #[test]
    fn dp_zero_frames_count_skipped() {
        let spec = DarkLibSpec::DefectPixels(DefectPixelsOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 0,
            ..Default::default()
        });
        assert!(build_program(&spec, &cam_opts(), false).is_empty());
    }

    // Master darks

    #[test]
    fn darks_defaults_from_cam_opts() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions::default());
        // Time mode: 60 * 60 / 30 = 120, min_count = 60 -> no clamp
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program, vec![MasterFileCreationProgramItem {
            count: 120, temperature: None, exposure: 30.0, gain: 100.0, offset: 10,
            binning: Binning::Bin2, crop: Crop::P50,
        }]);
    }

    #[test]
    fn darks_min_count_clamped_before_m5() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            integr_time:  25.0, // minutes: 60 * 25 / 30 = 50
            min_count_en: true,
            min_count:    53,
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program.len(), 1);
        // Order zero-check -> clamp (50 -> 53) -> m5 (53 -> 55); swapping gives 53
        assert_eq!(program[0].count, 55);
    }

    #[test]
    fn darks_min_count_not_applied_in_count_mode() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 3,
            min_count_en: true,
            min_count:    60,
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program[0].count, 5);
    }

    #[test]
    fn darks_zero_frames_count_skipped() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 0,
            ..Default::default()
        });
        assert!(build_program(&spec, &cam_opts(), false).is_empty());
    }

    #[test]
    fn darks_cartesian_product() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 10,
            temperature:  ValuesItem { used: true, values: vec![-10.0, -5.0] },
            exposure:     ValuesItem { used: true, values: vec![60.0] },
            gain:         ValuesItem { used: true, values: vec![200.0] },
            offset:       ValuesItem { used: true, values: vec![20.0] },
            binning:      BinningOptions {
                used: true, bin1x1: true, bin4x4: true, ..Default::default()
            },
            crop:         CropOptions {
                used: true, crop100: true, ..Default::default()
            },
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts_cooler(), true);
        // 2 temperatures x 2 binnings x 1 crop x 1 exposure x 1 gain x 1 offset
        assert_eq!(program.len(), 4);
        assert_eq!(program[0].temperature, Some(-10.0));
        assert_eq!(program[0].binning, Binning::Orig);
        assert_eq!(program[3].temperature, Some(-5.0));
        assert_eq!(program[3].binning, Binning::Bin4);
        assert!(program.iter()
            .all(|i| i.count == 10 && i.exposure == 60.0 && i.gain == 200.0
                && i.offset == 20));
    }

    #[test]
    fn darks_used_but_empty_values_fall_back_to_defaults() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: 5,
            exposure:     ValuesItem { used: true, values: vec![] },
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program[0].exposure, 30.0);
    }

    // Master biases

    #[test]
    fn biases_fixed_exposure_and_count() {
        let spec = DarkLibSpec::MasterBiases(MasterBiasesOptions {
            frames_count: 7,
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program, vec![MasterFileCreationProgramItem {
            count: 10, temperature: None, exposure: 0.0001, gain: 100.0, offset: 10,
            binning: Binning::Bin2, crop: Crop::P50,
        }]);
    }

    #[test]
    fn biases_zero_frames_count_skipped() {
        // Same zero skip as dp/darks: a zero-count item would build a master
        // from a partial stack
        let spec = DarkLibSpec::MasterBiases(MasterBiasesOptions {
            frames_count: 0,
            ..Default::default()
        });
        assert!(build_program(&spec, &cam_opts(), false).is_empty());
    }

    // Robustness against non-finite / absurd values (no panic, no inf casts)

    #[test]
    fn multiple_of_5_saturates() {
        assert_eq!(multiple_of_5(0), 0);
        assert_eq!(multiple_of_5(7), 10);
        assert_eq!(multiple_of_5(usize::MAX - 2), usize::MAX);
        assert_eq!(multiple_of_5(usize::MAX), usize::MAX);
    }

    #[test]
    fn dp_time_mode_bad_exposure_skipped() {
        for exposure in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let spec = DarkLibSpec::DefectPixels(DefectPixelsOptions {
                integr_time:   60.0,
                exposure_used: true,
                exposure,
                ..Default::default()
            });
            assert!(build_program(&spec, &cam_opts(), false).is_empty(),
                "exposure {exposure} must skip the item");
        }
    }

    #[test]
    fn darks_time_mode_bad_exposure_skipped() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            integr_time: 60.0,
            exposure:    ValuesItem { used: true, values: vec![0.0, f64::NAN] },
            ..Default::default()
        });
        assert!(build_program(&spec, &cam_opts(), false).is_empty());
    }

    #[test]
    fn dp_count_mode_huge_frames_count_saturates() {
        let spec = DarkLibSpec::DefectPixels(DefectPixelsOptions {
            frm_cnt_mode: FramesCountMode::Count,
            frames_count: usize::MAX,
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program.len(), 1);
        assert_eq!(program[0].count, usize::MAX);
    }

    #[test]
    fn darks_time_mode_huge_integr_time_saturates() {
        let spec = DarkLibSpec::MasterDarks(MasterDarksOptions {
            integr_time: f64::MAX, // 60 * MAX / 30 -> inf -> saturating cast
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts(), false);
        assert_eq!(program.len(), 1);
        assert_eq!(program[0].count, usize::MAX);
    }

    #[test]
    fn biases_temperatures_and_gain_offset_cartesian() {
        let spec = DarkLibSpec::MasterBiases(MasterBiasesOptions {
            temperature: ValuesItem { used: true, values: vec![-5.0] },
            gain:        ValuesItem { used: true, values: vec![100.0, 200.0] },
            offset:      ValuesItem { used: true, values: vec![10.0, 20.0, 30.0] },
            ..Default::default()
        });
        let program = build_program(&spec, &cam_opts_cooler(), true);
        assert_eq!(program.len(), 6);
        assert!(program.iter()
            .all(|i| i.temperature == Some(-5.0) && i.count == 60));
    }
}
