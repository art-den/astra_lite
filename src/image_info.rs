use std::collections::{HashSet, VecDeque};
use std::f64::consts::PI;
use std::sync::*;
use itertools::*;
use crate::log_utils::TimeLogger;
use crate::{image::*, image_raw::*, math::*};

const MAX_STAR_DIAM: usize = 32;
const MAX_STARS_CNT: usize = 500;
const MAX_STARS_FOR_STAR_IMAGE: usize = 200;

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

#[derive(Clone)]
pub struct Star {
    pub x: f64,
    pub y: f64,
    pub background: u16,
    pub max_value: u16,
    pub brightness: u32,
    pub overexposured: bool,
    pub points: HashSet<(isize, isize)>,
    pub width: usize,
    pub height: usize,
}

pub type Stars = Vec<Star>;

pub struct LightImageInfo {
    pub width: usize,
    pub height: usize,
    pub exposure: f64,
    pub noise: f32,
    pub background: i32,
    pub max_value: u16,
    pub stars: Stars,
    pub star_img: ImageLayer<u16>,
    pub stars_fwhm: Option<f32>,
    pub stars_fwhm_good: bool,
    pub stars_ovality: Option<f32>,
    pub stars_ovality_good: bool,
}

impl LightImageInfo {
    pub fn from_image(
        image:             &Image,
        exposure:          f64,
        max_stars_fwhm:    Option<f32>,
        max_stars_ovality: Option<f32>,
        mt:                bool
    ) -> Self {
        let max_value = image.max_value();
        let overexposured_bord = (90 * max_value as u32 / 100) as u16;
        let mono_layer = if image.is_color() { &image.g } else { &image.l };

        let tmr = TimeLogger::start();
        let noise = mono_layer.calc_noise();
        tmr.log("calc image noise");

        let tmr = TimeLogger::start();
        let background = mono_layer.calc_background(mt) as i32;
        tmr.log("calc image background");

        let tmr = TimeLogger::start();
        let stars = Self::find_stars_in_image(
            &mono_layer,
            noise,
            overexposured_bord,
            mt
        );
        tmr.log("searching stars");

        let tmr = TimeLogger::start();
        const COMMON_STAR_MAG: usize = 4;
        const COMMON_STAR_MAG_F: f64 = COMMON_STAR_MAG as f64;
        let star_img = Self::calc_common_star_image(&mono_layer, &stars, COMMON_STAR_MAG);
        let stars_fwhm = Self::calc_fwhm(&star_img)
            .map(|v| (v / (COMMON_STAR_MAG_F * COMMON_STAR_MAG_F)) as f32);
        let stars_ovality = Self::calc_ovality(&star_img)
            .map(|v| (v / COMMON_STAR_MAG_F) as f32);
        tmr.log("calc fwhm+ovality");

        let stars_fwhm_good = if let Some(max_stars_fwhm) = max_stars_fwhm {
            stars_fwhm.unwrap_or(999.0) < max_stars_fwhm
        } else {
            true
        };

        let stars_ovality_good = if let Some(max_stars_ovality) = max_stars_ovality {
            stars_ovality.unwrap_or(999.0) < max_stars_ovality
        } else {
            true
        };

        Self {
            width: image.width(),
            height: image.height(),
            exposure,
            noise,
            background,
            max_value,
            stars,
            star_img,
            stars_fwhm,
            stars_fwhm_good,
            stars_ovality,
            stars_ovality_good,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.stars_fwhm_good && self.stars_ovality_good
    }

    fn find_stars_in_image(
        image:              &ImageLayer<u16>,
        mut noise:          f32,
        overexposured_bord: u16,
        mt:                 bool
    ) -> Stars {
        if noise < 1.0 { noise = 1.0; }
        const MAX_STARS_POINTS_CNT: usize = MAX_STAR_DIAM * MAX_STAR_DIAM;
        let iir_filter_coeffs = IirFilterCoeffs::new(0.95);
        let border = (noise * 50.0) as u32;
        let possible_stars = Mutex::new(Vec::new());
        let find_possible_stars_in_rows = |y1: usize, y2: usize| {
            let mut filtered = Vec::new();
            filtered.resize(image.width(), 0);
            for y in y1..y2 {
                if y < MAX_STAR_DIAM/2 || y > image.height()-MAX_STAR_DIAM/2 {
                    continue;
                }
                let row = image.row(y);
                let mut filter = IirFilter::new();
                filter.filter_direct_and_revert_u16(&iir_filter_coeffs, row, &mut filtered);
                for (i, ((v1, v2, v3, v4, v5, v6, v7), f))
                in row.iter().tuple_windows().zip(&filtered[2..]).enumerate() {
                    let f1 = *v1 as u32 + *v2 as u32 + *v3 as u32;
                    let f2 = *v3 as u32 + *v4 as u32 + *v5 as u32;
                    let f3 = *v5 as u32 + *v6 as u32 + *v7 as u32;
                    if f1 > f2 || f3 > f2 { continue; }
                    let f = *f as u32 * 3;
                    if f2 > f && (f2-f) > border {
                        let star_x = i as isize+3;
                        let star_y = y as isize;
                        if star_x < (MAX_STAR_DIAM/2) as isize
                        || star_y < (MAX_STAR_DIAM/2) as isize
                        || star_x > (image.width() - MAX_STAR_DIAM/2) as isize
                        || star_y > (image.height() - MAX_STAR_DIAM/2) as isize {
                            continue; // skip points near image border
                        }
                        possible_stars.lock().unwrap().push((star_x, star_y, f2 / 3));
                    }
                }
            }
        };
        if !mt {
            find_possible_stars_in_rows(0, image.height());
        } else {
            rayon::scope(|s| {
                let mut y1 = 0_usize;
                loop {
                    let mut y2 = y1 + 100;
                    if y2 > image.height() { y2 = image.height(); }
                    s.spawn(move |_| {
                        find_possible_stars_in_rows(y1, y2);
                    });
                    if y2 == image.height() { break; }
                    y1 = y2;
                }
            });
        }

        let mut possible_stars = possible_stars.into_inner().unwrap();
        possible_stars.sort_by_key(|(_, _, v)| -(*v as i32));
        let mut all_star_coords = HashSet::<(isize, isize)>::new();
        let mut flood_filler = FloodFiller::new();
        let mut stars = Vec::new();
        let mut star_bg_values = Vec::new();
        let max_stars_points = image.width() * image.height() / 100; // 1% of area maximum
        let mut big_cnt = 0_usize;
        for (x, y, max_v) in possible_stars {
            if all_star_coords.contains(&(x, y)) { continue; }
            if all_star_coords.len() > max_stars_points
            || big_cnt > 1000 {
                return Stars::new();
            }
            let x1 = x - MAX_STAR_DIAM as isize / 2;
            let y1 = y - MAX_STAR_DIAM as isize / 2;
            let x2 = x + MAX_STAR_DIAM as isize / 2;
            let y2 = y + MAX_STAR_DIAM as isize / 2;
            star_bg_values.clear();
            for v in image.rect_iter(x1, y1, x2, y2) {
                star_bg_values.push(v);
            }
            let bg_pos = star_bg_values.len() / 4;
            let bg = *star_bg_values.select_nth_unstable(bg_pos).1;
            let mut star_points = HashSet::new();
            let border = bg as i32 + (max_v as i32 - bg as i32) / 3;
            if border <= 0 { continue; }
            let border = border as u16;
            let mut x_summ = 0_f64;
            let mut y_summ = 0_f64;
            let mut crd_cnt = 0_f64;
            let mut brightness = 0_i32;
            let mut overexposured = false;
            flood_filler.fill(
                x,
                y,
                |x, y| -> bool {
                    let v = image.get(x, y).unwrap_or(0);
                    let hit = v > border;
                    if hit {
                        if all_star_coords.contains(&(x, y))
                        || star_points.contains(&(x, y))
                        || star_points.len() > MAX_STARS_POINTS_CNT {
                            return false;
                        }
                        if v > overexposured_bord {
                            overexposured = true;
                        }
                        star_points.insert((x, y));
                        all_star_coords.insert((x, y));
                        let v_part = linear_interpolate(v as f64, bg as f64, max_v as f64, 0.0, 1.0);
                        x_summ += v_part * x as f64;
                        y_summ += v_part * y as f64;
                        crd_cnt += v_part;
                        brightness += v as i32 - bg as i32;
                    }
                    hit
                }
            );

            if star_points.len() > MAX_STARS_POINTS_CNT {
                big_cnt += 1;
            }

            if star_points.len() < MAX_STARS_POINTS_CNT
            && max_v > bg as u32
            && brightness > 0
            && Self::check_is_star_points_ok(&star_points) {
                let min_x = star_points.iter().map(|(x, _)| *x).min().unwrap_or(x);
                let max_x = star_points.iter().map(|(x, _)| *x).max().unwrap_or(x);
                let min_y = star_points.iter().map(|(_, y)| *y).min().unwrap_or(y);
                let max_y = star_points.iter().map(|(_, y)| *y).max().unwrap_or(y);
                let width = 3 * isize::max(x-min_x+1, max_x-x+1);
                let height = 3 * isize::max(y-min_y+1, max_y-y+1);
                stars.push(Star {
                    x: x_summ / crd_cnt,
                    y: y_summ / crd_cnt,
                    background: bg,
                    max_value: max_v as u16,
                    brightness: brightness as u32,
                    overexposured,
                    points: star_points,
                    width: width as usize,
                    height: height as usize,
                });
            }
        }

        stars.sort_by_key(|star| -(star.brightness as i32));

        if stars.len() > MAX_STARS_CNT {
            stars.drain(MAX_STARS_CNT..);
        }

        stars
    }

    fn check_is_star_points_ok(star_points: &HashSet<(isize, isize)>) -> bool {
        let real_perimeter = star_points
            .iter()
            .map(|&(x, y)| {
                if star_points.contains(&(x-1, y))
                ||star_points.contains(&(x+1, y))
                ||star_points.contains(&(x, y+1))
                ||star_points.contains(&(x, y-1)) {
                    1
                } else {
                    0
                }
            })
            .sum::<usize>() as f64;
        let possible_s = star_points.len() as f64;
        let possible_r = f64::sqrt(possible_s / PI);
        let possible_perimeter = 2.0 * PI * possible_r;
        real_perimeter < 3.0 * possible_perimeter
    }

    fn calc_common_star_image(image: &ImageLayer<u16>, stars: &[Star], k: usize) -> ImageLayer<u16> {
        let stars_for_image: Vec<_> = stars.iter()
            .filter(|s| !s.overexposured)
            .take(MAX_STARS_FOR_STAR_IMAGE)
            .map(|s| (s, (s.max_value - s.background) as i32))
            .collect();
        if stars_for_image.is_empty() {
            return ImageLayer::new_empty();
        }
        let aver_width = stars_for_image.iter().map(|(s, _)| s.width).sum::<usize>() / stars_for_image.len();
        let aver_height = stars_for_image.iter().map(|(s, _)| s.height).sum::<usize>() / stars_for_image.len();
        let mut result_width = usize::min(aver_width, MAX_STAR_DIAM) * k;
        if result_width % 2 == 0 { result_width += 1; }
        let result_width2 = (result_width / 2) as isize;
        let mut result_height = usize::min(aver_height, MAX_STAR_DIAM) * k;
        if result_height % 2 == 0 { result_height += 1; }
        let result_height2 = (result_height / 2) as isize;
        let mut result = ImageLayer::new_with_size(result_width, result_height);
        let k_f = 1.0 / k as f64;
        let mut values = Vec::new();
        for (i, dst) in result.as_slice_mut().iter_mut().enumerate() {
            let x = i % result_width;
            let y = i / result_width;
            let x_f = k_f * (x as isize - result_width2) as f64;
            let y_f = k_f * (y as isize - result_height2) as f64;
            values.clear();
            for (s, r) in &stars_for_image {
                if let Some(v) = image.get_f64_crd(s.x + x_f, s.y + y_f) {
                    values.push(u16::MAX as i64 * (v as i64 - s.background as i64) / *r as i64);
                }
            }
            if !values.is_empty() {
                let pos = values.len() / 2;
                let mut median = *values.select_nth_unstable(pos).1;
                if median < 0 { median = 0; }
                if median > u16::MAX as i64 { median = u16::MAX as i64; }
                *dst = median as u16;
            }
        }
        result
    }

    fn calc_fwhm(star_image: &ImageLayer<u16>) -> Option<f64> {
        if star_image.is_empty() {
            return None;
        }
        let above_cnt = star_image
            .as_slice()
            .iter()
            .filter(|&v| *v > u16::MAX / 2)
            .count();
        Some(above_cnt as f64)
    }

    fn calc_ovality(star_image: &ImageLayer<u16>) -> Option<f64> {
        if star_image.is_empty() {
            return None;
        }
        const ANGLE_CNT: usize = 32;
        const K: usize = 16;
        let center_x = (star_image.width() / 2) as f64;
        let center_y = (star_image.height() / 2) as f64;
        let size = (usize::max(star_image.width(), star_image.height()) * K) as i32;
        let mut diamemters = Vec::new();
        for i in 0..ANGLE_CNT {
            let angle = PI * (i as f64) / (ANGLE_CNT as f64);
            let cos_angle = f64::cos(angle);
            let sin_angle = f64::sin(angle);
            let mut inside_star_count1 = 0_usize;
            let mut inside_star = false;
            for j in -size/2..0 {
                let k = j as f64 / K as f64;
                let x = k * cos_angle + center_x;
                let y = k * sin_angle + center_y;
                if let Some(v) = star_image.get_f64_crd(x, y) {
                    if v >= u16::MAX/2 { inside_star = true; }
                }
                if inside_star { inside_star_count1 += 1; }
            }
            let mut inside_star = false;
            let mut inside_star_count2 = 0_usize;
            for j in (1..size/2).rev() {
                let k = j as f64 / K as f64;
                let x = k * cos_angle + center_x;
                let y = k * sin_angle + center_y;
                if let Some(v) = star_image.get_f64_crd(x, y) {
                    if v >= u16::MAX/2 { inside_star = true; }
                }
                if inside_star { inside_star_count2 += 1; }
            }
            let inside_star_count = 2 * usize::min(inside_star_count1, inside_star_count2);
            diamemters.push(inside_star_count);
        }
        let max_diam_pos = diamemters.iter().copied().position_max().unwrap_or_default();
        let min_diam_pos = (max_diam_pos + ANGLE_CNT/2) % ANGLE_CNT;
        let max_diameter = diamemters[max_diam_pos] as f64;
        let min_diameter = diamemters[min_diam_pos] as f64;
        let diff = max_diameter - min_diameter;
        Some(diff / K as f64)
    }
}

struct IirFilterCoeffs {
    a0: f32,
    b0: f32,
}

impl IirFilterCoeffs {
    fn new(b0: f32) -> IirFilterCoeffs {
        IirFilterCoeffs {
            a0: 1.0 - b0,
            b0: b0,
        }
    }
}

struct IirFilter {
    y0: Option<f32>,
}

impl IirFilter {
    fn new() -> Self {
        Self {
            y0: None,
        }
    }

    fn set_first_time(&mut self) {
        self.y0 = None;
    }

    fn filter(&mut self, coeffs: &IirFilterCoeffs, x: f32) -> f32 {
        let result = coeffs.a0 * x + coeffs.b0 * self.y0.unwrap_or(x);
        self.y0 = Some(result);
        result
    }

    #[inline(never)]
    fn filter_direct_and_revert_u16(&mut self, coeffs: &IirFilterCoeffs, src: &[u16], dst: &mut [u16]) {
        self.filter_direct_u16(coeffs, src, dst);
        self.filter_revert_u16(coeffs, src, dst);
    }

    #[inline(never)]
    fn filter_direct_u16(&mut self, coeffs: &IirFilterCoeffs, src: &[u16], dst: &mut [u16]) {
        self.set_first_time();
        for (s, d) in izip!(src, dst) {
            let mut res = self.filter(coeffs, *s as f32);
            if res < 0.0 { res = 0.0; }
            if res > u16::MAX as f32 { res = u16::MAX as f32; }
            *d = res as u16;
        }
    }

    #[inline(never)]
    fn filter_revert_u16(&mut self, coeffs: &IirFilterCoeffs, src: &[u16], dst: &mut [u16]) {
        self.set_first_time();
        for (s, d) in izip!(src.iter().rev(), dst.iter_mut().rev()) {
            let mut res = (self.filter(coeffs, *s as f32) + *d as f32) * 0.5;
            if res < 0.0 { res = 0.0; }
            if res > u16::MAX as f32 { res = u16::MAX as f32; }
            *d = res as u16;
        }
    }
}


struct FloodFiller {
    visited: VecDeque<(isize, isize)>,
}

impl FloodFiller {
    fn new() -> FloodFiller {
        FloodFiller {
            visited: VecDeque::new(),
        }
    }

    fn fill<SetFilled: FnMut(isize, isize) -> bool>(
        &mut self,
        x: isize,
        y: isize,
        mut try_set_filled: SetFilled
    ) {
        if !try_set_filled(x, y) { return; }

        self.visited.clear();
        self.visited.push_back((x, y));

        while let Some((pt_x, pt_y)) = self.visited.pop_front() {
            let mut check_neibour = |x, y| {
                if !try_set_filled(x, y) { return; }
                self.visited.push_back((x, y));
            };
            check_neibour(pt_x-1, pt_y);
            check_neibour(pt_x+1, pt_y);
            check_neibour(pt_x, pt_y-1);
            check_neibour(pt_x, pt_y+1);
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
            median: h.get_nth_element(h.count / 2),
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

    pub fn get_nth_element(&self, mut n: usize) -> u16 {
        for (idx, v) in self.freq.iter().enumerate() {
            if n < *v as usize {
                return idx as u16;
            }
            n -= *v as usize;
        }
        u16::MAX
    }

    pub fn get_percentile(&self, n: usize) -> u16 {
        self.get_nth_element(n * self.count / 100)
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
        img:         &RawImage,
        monochrome:  bool,
        _mt:         bool
    ) {
        self.max = img.info().max_value;
        if img.info().cfa == CfaType::None || monochrome {
            let mut l = self.l.take().unwrap_or(HistogramChan::new());
            let data = img.as_slice();

            let mut l_freq = [0u32; u16::MAX as usize + 1];
            let mut sum = 0_u64;
            for value in data {
                l_freq[*value as usize] += 1;
                sum += *value as u64;
            }
            l.freq.clear();
            l.freq.extend_from_slice(&l_freq[..(img.info().max_value as usize + 1)]);
            l.count = data.len();
            l.mean = sum as f64 / data.len() as f64;
            let mut sum = 0_f64;
            for (value, cnt) in l.freq.iter().enumerate() {
                let diff = l.mean - value as f64;
                sum += *cnt as f64 * diff * diff;
            }
            l.std_dev = f64::sqrt(sum / data.len() as f64);
            self.l = Some(l);
            self.r = None;
            self.g = None;
            self.b = None;
        } else {
            let mut r = self.r.take().unwrap_or(HistogramChan::new());
            let mut g = self.g.take().unwrap_or(HistogramChan::new());
            let mut b = self.b.take().unwrap_or(HistogramChan::new());

            let mut r_sum = 0_u64;
            let mut r_cnt = 0_u32;
            let mut g_sum = 0_u64;
            let mut g_cnt = 0_u32;
            let mut b_sum = 0_u64;
            let mut b_cnt = 0_u32;

            let mut r_freq = [0u32; u16::MAX as usize + 1];
            let mut g_freq = [0u32; u16::MAX as usize + 1];
            let mut b_freq = [0u32; u16::MAX as usize + 1];

            for y in 0..img.info().height {
                let row = img.row(y);
                let cfa = img.cfa_row(y);
                for (v, c) in izip!(row, cfa.iter().cycle()) {
                    match *c {
                        CfaColor::R => {
                            r_freq[*v as usize] += 1;
                            r_sum += *v as u64;
                            r_cnt += 1;
                        },
                        CfaColor::G => {
                            g_freq[*v as usize] += 1;
                            g_sum += *v as u64;
                            g_cnt += 1;
                        },
                        CfaColor::B => {
                            b_freq[*v as usize] += 1;
                            b_sum += *v as u64;
                            b_cnt += 1;
                        },
                        _ => {},
                    }
                }
            }

            r.freq.clear();
            r.freq.extend_from_slice(&r_freq[..img.info().max_value as usize + 1]);
            r.count = r_cnt as usize;
            r.mean = r_sum as f64 / r_cnt as f64;

            g.freq.clear();
            g.freq.extend_from_slice(&g_freq[..img.info().max_value as usize + 1]);
            g.count = g_cnt as usize;
            g.mean = g_sum as f64 / g_cnt as f64;

            b.freq.clear();
            b.freq.extend_from_slice(&b_freq[..img.info().max_value as usize + 1]);
            b.count = b_cnt as usize;
            b.mean = b_sum as f64 / b_cnt as f64;

            let mut r_sum = 0_f64;
            for (value, cnt) in r.freq.iter().enumerate() {
                let diff = r.mean - value as f64;
                r_sum += *cnt as f64 * diff * diff;
            }
            r.std_dev = f64::sqrt(r_sum / r_cnt as f64);

            let mut g_sum = 0_f64;
            for (value, cnt) in g.freq.iter().enumerate() {
                let diff = g.mean - value as f64;
                g_sum += *cnt as f64 * diff * diff;
            }
            g.std_dev = f64::sqrt(g_sum / g_cnt as f64);

            let mut b_sum = 0_f64;
            for (value, cnt) in b.freq.iter().enumerate() {
                let diff = b.mean - value as f64;
                b_sum += *cnt as f64 * diff * diff;
            }
            b.std_dev = f64::sqrt(b_sum / b_cnt as f64);

            self.l = None;
            self.r = Some(r);
            self.g = Some(g);
            self.b = Some(b);
        }
    }

    pub fn from_image(&mut self, img: &Image, mt: bool) {
        let from_image_layer = |
            chan:  Option<HistogramChan>,
            layer: &ImageLayer<u16>,
        | -> Option<HistogramChan> {
            if layer.is_empty() { return None; }
            let mut chan = chan.unwrap_or(HistogramChan::new());
            let mut freq = [0u32; u16::MAX as usize + 1];
            let len = layer.as_slice().len() as f64;
            let mut sum = 0_u64;
            for value in layer.as_slice().iter() {
                sum += *value as u64;
                freq[*value as usize] += 1;
            }
            chan.freq.clear();
            chan.freq.extend_from_slice(&freq[..img.max_value() as usize + 1]);
            chan.count = layer.as_slice().len();
            chan.mean = sum as f64 / len;
            let mut sum = 0_f64;
            for (value, cnt) in chan.freq.iter().enumerate() {
                let diff = chan.mean - value as f64;
                sum += *cnt as f64 * diff * diff;
            }
            chan.std_dev = f64::sqrt(sum / len);
            chan.freq.resize(img.max_value() as usize + 1, 0);
            Some(chan)
        };

        self.max = img.max_value();
        if !mt {
            self.l = from_image_layer(self.l.take(), &img.l);
            self.r = from_image_layer(self.l.take(), &img.r);
            self.g = from_image_layer(self.l.take(), &img.g);
            self.b = from_image_layer(self.l.take(), &img.b);
        } else {
            rayon::scope(|s| {
                s.spawn(|_| self.r = from_image_layer(self.r.take(), &img.r));
                s.spawn(|_| self.g = from_image_layer(self.g.take(), &img.g));
                s.spawn(|_| self.b = from_image_layer(self.b.take(), &img.b));
                s.spawn(|_| self.l = from_image_layer(self.l.take(), &img.l));
            });
        }
    }
}