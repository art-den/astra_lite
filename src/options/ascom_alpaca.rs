use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AscomAlpacaOptions {
    pub address: String,
}

impl Default for AscomAlpacaOptions {
    fn default() -> Self {
        Self {
            address: "http://localhost:11111".to_string(),
        }
    }
}
