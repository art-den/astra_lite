use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct IndiOptions {
    pub mount:    Option<String>,
    pub camera:   Option<String>,
    pub guid_cam: Option<String>,
    pub focuser:  Option<String>,
    pub remote:   bool,
    pub address:  String,
}

impl Default for IndiOptions {
    fn default() -> Self {
        Self {
            mount:    None,
            camera:   None,
            guid_cam: None,
            focuser:  None,
            remote:   false,
            address:  "localhost".to_string(),
        }
    }
}
