use std::path::PathBuf;

use crate::image::raw::{BadPixels, RawImage};

#[derive(Default)]
pub struct RawCalibration {
    pub subtract_image:      Option<RawImage>,
    pub subtract_fname:      Option<PathBuf>,
    pub master_flat:         Option<RawImage>,
    pub master_flat_fname:   Option<PathBuf>,
    pub defect_pixels:       Option<BadPixels>,
    pub defect_pixels_fname: Option<PathBuf>,
}

impl RawCalibration {
    pub fn clear(&mut self) {
        self.subtract_image = None;
        self.subtract_fname = None;
        self.master_flat = None;
        self.master_flat_fname = None;
        self.defect_pixels = None;
        self.defect_pixels_fname = None;
    }
}
