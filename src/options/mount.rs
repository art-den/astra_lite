use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct MountOptions {
    pub device: String,
    pub inv_ns: bool,
    pub inv_we: bool,
    pub speed:  Option<String>,
}
