use std::sync::*;
use chrono::{DateTime, Utc};
use itertools::*;
use crate::log_utils::TimeLogger;
use crate::stars_offset::{Point, Offset};
use crate::{image::*, image_raw::*, stars::*};

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

pub struct HistogramChan {
    pub mean: f64,
    pub std_dev: f64,
    pub count: usize,
    pub freq: Vec<u32>,
}

impl HistogramChan {
    pub fn new() -> Self {
        Self {
            mean: 0.0,
            std_dev: 0.0,
            count: 0,
            freq: Vec::new(),
        }
    }

    fn take_from_freq(&mut self, freq: Vec<u32>, max_value: u16) {
        self.freq = freq;
        self.freq.resize(max_value as usize + 1, 0);
        let total_cnt = self.freq.iter().sum::<u32>() as usize;
        let total_sum = self.freq.iter().enumerate().map(|(i, v)| { i * *v as usize }).sum::<usize>();
        self.count = total_cnt as usize;
        self.mean = total_sum as f64 / total_cnt as f64;
        let mut sum = 0_f64;
        for (value, cnt) in self.freq.iter().enumerate() {
            let diff = self.mean - value as f64;
            sum += *cnt as f64 * diff * diff;
        }
        self.std_dev = f64::sqrt(sum / total_cnt as f64);
    }

    pub fn get_nth_element(&self, mut n: usize) -> u16 {
        for (idx, v) in self.freq.iter().enumerate() {
            if n < *v as usize {
                return idx as u16;
            }
            n -= *v as usize;
        }
        u16::MAX
    }

    pub fn median(&self) -> u16 {
        self.get_nth_element(self.count/2)
    }

    pub fn get_percentile(&self, n: usize) -> u16 {
        self.get_nth_element((n * self.count + 50) / 100)
    }
}

struct HistTmp {
    freq: Vec<u32>,
}

impl HistTmp {
    fn new_from_slice(freq_data: &[u32]) -> Self {
        let mut freq = Vec::new();
        freq.extend_from_slice(freq_data);
        Self { freq }
    }

    fn new() -> Self {
        let mut freq = Vec::new();
        freq.resize(u16::MAX as usize + 1, 0);
        Self { freq }
    }

    fn reduce(&mut self, other: &HistTmp) {
        for (s, d) in izip!(&other.freq, &mut self.freq) { *d += *s; }
    }
}


pub struct Histogram {
    pub max: u16,
    pub r:   Option<HistogramChan>,
    pub g:   Option<HistogramChan>,
    pub b:   Option<HistogramChan>,
    pub l:   Option<HistogramChan>,
}

impl Histogram {
    pub fn new() -> Self {
        Self { max: 0, r: None, g: None, b: None, l: None }
    }

    pub fn from_raw_image(
        &mut self,
        img:        &RawImage,
        monochrome: bool,
    ) {
        let img_max_value = img.info().max_value;
        self.max = img_max_value;
        if img.info().cfa == CfaType::None || monochrome {
            let tmp = Self::tmp_from_slice(img.as_slice(), 1);
            let mut l = self.l.take().unwrap_or(HistogramChan::new());
            l.take_from_freq(tmp.freq, img_max_value);
            self.l = Some(l);
            self.r = None;
            self.g = None;
            self.b = None;
        } else {
            let tmp = Mutex::new(Vec::<(HistTmp, HistTmp, HistTmp)>::new());
            let img_height = img.info().height;
            let process_range = |y1, y2| {
                let mut r = [0u32; u16::MAX as usize + 1];
                let mut g = [0u32; u16::MAX as usize + 1];
                let mut b = [0u32; u16::MAX as usize + 1];
                for y in y1..y2 {
                    let cfa = img.cfa_row(y);
                    let row_data = img.row(y);
                    for (v, c) in izip!(row_data, cfa.iter().cycle()) {
                        match *c {
                            CfaColor::R => r[*v as usize] += 1,
                            CfaColor::G => g[*v as usize] += 1,
                            CfaColor::B => b[*v as usize] += 1,
                            _           => {},
                        }
                    }
                }
                tmp.lock().unwrap().push((
                    HistTmp::new_from_slice(&r),
                    HistTmp::new_from_slice(&g),
                    HistTmp::new_from_slice(&b)
                ));
            };

            // map
            let max_threads = rayon::current_num_threads();
            let tasks_cnt = if max_threads != 1 { 2 * max_threads  } else { 1 };
            rayon::scope(|s| {
                for t in 0..tasks_cnt {
                    let y1 = t * img_height / tasks_cnt;
                    let y2 = (t+1) * img_height / tasks_cnt;
                    s.spawn(move |_| process_range(y1, y2));
                }
            });

            // reduce
            let mut r_res = HistTmp::new();
            let mut g_res = HistTmp::new();
            let mut b_res = HistTmp::new();
            for (r, g, b) in tmp.lock().unwrap().iter() {
                r_res.reduce(r);
                g_res.reduce(g);
                b_res.reduce(b);
            }

            let mut r = self.r.take().unwrap_or(HistogramChan::new());
            let mut g = self.g.take().unwrap_or(HistogramChan::new());
            let mut b = self.b.take().unwrap_or(HistogramChan::new());

            r.take_from_freq(r_res.freq, img_max_value);
            g.take_from_freq(g_res.freq, img_max_value);
            b.take_from_freq(b_res.freq, img_max_value);

            self.l = None;
            self.r = Some(r);
            self.g = Some(g);
            self.b = Some(b);
        }
    }

    fn tmp_from_slice(data: &[u16], step: usize) -> HistTmp {
        let tmp = Mutex::new(Vec::<HistTmp>::new());
        let process_sub_slice = |from: usize, to: usize| {
            let sub_slice = &data[from..to];
            let mut res = [0u32; u16::MAX as usize + 1];
            if step == 1 {
                for v in sub_slice.iter() {
                    res[*v as usize] += 1;
                }
            } else {
                for v in sub_slice.iter().step_by(step) {
                    res[*v as usize] += 1;
                }
            }
            tmp.lock().unwrap().push(
                HistTmp::new_from_slice(&res)
            );
        };

        // map
        let size = data.len();
        let max_threads = rayon::current_num_threads();
        let tasks_cnt = if max_threads != 1 { 2 * max_threads  } else { 1 };
        rayon::scope(|s| {
            for t in 0..tasks_cnt {
                let from = t * size / tasks_cnt;
                let to = (t+1) * size / tasks_cnt;
                s.spawn(move |_| process_sub_slice(from, to));
            }
        });

        // reduce
        let mut res = HistTmp::new();
        for t in tmp.lock().unwrap().iter() {
            res.reduce(t);
        }
        res
    }

    pub fn from_image(&mut self, img: &Image) {
        let from_image_layer = |
            chan:  Option<HistogramChan>,
            layer: &ImageLayer<u16>,
        | -> Option<HistogramChan> {
            if layer.is_empty() { return None; }
            let mut chan = chan.unwrap_or(HistogramChan::new());
            let slice = layer.as_slice();
            let step = (slice.len() / 3_000_000) | 1;
            let tmp = Self::tmp_from_slice(slice, step);
            chan.take_from_freq(tmp.freq, img.max_value());
            Some(chan)
        };
        self.max = img.max_value();
        self.l = from_image_layer(self.l.take(), &img.l);
        self.r = from_image_layer(self.r.take(), &img.r);
        self.g = from_image_layer(self.g.take(), &img.g);
        self.b = from_image_layer(self.b.take(), &img.b);
    }
}