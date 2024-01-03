use chrono::{DateTime, Utc};
use crate::utils::log_utils::TimeLogger;
use super::{stars_offset::*, histogram::*, image::*, stars::*};

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
    pub time:         Option<DateTime<Utc>>,
    pub width:        usize,
    pub height:       usize,
    pub exposure:     f64,
    pub raw_noise:    Option<f32>,
    pub noise:        f32,
    pub background:   i32,
    pub bg_percent:   f32,
    pub max_value:    u16,
    pub stars:        StarsInfo,
    pub stars_offset: Option<Offset>,
    pub offset_is_ok: bool,
}

impl LightFrameInfo {
    pub fn from_image(
        image:                &Image,
        max_stars_fwhm:       Option<f32>,
        max_stars_ovality:    Option<f32>,
        stars_pos_for_offset: Option<&Vec<Point>>,
        mt:                   bool,
    ) -> Self {
        let max_value = image.max_value();
        let overexposured_bord = (90 * max_value as u32 / 100) as u16;
        let mono_layer = if image.is_color() { &image.g } else { &image.l };

        // Noise

        let tmr = TimeLogger::start();
        let noise = mono_layer.calc_noise();
        tmr.log("calc image noise");

        // Background

        let tmr = TimeLogger::start();
        let background = mono_layer.calc_background(mt) as i32;
        tmr.log("calc image background");

        // Stars

        let tmr = TimeLogger::start();

        let stars_info = StarsInfo::new_from_image(
            &mono_layer,
            noise,
            background,
            overexposured_bord,
            max_value,
            max_stars_fwhm,
            max_stars_ovality,
            mt
        );

        tmr.log("searching stars");

        // Offset by reference stars

        let (stars_offset, offset_is_ok) = if let (Some(starts_for_offset), true, true) =
        (stars_pos_for_offset, stars_info.fwhm_is_ok, stars_info.ovality_is_ok) {
            let tmr = TimeLogger::start();
            let cur_stars_points: Vec<_> = stars_info.items.iter()
                .map(|star| Point {x: star.x, y: star.y })
                .collect();
            let image_offset = Offset::calculate(
                starts_for_offset,
                &cur_stars_points,
                image.width() as f64,
                image.height() as f64
            );
            tmr.log("Offset::calculate");
            let img_offset_is_ok = !image_offset.is_none();
            (image_offset, img_offset_is_ok)
        } else {
            (None, true)
        };

        Self {
            time: image.time.clone(),
            width: image.width(),
            height: image.height(),
            exposure: 0.0,
            raw_noise: None,
            noise,
            background,
            bg_percent: (100.0 * background as f64 / image.max_value() as f64) as f32,
            max_value,
            stars: stars_info,
            stars_offset,
            offset_is_ok,
        }
    }
}

pub struct FlatInfoChan {
    pub aver: f64,
    pub max: u16,
}

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
    pub aver: f64,
    pub median: u16,
    pub std_dev: f64,
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
