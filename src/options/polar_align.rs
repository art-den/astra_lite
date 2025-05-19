use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PloarAlignDir {
    East,
    West,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PloarAlignOptions {
    pub angle:        f64,
    pub direction:    PloarAlignDir,
    pub speed:        Option<String>,
    pub sim_alt_err:  f64,
    pub sim_az_err:   f64,
    pub auto_refresh: bool,
}

impl Default for PloarAlignOptions {
    fn default() -> Self {
        Self {
            angle:        30.0,
            direction:    PloarAlignDir::West,
            speed:        None,
            sim_alt_err:  1.1,
            sim_az_err:   1.4,
            auto_refresh: true,
        }
    }
}
