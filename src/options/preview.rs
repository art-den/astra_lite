use serde::{Serialize, Deserialize};

use crate::image::preview::PreviewParams;

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone, Copy)]
pub enum PreviewColorMode {
    #[default]Rgb,
    Red,
    Green,
    Blue
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub enum PreviewSource {
    #[default]OrigFrame,
    LiveStacking
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone, Copy)]
pub enum PreviewScale {
    #[default]FitWindow,
    Original,
    P75,
    P50,
    P33,
    P25,
    CenterAndCorners
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct PreviewOptions {
    pub scale:       PreviewScale,
    pub dark_lvl:    f64,
    pub light_lvl:   f64,
    pub gamma:       f64,
    pub source:      PreviewSource,
    pub remove_grad: bool,
    pub wb_auto:     bool,
    pub wb_red:      f64,
    pub wb_green:    f64,
    pub wb_blue:     f64,
    pub stars:       bool,

    #[serde(skip_serializing)]
    pub color:       PreviewColorMode,

    // fields for PreviewOptions::preview_params
    #[serde(skip_serializing)] pub widget_width: usize,
    #[serde(skip_serializing)] pub widget_height: usize,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            scale:         PreviewScale::default(),
            dark_lvl:      0.2,
            light_lvl:     0.8,
            gamma:         2.2,
            source:        PreviewSource::default(),
            remove_grad:   false,
            wb_auto:       true,
            wb_red:        1.0,
            wb_green:      1.0,
            wb_blue:       1.0,
            color:         PreviewColorMode::Rgb,
            widget_width:  0,
            widget_height: 0,
            stars:         false,
        }
    }
}

impl PreviewOptions {
    pub fn preview_params(&self) -> PreviewParams {
        let wb = if !self.wb_auto {
            Some([self.wb_red, self.wb_green, self.wb_blue])
        } else {
            None
        };

        PreviewParams {
            dark_lvl:         self.dark_lvl,
            light_lvl:        self.light_lvl,
            gamma:            self.gamma,
            pr_area_width:    self.widget_width,
            pr_area_height:   self.widget_height,
            scale:            self.scale,
            orig_frame_in_ls: self.source == PreviewSource::OrigFrame,
            remove_gradient:  self.remove_grad,
            color:            self.color,
            stars:            self.stars,
            wb,
        }
    }
}