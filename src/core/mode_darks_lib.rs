use std::{collections::VecDeque, sync::{Arc, Mutex}};

use crate::{
    core::{frame_processing::*, mode_camera::{CameraMode, TakingPicturesMode}, mode_waiting::WaitingMode},
    hal::Camera,
   options::*
};

use super::{core::*, events::Progress};

const WAIT_TEMPERATURE_TIME: usize = 20; // seconds

enum State {
    Undefined,
    WaitingForTemperature(f64),
    WaitingForDarkCreation,
}

pub enum DarkLibMode {
    DefectPixels,
    MasterDark,
    MasterBias,
}

#[derive(Clone)]
pub struct MasterFileCreationProgramItem {
    pub count:       usize,
    pub temperature: Option<f64>,
    pub exposure:    f64,
    pub gain:        f64,
    pub offset:      i32,
    pub binning:     Binning,
    pub crop:        Crop,
}

pub struct DarkCreationMode {
    camera:      Arc<dyn Camera + Send + Sync>,
    mode:        DarkLibMode,
    calibr_data: Arc<Mutex<CalibrData>>,
    program:     Vec<MasterFileCreationProgramItem>,
    index:       usize,
    state:       State,
    temperature: VecDeque<f64>,
}

impl DarkCreationMode {
    pub fn new(
        core:        &Core,
        mode:        DarkLibMode,
        calibr_data: &Arc<Mutex<CalibrData>>,
        program:     &[MasterFileCreationProgramItem]
    ) -> eyre::Result<Self> {
        let camera = core.cur_devices.camera_or_err()?;
        Ok(Self {
            camera,
            mode,
            calibr_data: Arc::clone(calibr_data),
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
        let start_focusing_fun = move |core: &Arc<Core>, mode: &mut ModeData| -> eyre::Result<()> {
            mode.active.abort()?;
            let prev_mode = std::mem::replace(&mut mode.active, Box::new(WaitingMode));
            let mut new_mode = TakingPicturesMode::new(cam_mode, core)?;
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
    fn get_type(&self) -> ModeType {
        match self.mode {
            DarkLibMode::DefectPixels =>
                ModeType::CreatingDefectPixels,
            DarkLibMode::MasterDark =>
                ModeType::CreatingMasterDarks,
            DarkLibMode::MasterBias =>
                ModeType::CreatingMasterBiases,
        }
    }

    fn progress_string(&self) -> String {
        match (&self.state, &self.mode) {
            (State::WaitingForTemperature(value), _) =>
                format!("Waiting temperature ({:.1}°С) stabilization...", value),
            (_, DarkLibMode::DefectPixels) =>
                "Creating defect pixels files...".to_string(),
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

    fn notify_periodical_timer_tick(&mut self, timer_period_ms: usize) -> eyre::Result<NotifyResult> {
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
