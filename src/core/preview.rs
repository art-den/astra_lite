use std::sync::{Arc, RwLock};

use crate::{
    core::frame_processing::LightFrameResult,
    image::{
        histogram::Histogram,
        image::Image,
        info::{FlatImageInfo, RawImageStat},
        preview::{PreviewRgbData, get_preview_rgb_data},
    },
    options::{PreviewOptions, PreviewScale},
};


pub enum ResultImageInfo {
    None,
    LightInfo(Arc<LightFrameResult>),
    FlatInfo(FlatImageInfo),
    RawInfo(RawImageStat),
}

pub struct Preview {
    pub image:    Arc<RwLock<Image>>,
    pub raw_hist: Arc<RwLock<Histogram>>,
    pub img_hist: RwLock<Histogram>,
    pub info:     RwLock<ResultImageInfo>,
}

impl Preview {
    pub fn new() -> Self {
        Self {
            image:    Arc::new(RwLock::new(Image::new_empty())),
            raw_hist: Arc::new(RwLock::new(Histogram::new())),
            img_hist: RwLock::new(Histogram::new()),
            info:     RwLock::new(ResultImageInfo::None),
        }
    }

    pub fn create_preview_for_platesolve_image(&self, po: &PreviewOptions) -> Option<PreviewRgbData> {
        let image = self.image.read().unwrap();
        let hist = self.img_hist.read().unwrap();
        let mut pp = po.preview_params();
        pp.pr_area_height = 1500;
        pp.pr_area_width = 1500;
        pp.scale = PreviewScale::FitWindow;
        get_preview_rgb_data(&image, &hist, &pp, None)
    }
}
