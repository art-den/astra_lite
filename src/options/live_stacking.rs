use std::path::PathBuf;

use serde::{Serialize, Deserialize};

use crate::core::consts::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct LiveStackingOptions {
    pub save_orig:     bool,
    pub save_minutes:  usize,
    pub save_enabled:  bool,
    pub out_dir:       PathBuf,
    pub remove_tracks: bool,
}

impl Default for LiveStackingOptions {
    fn default() -> Self {
        Self {
            save_orig:     false,
            save_minutes:  5,
            save_enabled:  true,
            out_dir:       PathBuf::new(),
            remove_tracks: false,
        }
    }
}

impl LiveStackingOptions {
    pub fn check(&mut self) -> anyhow::Result<()> {
        if self.out_dir.as_os_str().is_empty() {
            let mut save_path = dirs::home_dir().unwrap();
            save_path.push(DIRECTORY);
            save_path.push(LIVE_STACKING_DIR);
            if !save_path.is_dir() {
                std::fs::create_dir_all(&save_path)?;
            }
            self.out_dir = save_path;
        }
        Ok(())
    }
}
