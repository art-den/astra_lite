use std::{collections::{HashMap, HashSet}, f64::consts::PI, sync::atomic::{AtomicUsize, Ordering}};
use itertools::{izip, Itertools};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use crate::{options::StarRecognSensitivity, utils::{math::*, log_utils::TimeLogger}};
use super::{image::ImageLayer, raw::RawImageInfo, utils::*};

const MAX_STAR_DIAM: usize = 32;
const MAX_STARS_POINTS_CNT: usize = MAX_STAR_DIAM * MAX_STAR_DIAM;
const MAX_STARS_CNT: usize = 500;
const MAX_STARS_CNT_FOR_STAR_IMAGE: usize = 100;

// Saturation plateau detector: a clean Gaussian core has <= ~0.2 of its
// flood-fill area within 10 % of the own peak; a clipped star has >= ~0.45
const PLATEAU_BORDER_DIV: u16 = 10;
const PLATEAU_MIN_CORE_AREA: usize = 5;  // smaller cores are too discrete for the fraction
const PLATEAU_MIN_CNT: usize = 5;
const PLATEAU_FRACTION_NUM: usize = 7;   // flag if plateau fraction > 7/20
const PLATEAU_FRACTION_DEN: usize = 20;

#[derive(Clone, Default)]
pub struct Star {
    pub x:           f64,
    pub y:           f64,
    pub background:  u16,
    pub max_value:   u16,
    pub brightness:  u32,
    pub overexposed: bool,
    pub width:       usize,
    pub height:      usize,
    pub points:      Vec<(usize, usize)>,
}

pub type StarItems = Vec<Star>;

#[derive(Default, Clone)]
pub struct StarsInfo {
    pub hfd:           Option<f32>, // half flux diameter
    pub fwhm:          Option<f32>, // full width at half maximum
    pub fwhm_angular:  Option<f32>,
    pub ovality:       Option<f32>,
}

#[derive(Default)]
pub struct Stars {
    pub items: StarItems,
    pub info:  StarsInfo,
}

#[derive(Clone, Copy)]
struct Extremum {
    peak: u16,
    // Coarse row-median background at scan time, not the star's true background
    coarse_bg: u16,
}

impl Extremum {
    fn contrast(&self) -> i32 {
        self.peak as i32 - self.coarse_bg as i32
    }
}

pub struct StarsFinder;

impl StarsFinder {
    pub fn new() -> Self {
        Self
    }

    pub fn find_stars_and_get_info(
        &self,
        image:            &ImageLayer<u16>,
        raw_info:         &Option<RawImageInfo>,
        sensitivity:      StarRecognSensitivity,
        ignore_3px_stars: bool,
        mt:               bool,
    ) -> eyre::Result<Stars> {
        log::debug!(
            "StarsFinder::find_stars_and_get_info ignore_3px_stars={}, mt={}",
            ignore_3px_stars, mt
        );

        let tm_total = TimeLogger::start();

        let tm = TimeLogger::start();
        let mut threshold = Self::calc_threshold_for_stars_detection(image, sensitivity)?;
        tm.log("StarsFinder::calc_threshold_for_stars_detection");
        log::debug!("Stars detection threshold = {}", threshold);

        let tm = TimeLogger::start();
        let star_centers = Self::find_possible_stars_centers(image, &mut threshold, mt);
        tm.log("StarsFinder::find_possible_stars_centers");
        log::debug!("Stars star_centers.len() = {}", star_centers.len());

        let tm = TimeLogger::start();
        let stars = Self::get_stars(&star_centers, image, threshold, ignore_3px_stars);
        tm.log("Stars::get_stars");
        log::debug!("Stars stars.len() = {}", stars.len());

        const COMMON_STAR_MAG: usize = 4;
        let tm = TimeLogger::start();
        let star_img = Self::calc_common_star_image(image, &stars, COMMON_STAR_MAG);
        tm.log("StarsFinder::calc_common_star_image");

        let (hfd, fwhm, ovality) = if let Some(star_img) = star_img {
            (star_img.calc_hfd(), star_img.calc_fwhm(), star_img.calc_ovality())
        } else {
            (None, None, None)
        };

        let fwhm_angular = CommonStarsImage::calc_angular_fwhm(fwhm, raw_info);

        let info = StarsInfo {
            hfd,
            fwhm,
            fwhm_angular,
            ovality,
        };

        tm_total.log("StarsFinder TOTAL");

        Ok(Stars { items: stars, info })
    }

    fn calc_threshold_for_stars_detection(
        image:       &ImageLayer<u16>,
        sensitivity: StarRecognSensitivity,
    ) -> eyre::Result<u16> {
        // Bail early: chunking/indexing below panics on tiny images
        if image.width() < MAX_STAR_DIAM || image.height() < MAX_STAR_DIAM {
            eyre::bail!("Image is too small for stars detection");
        }

        let mut diffs = Vec::new();
        for (y, row) in image.as_slice().chunks_exact(image.width()).enumerate() {
            let start = y & 0xF;
            for area in row[start..].as_chunks::<MAX_STAR_DIAM>().0 {
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

        if diffs.is_empty() {
            eyre::bail!("Image is too small for stars detection");
        }

        let m = median(&mut diffs);
        let diff = 0.5 * f64::sqrt(m as _);

        let k = match sensitivity {
            StarRecognSensitivity::Low    => 30.0,
            StarRecognSensitivity::Normal => 15.0,
            StarRecognSensitivity::High   => 9.0,
        };

        let result = (k * diff) as i32;
        let result = i32::max(result, 1);
        let result = i32::min(result, u16::MAX as _);

        Ok(result as _)
    }

    fn find_extremums_try(
        image:     &ImageLayer<u16>,
        threshold: u16,
        limit:     usize,
        mt:        bool,
    ) -> Option<HashMap<(isize, isize), Extremum>> {
        let process_rows =
            |image: &ImageLayer<u16>, threshold: u16, y1: usize, y2: usize,
             out: &mut Vec<((isize, isize), Extremum)>, cnt: &AtomicUsize| {
            let mut filtered = vec![0; image.width()];
            for y in y1..y2 {
                if cnt.load(Ordering::Relaxed) >= limit {
                    return;
                }

                if y < MAX_STAR_DIAM/2 || y > image.height()-MAX_STAR_DIAM/2 {
                    // height >= MAX_STAR_DIAM is guaranteed upstream by calc_threshold_for_stars_detection,
                    // otherwise the subtraction below underflows
                    continue;
                }
                let row = image.row(y);

                const MEDIAN_FILTER_CHUNK_SIZE: usize = 9 * 8;
                for (r, f) in izip!(
                    row.chunks(MEDIAN_FILTER_CHUNK_SIZE),
                    filtered.chunks_mut(MEDIAN_FILTER_CHUNK_SIZE)
                ) {
                    if r.len() == MEDIAN_FILTER_CHUNK_SIZE {
                        let m = median9(
                            r[4],  r[12], r[20],
                            r[28], r[36], r[44],
                            r[52], r[60], r[68]
                        );
                        f.fill(m);
                    } else {
                        f.copy_from_slice(r);
                        let m = median(f);
                        f.fill(m);
                    }
                }

                const FILTERED_OFFSET: usize = 5;
                //       0   1   2   3   4  >5<  6   7   8   9   10
                for (i, (l1, l2, l3, l4, s1, s2, s3, r1, r2, r3, r4), f)
                in izip!(FILTERED_OFFSET.., row.iter().tuple_windows(), &filtered[FILTERED_OFFSET..]) {
                    let star_x = i as isize;
                    let star_y = y as isize;
                    if star_x < (MAX_STAR_DIAM/2) as isize
                    || star_y < (MAX_STAR_DIAM/2) as isize
                    || star_x > (image.width() - MAX_STAR_DIAM/2) as isize
                    || star_y > (image.height() - MAX_STAR_DIAM/2) as isize {
                        continue; // skip points near image border
                    }

                    let s = median3(*s1, *s2, *s3);
                    if s < *f || (s - *f) < threshold { continue; }
                    let l = median4_u16(*l1, *l2, *l3, *l4);
                    if l >= s { continue; }
                    let r = median4_u16(*r1, *r2, *r3, *r4);
                    if r >= s { continue; }

                    if cnt.fetch_add(1, Ordering::Relaxed) >= limit {
                        break;
                    }
                    out.push(((star_x, star_y), Extremum {peak: s, coarse_bg: *f}));
                }
            }
        };

        let counter = AtomicUsize::new(0);
        let image_height = image.height();
        let results: Vec<((isize, isize), Extremum)> = if !mt {
            let mut out = Vec::new();
            process_rows(image, threshold, 0, image_height, &mut out, &counter);
            out
        } else {
            let num_threads = rayon::current_num_threads();
            let chunk = ((image_height + num_threads - 1) / num_threads).max(1);
            let num_chunks = (image_height + chunk - 1) / chunk;
            (0..num_chunks)
                .into_par_iter()
                .map(|i| {
                    let start = i * chunk;
                    let end = (start + chunk).min(image_height);
                    let mut out = Vec::new();
                    process_rows(image, threshold, start, end, &mut out, &counter);
                    out
                })
                .flatten()
                .collect()
        };

        if counter.load(Ordering::Relaxed) >= limit {
            return None;
        }

        Some(results.into_iter().collect())
    }

    fn find_possible_stars_centers(
        image:     &ImageLayer<u16>,
        threshold: &mut u16,
        mt:        bool,
    ) -> Vec<(isize, isize, Extremum)> {
        const MAX_WEAK_CENTERS_CNT: usize = 10 * MAX_STARS_CNT;
        // Memory guard only: reaching it aborts the scan (None) and bumps the threshold
        const MAX_EXTREMUMS_CNT: usize = 100 * MAX_STARS_CNT;
        // A center is "weak" if its contrast is below 1.5 * threshold (empirical factor):
        // a population hugging the threshold means the noise estimate collapsed
        const WEAK_CONTRAST_NUM: i32 = 3;
        const WEAK_CONTRAST_DEN: i32 = 2;
        const MAX_CENTERS_CNT: usize = 5 * MAX_STARS_CNT;

        let mut result = Vec::new();
        loop {
            let extrema = Self::find_extremums_try(
                image,
                *threshold,
                MAX_EXTREMUMS_CNT,
                mt
            );

            if let Some(extrema) = extrema {
                Self::cluster_extrema(&extrema, &mut result);

                let weak_border = WEAK_CONTRAST_NUM * (*threshold as i32) / WEAK_CONTRAST_DEN;
                let weak_centers_count = result.iter()
                    .filter(|(_, _, extr)| extr.contrast() < weak_border)
                    .count();

                if weak_centers_count < MAX_WEAK_CENTERS_CNT {
                    break;
                }
            }

            // Too many weak centers (or the extrema cap was hit): the threshold
            // is likely underestimated, increase it and repeat

            let new_threshold =  3 * (*threshold as u32 + 1) / 2;
            if new_threshold > u16::MAX as u32 {
                break;
            }
            *threshold = new_threshold as u16;
        }
        result.sort_by_key(|(_, _, extr)| -extr.contrast());
        if result.len() > MAX_CENTERS_CNT {
            result.drain(MAX_CENTERS_CNT..);
        }
        result
    }

    fn cluster_extrema(
        extrema: &HashMap<(isize, isize), Extremum>,
        result:  &mut Vec<(isize, isize, Extremum)>,
    ) {
        let mut processed_points = HashSet::<(isize, isize)>::new();
        let mut cluster_filler = FloodFiller::new();
        let mut cluster = Vec::new();
        result.clear();
        for crd in extrema.keys() {
            if processed_points.contains(crd) { continue; }
            let (x, y) = crd;
            cluster.clear();
            cluster_filler.fill(*x, *y, |x, y| -> FillPtSetResult {
                if let Some(existing) = extrema.get(&(x, y)) {
                    if processed_points.contains(&(x, y)) {
                        return FillPtSetResult::Miss
                    }
                    cluster.push((x, y, *existing));
                    processed_points.insert((x, y));
                    return FillPtSetResult::Hit
                };
                FillPtSetResult::Miss
            });

            cluster.sort_by_key(|(_, _, extr)| extr.peak);

            if let Some(last) = cluster.last() {
                result.push(*last);
            }
        }
    }

    fn get_stars(
        star_centers:     &Vec<(isize, isize, Extremum)>,
        image:            &ImageLayer<u16>,
        threshold:        u16,
        ignore_3px_stars: bool,
    ) -> Vec<Star> {
        let mut all_star_coords = HashSet::<(isize, isize)>::new();
        let mut flood_filler = FloodFiller::new();
        let mut stars = Vec::new();
        let mut star_bg_values = Vec::new();
        let max_all_stars_points = image.width() * image.height() / 10; // 10% of area maximum
        let mut wrong_cnt = 0_usize;
        let mut star_points = HashMap::new();
        let mut star_extra_points = HashMap::new();
        for &(x, y, extr) in star_centers {
            if all_star_coords.contains(&(x, y)) { continue; }
            if all_star_coords.len() > max_all_stars_points {
                return StarItems::new();
            }
            // Too many malformed candidates: stop, keep what was already found
            if wrong_cnt > 1000 { break; }
            star_bg_values.clear();
            // (2 * MAX_STAR_DIAM + 1) square centered at the star, clipped to the image borders
            for v in image.rect_iter(
                x - MAX_STAR_DIAM as isize,
                y - MAX_STAR_DIAM as isize,
                2 * MAX_STAR_DIAM + 1,
                2 * MAX_STAR_DIAM + 1,
            ) {
                star_bg_values.push(v);
            }
            let bg_pos = star_bg_values.len() / 3;
            let bg = *star_bg_values.select_nth_unstable(bg_pos).1;

            if extr.peak <= bg || extr.peak - bg < threshold { continue; }
            let border = (bg as u32 + (extr.peak as u32 - bg as u32 + 1) / 3) as u16;
            if border == 0 { continue; }
            let mut x_summ = 0_f64;
            let mut y_summ = 0_f64;
            let mut crd_cnt = 0_f64;
            let mut brightness = 0_i32;
            let mut min_x = x;
            let mut max_x = x;
            let mut min_y = y;
            let mut max_y = y;
            star_points.clear();

            let fill_ok = flood_filler.fill(
                x,
                y,
                |x, y| -> FillPtSetResult {
                    let v = image.get(x, y).unwrap_or(0);
                    if v >= border {
                        if star_points.contains_key(&(x, y)) {
                            return FillPtSetResult::Miss;
                        }

                        // near other star
                        if all_star_coords.contains(&(x, y))
                        // too many star points
                        || star_points.len() > MAX_STARS_POINTS_CNT {
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
                        let v_part = linear_interpolate(v as f64, bg as f64, extr.peak as f64, 0.0, 1.0);
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

            if star_points.is_empty() { continue; }
            if ignore_3px_stars && star_points.len() <= 3 { continue; }

            if !fill_ok {
                wrong_cnt += 1;
                continue;
            }

            // Detect the saturation plateau on the flood-fill core, before halo inflation
            let overexposed = Self::is_core_overexposed(&star_points, bg);

            for (x, y) in star_points.keys() {
                all_star_coords.insert((*x, *y));
            }

            // inflate star points by one pixel
            star_extra_points.clear();
            for (x, y) in star_points.keys() {
                for dx in -1..=1 {
                    let sx = x + dx;
                    for dy in -1..=1 {
                        if dx == 0 && dy == 0 { continue; }
                        let sy = y + dy;
                        let key = (sx, sy);
                        if star_extra_points.contains_key(&key) { continue; }
                        let v = image.get(sx, sy).unwrap_or(0);
                        star_extra_points.insert(key, v);
                    }
                }
            }
            for ((x, y), v) in &star_extra_points {
                star_points.insert((*x, *y), *v);
            }

            if brightness > 0
            && Self::check_is_star_ok(&star_points) {
                let width = 3 * isize::max(x-min_x+1, max_x-x+1);
                let height = 3 * isize::max(y-min_y+1, max_y-y+1);
                let center_x = x_summ / crd_cnt;
                let center_y = y_summ / crd_cnt;
                let points = star_points.keys()
                    .filter(|(x, y)|
                        *x >= 0 && *y >= 0 &&
                        *x < image.width() as isize &&
                        *y < image.height() as isize
                    )
                    .map(|(x, y)| (*x as usize, *y as usize))
                    .collect();
                stars.push(Star {
                    x: center_x,
                    y: center_y,
                    background: bg,
                    max_value: extr.peak,
                    brightness: brightness as u32,
                    width: width as usize,
                    height: height as usize,
                    points,
                    overexposed,
                });
            }
        }

        stars.sort_by_key(|star| -(star.brightness as i32));

        // remove non-overexposed stars larger than
        // 2 * median value of non-verexposed stars area
        let mut not_overexposed_areas: Vec<_> = stars
            .iter()
            .filter(|s| !s.overexposed)
            .map(|s| s.points.len())
            .collect();

        if !not_overexposed_areas.is_empty() {
            let med = median(&mut not_overexposed_areas);
            let border = med * 2;
            stars.retain(|s| s.overexposed || s.points.len() < border);
        }

        Self::cap_star_count(&mut stars);

        stars
    }

    // Keep <= MAX_STARS_CNT stars, capping overexposed ones so saturated stars
    // cannot crowd clean ones out. The list must be brightness-sorted
    fn cap_star_count(stars: &mut Vec<Star>) {
        const MAX_OVEREXPOSED_STARS_CNT: usize = MAX_STARS_CNT / 2;
        let mut total_cnt = 0;
        let mut over_cnt = 0;
        stars.retain(|s| {
            if total_cnt >= MAX_STARS_CNT { return false; }
            if s.overexposed {
                if over_cnt >= MAX_OVEREXPOSED_STARS_CNT { return false; }
                over_cnt += 1;
            }
            total_cnt += 1;
            true
        });
    }

    fn check_is_star_ok(star_points: &HashMap<(isize, isize), u16>) -> bool {
        let real_perimeter = star_points
            .keys()
            .map(|&(x, y)| {
                if !star_points.contains_key(&(x-1, y))
                || !star_points.contains_key(&(x+1, y))
                || !star_points.contains_key(&(x, y+1))
                || !star_points.contains_key(&(x, y-1)) {
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

    /// Detects a saturation plateau as a large fraction of the flood-fill core
    /// within 10 % of the star's own peak (amplitude independent).
    /// Known misses: tiny cores of undersampled stars and mild clips
    /// (true peak below ~2x the clipping level) have no separable plateau
    fn is_core_overexposed(core: &HashMap<(isize, isize), u16>, bg: u16) -> bool {
        let area = core.len();
        if area < PLATEAU_MIN_CORE_AREA {
            return false;
        }
        let max = *core.values().max().unwrap_or(&0);
        if max <= bg {
            return false;
        }
        let range = max - bg;
        let plateau_border = max - range / PLATEAU_BORDER_DIV;
        let plateau_cnt = core.values().filter(|v| **v > plateau_border).count();
        plateau_cnt >= PLATEAU_MIN_CNT
            && plateau_cnt * PLATEAU_FRACTION_DEN > area * PLATEAU_FRACTION_NUM
    }

    fn calc_common_star_image(
        image: &ImageLayer<u16>,
        stars: &[Star],
        k:     usize
    ) -> Option<CommonStarsImage>{
        let stars_for_image: Vec<_> =
            stars.iter()
                .filter(move |s| { !s.overexposed })
                .map(|s| (s, (s.max_value - s.background) as i32))
                .filter(|(_, r)| *r > 0)
                .collect();
        if stars_for_image.is_empty() {
            return None;
        }

        // Prefer stars near the center of the image

        const MIN_STARS_CNT_FOR_STAR_IMAGE: usize = 50;
        let img_size = (usize::min(image.width(), image.height()) / 2) as f64;
        let img_center_x = (image.width()/2) as f64;
        let img_center_y = (image.height()/2) as f64;
        let dists_to_center = [0.5 * img_size, 0.66 * img_size, 0.75 * img_size, img_size, 1_000_000.0];
        let mut filtered_stars = Vec::new();
        for dists_to_center in dists_to_center {
            filtered_stars.clear();
            for (s, v) in &stars_for_image {
                let dx = s.x - img_center_x;
                let dy = s.y - img_center_y;
                let star_dist_to_center = f64::sqrt(dx * dx + dy * dy);
                if star_dist_to_center < dists_to_center {
                    filtered_stars.push((*s, *v));
                }
                if filtered_stars.len() > MAX_STARS_CNT_FOR_STAR_IMAGE {
                    break;
                }
            }
            if filtered_stars.len() > MIN_STARS_CNT_FOR_STAR_IMAGE {
                break;
            }
        }
        let stars_count = filtered_stars.len();
        if stars_count == 0 {
            return None;
        }

        // Size of image

        let avg_width = filtered_stars
            .iter()
            .map(|(s, _)| s.width)
            .sum::<usize>() / stars_count;
        let avg_height = filtered_stars
            .iter()
            .map(|(s, _)| s.height)
            .sum::<usize>() / stars_count;
        let mut result_width = usize::min(avg_width, MAX_STAR_DIAM) * k;
        if result_width.is_multiple_of(2) { result_width += 1; }
        let result_width2 = (result_width / 2) as isize;
        let mut result_height = usize::min(avg_height, MAX_STAR_DIAM) * k;
        if result_height.is_multiple_of(2) { result_height += 1; }
        let result_height2 = (result_height / 2) as isize;

        // Create image

        let mut result = ImageLayer::new_with_size(result_width, result_height);
        let k_f = 1.0 / k as f64;
        let mut values = Vec::new();
        for (i, dst) in result.as_slice_mut().iter_mut().enumerate() {
            let x = i % result_width;
            let y = i / result_width;
            let x_f = k_f * (x as isize - result_width2) as f64;
            let y_f = k_f * (y as isize - result_height2) as f64;
            values.clear();
            for (s, r) in &filtered_stars {
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

        Some(CommonStarsImage { image: result, k })
    }

}

struct CommonStarsImage {
    image: ImageLayer<u16>,
    k:     usize,
}

impl CommonStarsImage {
    fn calc_hfd(&self) -> Option<f32> {
        let mut sum = 0_f64;
        let mut sum_x = 0_f64;
        let mut sum_y = 0_f64;
        for (x, y, v) in self.image.coord_iter() {
            let v = v as f64;
            sum += v;
            sum_x += x as f64 * v;
            sum_y += y as f64 * v;
        }

        if sum == 0.0 {
            return None;
        }

        let centroid_x = sum_x / sum;
        let centroid_y = sum_y / sum;

        let mut sum_d = 0_f64;
        for (x, y, v) in self.image.coord_iter() {
            let dx = centroid_x - x as f64;
            let dy = centroid_y - y as f64;
            let dist = f64::sqrt(dx * dx + dy * dy);
            sum_d += dist * v as f64;
        }

        let mut radius = sum_d / sum;
        radius /=  self.k as f64;

        Some(2.0 * radius as f32)
    }

    fn calc_fwhm(&self) -> Option<f32> {
        use std::f32::consts::PI;
        if self.image.is_empty() {
            return None;
        }
        let area = self.image
            .as_slice()
            .iter()
            .filter(|&v| *v >= u16::MAX / 2)
            .count() as f32;
        let radius = f32::sqrt(area / PI);
        Some(2.0 * radius / self.k as f32)
    }

    fn calc_ovality(&self) -> Option<f32> {
        if self.image.is_empty() {
            return None;
        }
        const ANGLE_CNT: usize = 32;
        let center_x = (self.image.width() / 2) as f64;
        let center_y = (self.image.height() / 2) as f64;
        let size = usize::max(self.image.width(), self.image.height()) as i32;
        let mut widths = Vec::new();
        for i in 0..ANGLE_CNT {
            let angle = PI * (i as f64) / (ANGLE_CNT as f64);
            let cos_angle = f64::cos(angle);
            let sin_angle = f64::sin(angle);

            let mut first_over_index = None;
            let mut last_over_index = None;

            let mut prev_over = false;
            for j in -size/2..size/2 {
                let x = j as f64 * cos_angle + center_x;
                let y = j as f64 * sin_angle + center_y;
                let mut over = false;
                if let Some(v) = self.image.get_f64_crd(x, y) {
                    over = v >= u16::MAX/2;
                }
                if first_over_index.is_none() && over {
                    first_over_index = Some(j);
                }
                if prev_over && !over {
                    last_over_index = Some(j-1);
                }
                prev_over = over;
            }

            if let (Some(from), Some(to)) = (first_over_index, last_over_index) {
                widths.push(to - from + 1);
            } else {
                widths.push(0);
            }
        }
        let max_width_pos = widths.iter().copied().position_max().unwrap_or_default();
        let min_width_pos = (max_width_pos + ANGLE_CNT/2) % ANGLE_CNT;
        let max_width = widths[max_width_pos] as f64;
        let min_width = widths[min_width_pos] as f64;
        let ovality = (max_width - min_width) / self.k as f64;

        Some(ovality as f32)
    }

    fn calc_angular_fwhm(fwhm: Option<f32>, raw_info: &Option<RawImageInfo>) -> Option<f32> {
        let fwhm = fwhm?;
        let raw_info = raw_info.as_ref()?;
        let focal_len = raw_info.focal_len?;
        let pixel_size_x = raw_info.pixel_size_x?;
        let pixel_size_y = raw_info.pixel_size_y?;

        let pixel_size = 0.5 * (pixel_size_x + pixel_size_y);
        let pixel_size_m = pixel_size / 1_000_000.0;
        let r = 0.5 * fwhm as f64 * pixel_size_m;
        let focal_len_m = focal_len / 1000.0;
        let result = 2.0 * f64::atan2(r, focal_len_m);

        Some(result as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_threshold_narrow_image() {
        // Tiny image: graceful error, not a panic
        let image = ImageLayer::new_with_size(16, 100);
        let bad_res = StarsFinder::calc_threshold_for_stars_detection(&image, StarRecognSensitivity::Normal);
        assert!(bad_res.is_err());
    }

    // Flood-fill core of a Gaussian star (optionally clipped at `clip` amplitude),
    // cut at the 1/3-of-observed-max border like the pipeline does
    fn make_core(sigma: f64, amp: f64, clip: Option<f64>, bg: u16) -> HashMap<(isize, isize), u16> {
        let observed_amp = match clip { Some(c) => f64::min(amp, c), None => amp };
        let border = bg + ((observed_amp + 1.0) / 3.0) as u16;
        let mut core = HashMap::new();
        for dy in -8_isize..=8 {
            for dx in -8_isize..=8 {
                let r2 = (dx * dx + dy * dy) as f64;
                let mut v = amp * f64::exp(-r2 / (2.0 * sigma * sigma));
                if let Some(c) = clip { v = f64::min(v, c); }
                let v = bg + v.round() as u16;
                if v >= border {
                    core.insert((dx, dy), v);
                }
            }
        }
        core
    }

    #[test]
    fn test_overexposure_clean_stars_not_flagged() {
        // FWHM 2.4 .. 9.4 px: the regime where the old row-based test flagged
        // nearly all stars regardless of brightness
        for sigma in [1.0, 1.5, 2.0, 2.5, 3.0, 4.0] {
            let core = make_core(sigma, 5000.0, None, 1000);
            assert!(
                !StarsFinder::is_core_overexposed(&core, 1000),
                "clean star sigma={} flagged", sigma
            );
            let core = make_core(sigma, 30000.0, None, 1000);
            assert!(
                !StarsFinder::is_core_overexposed(&core, 1000),
                "bright clean star sigma={} flagged", sigma
            );
        }
    }

    #[test]
    fn test_overexposure_clipped_stars_flagged() {
        // True peak >= 2x the clipping level: a substantial plateau
        for sigma in [1.5, 2.0, 2.5, 3.0, 4.0] {
            let core = make_core(sigma, 50000.0, Some(20000.0), 1000);
            assert!(
                StarsFinder::is_core_overexposed(&core, 1000),
                "clipped star sigma={} not flagged", sigma
            );
        }
    }

    #[test]
    fn test_overexposure_tiny_clipped_core_not_flagged() {
        // Known limitation: a hard-clipped star with a core of a few pixels
        // has no statistically separable plateau (needs the sensor ceiling test)
        let core = make_core(0.6, 50000.0, Some(20000.0), 1000);
        assert!(!StarsFinder::is_core_overexposed(&core, 1000));
    }

    #[test]
    fn test_star_count_cap_limits_overexposed() {
        let mut stars: Vec<_> = (0..400u32)
            .map(|i| Star { brightness: i * 2 + 1, overexposed: true, ..Default::default() })
            .chain((0..400u32).map(|i| Star { brightness: i * 2, ..Default::default() }))
            .collect();
        stars.sort_by_key(|s| -(s.brightness as i32));
        StarsFinder::cap_star_count(&mut stars);
        assert_eq!(stars.len(), MAX_STARS_CNT);
        assert_eq!(stars.iter().filter(|s| s.overexposed).count(), MAX_STARS_CNT / 2);

        // Few clean stars: the list shrinks instead of being padded with overexposed
        let mut stars: Vec<_> = (0..300)
            .map(|_| Star { overexposed: true, ..Default::default() })
            .chain((0..100).map(|_| Star::default()))
            .collect();
        StarsFinder::cap_star_count(&mut stars);
        assert_eq!(stars.len(), 100 + MAX_STARS_CNT / 2);
    }

    #[test]
    fn test_overexposure_real_frames() {
        // Smoke test on captured frames (untracked tmp_frames corpus, skipped when
        // absent). Strict false-positive/false-negative coverage is in the tests above
        // (the old row-based detector would fail those). Here: detection works on
        // real frames, saturated stars exist but do not dominate
        let frames_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp_frames");
        if !frames_dir.is_dir() {
            eprintln!("tmp_frames/ not found, skipping");
            return;
        }
        let mut paths: Vec<_> = std::fs::read_dir(&frames_dir).unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "fits"))
            .collect();
        paths.sort();
        let mut total = 0; let mut oexp = 0; let mut frames_with_sat = 0;
        for p in paths {
            let raw = crate::image::io::load_raw_image_from_fits_file(&p).unwrap();
            // Bayer frames are detected by the app only after demosaicing, skip them here
            if raw.info().cfa != crate::image::raw::CfaType::None {
                eprintln!("{}: CFA frame, skipped", p.file_name().unwrap().to_string_lossy());
                continue;
            }
            let (w, h) = (raw.info().width, raw.info().height);
            let mut image = ImageLayer::new_with_size(w, h);
            image.as_slice_mut().copy_from_slice(raw.as_slice());
            let finder = StarsFinder::new();
            let stars = finder.find_stars_and_get_info(
                &image, &None, StarRecognSensitivity::Normal, false, true).unwrap();
            let items = &stars.items;
            let n = items.len(); let k = items.iter().filter(|s| s.overexposed).count();
            if k > 0 { frames_with_sat += 1; }
            total += n; oexp += k;
            eprintln!("{}: {} stars, {} overexposed ({}%)",
                p.file_name().unwrap().to_string_lossy(), n, k,
                if n>0 {100*k/n} else {0});
        }
        assert!(total > 0, "no stars found");
        assert!(frames_with_sat >= 4, "expected saturated stars in >=4 frames, got {}", frames_with_sat);
        assert!(oexp < total / 2, "too many overexposed: {}/{}", oexp, total);
    }
}
