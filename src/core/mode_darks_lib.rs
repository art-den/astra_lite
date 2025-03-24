use std::{collections::VecDeque, sync::{Arc, Mutex, RwLock}};

use crate::{core::frame_processing::*, indi::{self}, options::*};

use super::{core::*, events::Progress};

enum State {
    Undefined,
    WaitingForTemperature(f64),
    WaitingForDarkCreation,
}

pub enum DarkLibMode {
    DefectPixelsFiles,
    MasterDarkFiles,
    MasterBiasFiles,
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
    mode:        DarkLibMode,
    calibr_data: Arc<Mutex<CalibrData>>,
    indi:        Arc<indi::Connection>,
    program:     Vec<MasterFileCreationProgramItem>,
    device:      DeviceAndProp,
    index:       usize,
    state:       State,
    temperature: VecDeque<f64>,
}

impl DarkCreationMode {
    pub fn new(
        mode:        DarkLibMode,
        calibr_data: &Arc<Mutex<CalibrData>>,
        options:     &Arc<RwLock<Options>>,
        indi:        &Arc<indi::Connection>,
        program:     &[MasterFileCreationProgramItem]
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };

        Ok(Self {
            mode,
            calibr_data: Arc::clone(calibr_data),
            indi:        Arc::clone(indi),
            program:     program.to_vec(),
            device:      cam_device.clone(),
            index:       0,
            state:       State::Undefined,
            temperature: VecDeque::new(),
        })
    }

    fn clear_calibr_data(&self) {
        let mut calibr_data = self.calibr_data.lock().unwrap();
        calibr_data.clear();
    }
}

impl Mode for DarkCreationMode {
    fn get_type(&self) -> ModeType {
        match self.mode {
            DarkLibMode::DefectPixelsFiles =>
                ModeType::CreatingDefectPixels,
            DarkLibMode::MasterDarkFiles =>
                ModeType::CreatingMasterDarks,
            DarkLibMode::MasterBiasFiles =>
                ModeType::CreatingMasterBiases,
        }
    }

    fn progress_string(&self) -> String {
        match (&self.state, &self.mode) {
            (State::WaitingForTemperature(value), _) =>
                format!("Waiting temperature ({:.1}°С) stabilization...", value),
            (_, DarkLibMode::DefectPixelsFiles) =>
                "Creating defect pixels files...".to_string(),
            (_, DarkLibMode::MasterDarkFiles) =>
                "Creating master dark files...".to_string(),
            (_, DarkLibMode::MasterBiasFiles) =>
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

    fn start(&mut self) -> anyhow::Result<()> {
        self.state = State::Undefined;
        self.clear_calibr_data();
        Ok(())
    }

    fn notify_timer_1s(&mut self) -> anyhow::Result<NotifyResult> {
        let mut result = NotifyResult::Empty;
        let mut have_to_start = false;
        match self.state {
            State::Undefined => {
                let Some(item) = self.program.get(self.index) else {
                    return Ok(NotifyResult::Finished { next_mode: None });
                };

                if let Some(temperature) = item.temperature {
                    self.temperature.clear();
                    self.indi.camera_set_temperature(&self.device.name, temperature)?;
                    self.state = State::WaitingForTemperature(temperature);
                    result = NotifyResult::ProgressChanges;
                } else {
                    have_to_start = true;
                }
            }

            State::WaitingForTemperature(desired_temperature) => {
                let temperature = self.indi.camera_get_temperature_prop_value(
                    &self.device.name
                )?.value;

                self.temperature.push_back(temperature);
                if self.temperature.len() > 20 {
                    self.temperature.pop_front();

                    let min_temperature = self.temperature.iter()
                        .copied()
                        .min_by(f64::total_cmp)
                        .unwrap_or_default();
                    let max_temperature = self.temperature.iter()
                        .copied()
                        .max_by(f64::total_cmp)
                        .unwrap_or_default();

                    if desired_temperature - min_temperature < 1.0
                    && max_temperature - desired_temperature < 1.0 {
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
            let prorgam_item = self.program[self.index].clone();

            result = match self.mode {
                DarkLibMode::DefectPixelsFiles =>
                    NotifyResult::StartCreatingDefectPixelsFile(prorgam_item),
                DarkLibMode::MasterDarkFiles =>
                    NotifyResult::StartCreatingMasterDarkFile(prorgam_item),
                DarkLibMode::MasterBiasFiles =>
                    NotifyResult::StartCreatingMasterBiasFile(prorgam_item),
            };
        }

        Ok(result)
    }
}
