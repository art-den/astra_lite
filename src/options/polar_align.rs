use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PolarAlignDir {
    East,
    West,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PolarAlignOptions {
    pub angle:        f64,
    pub direction:    PolarAlignDir,
    pub speed:        Option<String>,
    pub sim_alt_err:  f64,
    pub sim_az_err:   f64,
    pub auto_refresh: bool,
}

impl Default for PolarAlignOptions {
    fn default() -> Self {
        Self {
            angle:        30.0,
            direction:    PolarAlignDir::West,
            speed:        None,
            sim_alt_err:  1.1,
            sim_az_err:   1.4,
            auto_refresh: true,
        }
    }
}
