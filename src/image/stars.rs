use std::{collections::{HashMap, HashSet, VecDeque}, f64::consts::PI, isize, sync::Mutex};
use itertools::Itertools;
use crate::{utils::math::*, TimeLogger};
use super::{image::ImageLayer, raw::RawImageInfo};

const MAX_STAR_DIAM: usize = 32;
const MAX_STARS_CNT: usize = 500;
const MAX_STARS_FOR_STAR_IMAGE: usize = 200;

#[derive(Clone, Default)]
pub struct Star {
    pub x:             f64,
    pub y:             f64,
    pub background:    u16,
    pub max_value:     u16,
    pub brightness:    u32,
    pub overexposured: bool,
    pub width:         usize,
    pub height:        usize,
    pub points:        Vec<(usize, usize)>,
}

pub type StarItems = Vec<Star>;

#[derive(Default)]
pub struct StarsInfo {
    pub fwhm:          Option<f32>,
    pub fwhm_angular:  Option<f32>,
    pub fwhm_is_ok:    bool,
    pub ovality:       Option<f32>,
    pub ovality_is_ok: bool,
}

impl StarsInfo {
    pub fn is_ok(&self) -> bool {
        self.fwhm_is_ok && self.ovality_is_ok
    }
}

#[derive(Default)]
pub struct Stars {
    pub items: StarItems,
    pub info:  StarsInfo,
}

impl Stars {
    pub fn new_from_image(
        image:              &ImageLayer<u16>,
        raw_info:           &Option<RawImageInfo>,
        max_stars_fwhm:     Option<f32>,
        max_stars_ovality:  Option<f32>,
        mt:                 bool
    ) -> Self {
        let items = Self::find_stars_in_image(&image, mt);

        const COMMON_STAR_MAG: usize = 4;
        const COMMON_STAR_MAG_F: f64 = COMMON_STAR_MAG as f64;
        let star_img = Self::calc_common_star_image(image, &items, COMMON_STAR_MAG);
        let fwhm = Self::calc_fwhm(&star_img)
            .map(|v| (v / (COMMON_STAR_MAG_F * COMMON_STAR_MAG_F)) as f32);
        let ovality = Self::calc_ovality(&star_img)
            .map(|v| (v / COMMON_STAR_MAG_F) as f32);

        let fwhm_is_ok = if let Some(max_stars_fwhm) = max_stars_fwhm {
            fwhm.unwrap_or(999.0) < max_stars_fwhm
        } else {
            true
        };

        let ovality_is_ok = if let Some(max_stars_ovality) = max_stars_ovality {
            ovality.unwrap_or(999.0) < max_stars_ovality
        } else {
            true
        };

        let fwhm_angular = Self::calc_angular_fwhm(fwhm, raw_info);

        let info = StarsInfo {
            fwhm,
            fwhm_angular,
            fwhm_is_ok,
            ovality,
            ovality_is_ok,
        };

        Self { items, info }
    }

    fn find_border_for_stars_detection(image: &ImageLayer<u16>) -> u16 {
        let mut diffs = Vec::new();
        for row in image.as_slice().chunks_exact(image.width()) {
            for area in row.chunks_exact(MAX_STAR_DIAM) {
                let Some(&[b1, b2, b3, b4, b5]) = area.first_chunk::<5>() else { continue; };
                let Some(&[e1, e2, e3, e4, e5]) = area.last_chunk::<5>() else { continue; };
                let area_middle = area.len() / 2;
                let &[c1, c2, c3] = &area[area_middle-1..=area_middle+1] else { continue; };
                let begin = median5(b1, b2, b3, b4, b5) as i32;
                let end = median5(e1, e2, e3, e4, e5) as i32;
                let center = median3(c1, c2, c3) as i32;
                let diff = begin + end - 2 * center;
                if diff > i16::MAX as i32 || diff < -i16::MAX as i32 {
                    continue;
                }
                diffs.push(diff * diff);
            }
        }

        let m = median(&mut diffs);
        let diff = 0.5 * f64::sqrt(m as _);

        let result = (20.0 * diff) as i32;
        let result = i32::max(result, 1);
        let result = i32::min(result, u16::MAX as _);

        result as _
    }

    fn find_stars_in_image(image: &ImageLayer<u16>, mt: bool) -> StarItems {
        const MAX_STARS_POINTS_CNT: usize = MAX_STAR_DIAM * MAX_STAR_DIAM;
        let iir_filter_coeffs = IirFilterCoeffs::new(230);
        let border = Self::find_border_for_stars_detection(image) as u32;
        let possible_stars = Mutex::new(HashMap::new());
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
                in row.iter().tuple_windows().zip(&filtered[3..]).enumerate() {
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
                        possible_stars.lock().unwrap().insert((star_x, star_y), f2 / 3);
                    }
                }
            }
        };

        let tm = TimeLogger::start();

        if !mt {
            find_possible_stars_in_rows(0, image.height());
        } else {
            let max_threads = rayon::current_num_threads();
            let tasks_cnt = if max_threads != 1 { 2 * max_threads  } else { 1 };
            let image_height = image.height();
            rayon::scope(|s| {
                for t in 0..tasks_cnt {
                    let y1 = t * image_height / tasks_cnt;
                    let y2 = (t + 1) * image_height / tasks_cnt;
                    s.spawn(move |_| { find_possible_stars_in_rows(y1, y2); });
                }
            });
        }
        let possible_stars = possible_stars.into_inner().unwrap();
        tm.log("find_possible_stars_in_rows");

        // Optimization: find point clusters and leave brightest point from each cluster

        let mut processed_points = HashSet::<(isize, isize)>::new();
        let mut cluster_filler = FloodFiller::new();
        let mut cluster = Vec::new();
        let mut possible_star_centers = Vec::new();
        for (crd, _) in &possible_stars {
            if processed_points.contains(crd) { continue; }
            let (x, y) = crd;
            cluster.clear();
            cluster_filler.fill(*x, *y, |x, y| -> FillPtSetResult {
                if let Some(existing) = possible_stars.get(&(x, y)) {
                    if processed_points.contains(&(x, y)) {
                        return FillPtSetResult::Miss
                    }
                    cluster.push((x, y, *existing));
                    processed_points.insert((x, y));
                    return FillPtSetResult::Hit
                };
                FillPtSetResult::Miss
            });

            cluster.sort_by_key(|(_, _, br)| *br);

            if let Some(last) = cluster.last() {
                possible_star_centers.push(last.clone());
            }
        }
        possible_star_centers.sort_by_key(|(_, _, v)| -(*v as i32));
        if possible_star_centers.len() >  MAX_STARS_CNT {
            possible_star_centers.drain(MAX_STARS_CNT..);
        }

        let mut overexp_buffer = Vec::new();

        let mut all_star_coords = HashSet::<(isize, isize)>::new();
        let mut flood_filler = FloodFiller::new();
        let mut stars = Vec::new();
        let mut star_bg_values = Vec::new();
        let max_stars_points = image.width() * image.height() / 100; // 1% of area maximum
        let mut wrong_cnt = 0_usize;
        for (x, y, max_v) in possible_star_centers {
            if all_star_coords.contains(&(x, y)) { continue; }
            if all_star_coords.len() > max_stars_points
            || wrong_cnt > 1000 {
                return StarItems::new();
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
            let mut star_points = HashMap::new();
            let half_max = bg as i32 + (max_v as i32 - bg as i32) / 3;
            if half_max <= 0 { continue; }
            let half_max = half_max as u16;
            let mut x_summ = 0_f64;
            let mut y_summ = 0_f64;
            let mut crd_cnt = 0_f64;
            let mut brightness = 0_i32;
            let mut min_x = isize::MAX;
            let mut max_x = isize::MIN;
            let mut min_y = isize::MAX;
            let mut max_y = isize::MIN;
            let fill_ok = flood_filler.fill(
                x,
                y,
                |x, y| -> FillPtSetResult {
                    let v = image.get(x, y).unwrap_or(0);
                    let more_than_half_max = v >= half_max;
                    if more_than_half_max {
                        if all_star_coords.contains(&(x, y))
                        || star_points.contains_key(&(x, y)) {
                            return FillPtSetResult::Miss;
                        }

                        if star_points.len() > MAX_STARS_POINTS_CNT {
                            return FillPtSetResult::Error;
                        }
                        if x < min_x { min_x = x; }
                        if x > max_x { max_x = x; }
                        if y < min_y { min_y = y; }
                        if y > max_y { max_y = y; }

                        if max_x - min_x > MAX_STAR_DIAM as isize
                        || max_y - min_y > MAX_STAR_DIAM as isize {
                            return FillPtSetResult::Error;
                        }

                        star_points.insert((x, y), v);
                        all_star_coords.insert((x, y));
                        let v_part = linear_interpolate(v as f64, bg as f64, max_v as f64, 0.0, 1.0);
                        x_summ += v_part * x as f64;
                        y_summ += v_part * y as f64;
                        crd_cnt += v_part;
                        brightness += v as i32 - bg as i32;
                        FillPtSetResult::Hit
                    } else {
                        FillPtSetResult::Miss
                    }
                }
            );

            if !fill_ok {
                wrong_cnt += 1;
                continue;
            }

            if max_v > bg as u32
            && brightness > 0
            && Self::check_is_star_points_ok(&star_points) {
                let min_x = star_points.keys().map(|(x, _)| *x).min().unwrap_or(x);
                let max_x = star_points.keys().map(|(x, _)| *x).max().unwrap_or(x);
                let min_y = star_points.keys().map(|(_, y)| *y).min().unwrap_or(y);
                let max_y = star_points.keys().map(|(_, y)| *y).max().unwrap_or(y);
                let width = 3 * isize::max(x-min_x+1, max_x-x+1);
                let height = 3 * isize::max(y-min_y+1, max_y-y+1);
                let center_x = x_summ / crd_cnt;
                let center_y = y_summ / crd_cnt;
                let overexposured = Self::check_is_star_overexposured(
                    &star_points,
                    center_x.round() as isize,
                    center_y.round() as isize,
                    min_x, min_y,
                    max_x, max_y,
                    bg,
                    &mut overexp_buffer
                );
                let points = star_points.keys()
                    .map(|(x, y)| (*x as usize, *y as usize))
                    .collect();
                stars.push(Star {
                    x: center_x,
                    y: center_y,
                    background: bg,
                    max_value: max_v as u16,
                    brightness: brightness as u32,
                    width: width as usize,
                    height: height as usize,
                    points,
                    overexposured,
                });
            }
        }

        stars.sort_by_key(|star| -(star.brightness as i32));

        if stars.len() > MAX_STARS_CNT {
            stars.drain(MAX_STARS_CNT..);
        }

        stars
    }

    fn check_is_star_points_ok(star_points: &HashMap<(isize, isize), u16>) -> bool {
        let real_perimeter = star_points
            .keys()
            .map(|&(x, y)| {
                if star_points.contains_key(&(x-1, y))
                ||star_points.contains_key(&(x+1, y))
                ||star_points.contains_key(&(x, y+1))
                ||star_points.contains_key(&(x, y-1)) {
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

    fn check_is_star_overexposured(
        star_points: &HashMap<(isize, isize), u16>,
        center_x:    isize,
        center_y:    isize,
        min_x:       isize,
        min_y:       isize,
        max_x:       isize,
        max_y:       isize,
        bg:          u16,
        buffer:      &mut Vec<u16>,
    ) -> bool {
        fn is_overexposured_values(values: &[u16], bg: u16) -> bool {
            let points_count = values.len();
            if points_count < 4 {
                return false;
            }
            let max = values.iter().max().copied().unwrap_or_default();
            let range = max - bg;
            let plateau_border = max - range / 10;
            let under_plateau_cnt = values.iter().filter(|v| **v > plateau_border).count();
            under_plateau_cnt > points_count/4
        }

        // check horizontally

        buffer.clear();
        for x in min_x..=max_x {
            if let Some(v) = star_points.get(&(x, center_y)) {
                buffer.push(*v);
            }
        }
        if !is_overexposured_values(&buffer, bg) {
            return false;
        }

        // check vertically

        buffer.clear();
        for y in min_y..=max_y {
            if let Some(v) = star_points.get(&(center_x, y)) {
                buffer.push(*v);
            }
        }
        is_overexposured_values(&buffer, bg)
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

    fn calc_angular_fwhm(fwhm: Option<f32>, raw_info: &Option<RawImageInfo>) -> Option<f32> {
        let Some(fwhm) = fwhm else { return None; };
        let Some(raw_info) = raw_info else { return None; };
        let Some(focal_len) = raw_info.focal_len else { return None; };
        let Some(pixel_size_x) = raw_info.pixel_size_x else { return None; };
        let Some(pixel_size_y) = raw_info.pixel_size_y else { return None; };

        let pixel_size = 0.5 * (pixel_size_x + pixel_size_y);
        let pixel_size_m = pixel_size / 1_000_000.0;
        let r = f64::sqrt(fwhm as f64 / PI) * pixel_size_m;
        let focal_len_m = focal_len / 1000.0;
        let result = 2.0 * f64::atan2(r, focal_len_m);

        Some(result as f32)
    }
}

struct FloodFiller {
    visited: VecDeque<(isize, isize)>,
}

#[derive(PartialEq)]
pub enum FillPtSetResult {
    Hit,
    Miss,
    Error
}

impl FloodFiller {
    fn new() -> FloodFiller {
        FloodFiller {
            visited: VecDeque::new(),
        }
    }

    fn fill<SetFilled: FnMut(isize, isize) -> FillPtSetResult>(
        &mut self,
        x: isize,
        y: isize,
        mut try_set_filled: SetFilled
    ) -> bool {
        match try_set_filled(x, y) {
            FillPtSetResult::Miss => return true,
            FillPtSetResult::Error=> return false,
            _ => {},
        };

        self.visited.clear();
        self.visited.push_back((x, y));

        let mut error_flag = false;
        while let Some((pt_x, pt_y)) = self.visited.pop_front() {
            let mut check_neibour = |x, y| {
                let result = try_set_filled(x, y);
                if result == FillPtSetResult::Error {
                    error_flag = true;
                }
                if result != FillPtSetResult::Hit { return; }
                self.visited.push_back((x, y));
            };
            check_neibour(pt_x-1, pt_y);
            check_neibour(pt_x+1, pt_y);
            check_neibour(pt_x, pt_y-1);
            check_neibour(pt_x, pt_y+1);
            if error_flag { return false; }
        }
        true
    }
}
