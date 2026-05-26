use std::path::PathBuf;

use serde::{Serialize, Deserialize};

use crate::core::consts::DIRECTORY;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct CalibrOptions {
    pub dark_library_path: PathBuf,
    pub dark_frame_en:     bool,
    pub flat_frame_en:     bool,
    pub flat_frame_fname:  PathBuf,
    pub hot_pixels:        bool,
}

impl Default for CalibrOptions {
    fn default() -> Self {
        Self {
            dark_library_path: PathBuf::new(),
            dark_frame_en:     true,
            flat_frame_en:     false,
            flat_frame_fname:  PathBuf::new(),
            hot_pixels:        true,
        }
    }
}

impl CalibrOptions {
    pub fn check(&mut self) -> anyhow::Result<()> {
        if self.dark_library_path.as_os_str().is_empty() {
            let mut dark_lib_path = dirs::home_dir().unwrap();
            dark_lib_path.push(DIRECTORY);
            dark_lib_path.push("DarksLibrary");
            if !dark_lib_path.is_dir() {
                std::fs::create_dir_all(&dark_lib_path)?;
            }
            self.dark_library_path = dark_lib_path;
        }
        Ok(())
    }
}
