use std::sync::{Arc, Mutex, RwLock};

use crate::image::{histogram::Histogram, image::Image, image_stacker::{ImageStacker, ImageStackingMode}, info::LightFrameInfo, stars::Stars};

#[derive(Clone)]
pub struct LiveStackedImageInfo {
    pub image: Arc<LightFrameInfo>,
    pub stars: Arc<Stars>,
}

pub struct LiveStacking {
    pub stacker:  RwLock<ImageStacker>,
    pub image:    RwLock<Image>,
    pub hist:     RwLock<Histogram>,
    pub info:     RwLock<Option<LiveStackedImageInfo>>,
    pub time_cnt: Mutex<f64>,
}

impl LiveStacking {
    pub fn new() -> Self {
        Self {
            stacker:  RwLock::new(ImageStacker::new()),
            image:    RwLock::new(Image::new_empty()),
            hist:     RwLock::new(Histogram::new()),
            info:     RwLock::new(None),
            time_cnt: Mutex::new(0.0),
        }
    }

    pub fn prepare_for_work(&self, mode: ImageStackingMode) {
        self.stacker.write().unwrap().prepare_for_work(mode);
        self.image.write().unwrap().clear();
        self.hist.write().unwrap().clear();
        *self.info.write().unwrap() = None;
        *self.time_cnt.lock().unwrap() = 0.0;
    }
}
