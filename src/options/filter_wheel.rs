use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct FilterWheelOptions {
    pub device: String,
}