use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct SiteOptions {
    pub latitude:  f64, // in degrees
    pub longitude: f64, // in degrees
}
