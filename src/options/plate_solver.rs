use serde::{Serialize, Deserialize};

use super::{Binning, Gain};

#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy)]
pub enum PlateSolverType {
    #[default]
    Astrometry,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PlateSolverOptions {
    pub solver:        PlateSolverType,
    pub exposure:      f64,
    pub gain:          Gain,
    pub bin:           Binning,
    pub timeout:       u32,
    pub blind_timeout: u32,
}

impl Default for PlateSolverOptions {
    fn default() -> Self {
        Self {
            solver: PlateSolverType::default(),
            exposure: 3.0,
            gain: Gain::Same,
            bin: Binning::Bin2,
            timeout: 10,
            blind_timeout: 30,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct SeparatedPlateSolverOptions {
    pub exposure: f64,
    pub gain: Gain,
    pub bin: Binning,
}
