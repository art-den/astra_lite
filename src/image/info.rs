use chrono::{DateTime, Utc};
use crate::utils::log_utils::TimeLogger;
use super::{histogram::*, image::*, raw::CalibrMethods};

pub fn seconds_to_total_time_str(seconds: f64, short: bool) -> String {
    let secs_total = seconds as u64;
    let minutes_total = secs_total / 60;
    let hours = minutes_total / 60;
    if hours != 0 {
        if !short {
            format!("{} h. {} min.", hours, minutes_total % 60)
        } else {
            format!("{}h{}m", hours, minutes_total % 60)
        }
    } else if minutes_total != 0 {
        if !short {
            format!("{} min.", minutes_total)
        } else {
            format!("{}m", minutes_total)
        }
    } else {
        if !short {
            format!("{} sec.", secs_total)
        } else {
            format!("{}s", secs_total)
        }
    }
}

pub struct LightFrameInfo {
    pub time:           Option<DateTime<Utc>>,
    pub width:          usize,
    pub height:         usize,
    pub exposure:       f64,
    pub raw_noise:      Option<f32>,
    pub noise:          f32,
    pub background:     i32,
    pub bg_percent:     f32,
    pub max_value:      u16,
    pub calibr_methods: CalibrMethods,
}

impl LightFrameInfo {
    pub fn from_image(image: &Image, mt: bool) -> Self {
        let max_value = image.max_value();
        let mono_layer = if image.is_color() { &image.g } else { &image.l };

        // Noise

        let tmr = TimeLogger::start();
        let noise = mono_layer.calc_noise();
        tmr.log("calc image noise");

        // Background

        let tmr = TimeLogger::start();
        let background = mono_layer.calc_background(mt) as i32;
        tmr.log("calc image background");

        Self {
            time: image.raw_info.as_ref().map(|info| info.time.clone()).flatten(),
            width: image.width(),
            height: image.height(),
            exposure: 0.0,
            raw_noise: None,
            noise,
            background,
            bg_percent: (100.0 * background as f64 / image.max_value() as f64) as f32,
            max_value,
            calibr_methods: CalibrMethods::empty(),
        }
    }
}

#[derive(Clone)]
pub struct FlatInfoChan {
    pub aver: f32,
    pub max: u16,
}

#[derive(Default, Clone)]
pub struct FlatImageInfo {
    pub max_value: u16,
    pub r: Option<FlatInfoChan>,
    pub g: Option<FlatInfoChan>,
    pub b: Option<FlatInfoChan>,
    pub l: Option<FlatInfoChan>,
}

impl FlatImageInfo {
    pub fn from_histogram(hist: &Histogram) -> Self {
        let calc = |chan: Option<&HistogramChan>| -> Option<FlatInfoChan> {
            chan
                .map(|c| { FlatInfoChan {
                    aver: c.mean,
                    max:  c.get_nth_element(95 * c.count / 100),
                }})
        };
        Self {
            max_value: hist.max,
            r: calc(hist.r.as_ref()),
            g: calc(hist.g.as_ref()),
            b: calc(hist.b.as_ref()),
            l: calc(hist.l.as_ref()),
        }
    }
}

pub struct RawImageStat {
    pub max_value: u16,
    pub aver: f32,
    pub median: u16,
    pub std_dev: f32,
}

impl RawImageStat {
    pub fn from_histogram(hist: &Histogram) -> Self {
        let h = hist.l.as_ref().unwrap();
        Self {
            max_value: hist.max,
            aver: h.mean,
            median: h.median(),
            std_dev: h.std_dev,
        }
    }
}
