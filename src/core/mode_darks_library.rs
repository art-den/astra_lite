use std::{collections::VecDeque, sync::{Arc, RwLock}};

use crate::{indi, options::*, DeviceAndProp, Options};

use super::core::*;

enum State {
    Undefined,
    WaitingForTemperature(f64),
    WaitingForDarkCreation,
}

pub enum DarkLibMode {
    DarkFiles,
    DefectPixelsFiles,
}

#[derive(Clone)]
pub struct DarkCreationProgramItem {
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
    options:     Arc<RwLock<Options>>,
    indi:        Arc<indi::Connection>,
    program:     Vec<DarkCreationProgramItem>,
    device:      DeviceAndProp,
    index:       usize,
    state:       State,
    temperature: VecDeque<f64>,
}

impl DarkCreationMode {
    pub fn new(
        mode:    DarkLibMode,
        options: &Arc<RwLock<Options>>,
        indi:    &Arc<indi::Connection>,
        program: &[DarkCreationProgramItem]
    ) -> anyhow::Result<Self> {
        let opts = options.read().unwrap();
        let Some(cam_device) = &opts.cam.device else {
            anyhow::bail!("Camera is not selected");
        };

        Ok(Self {
            mode,
            options:     Arc::clone(options),
            indi:        Arc::clone(indi),
            program:     program.to_vec(),
            device:      cam_device.clone(),
            index:       0,
            state:       State::Undefined,
            temperature: VecDeque::new(),
        })
    }
}

impl Mode for DarkCreationMode {
    fn get_type(&self) -> ModeType {
        match self.mode {
            DarkLibMode::DarkFiles =>
                ModeType::CreatingDarks,
            DarkLibMode::DefectPixelsFiles =>
                ModeType::CreatingDefectPixels,
        }
    }

    fn progress_string(&self) -> String {
        match (&self.state, &self.mode) {
            (State::WaitingForTemperature(value), _) =>
                format!("Waiting temperature ({:.1}°С) stabilization...", value),
            (_, DarkLibMode::DarkFiles) =>
                "Creating dark files...".to_string(),
            (_, DarkLibMode::DefectPixelsFiles) =>
                "Creating defect pixels files...".to_string(),
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
                    result = NotifyResult::ModeStrChanged;
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
                result = NotifyResult::ModeStrChanged;
            }
        }

        if have_to_start {
            self.state = State::WaitingForDarkCreation;
            let prorgam_item = self.program[self.index].clone();

            result = match self.mode {
                DarkLibMode::DarkFiles =>
                    NotifyResult::StartCreatingDark(prorgam_item),
                DarkLibMode::DefectPixelsFiles =>
                    NotifyResult::StartCreatingDefectPixelsFiles(prorgam_item),
            };
        }

        Ok(result)
    }
}
