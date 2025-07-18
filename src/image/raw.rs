use std::{collections::HashSet, fs::File, io::*, path::Path};
use bitflags::bitflags;
use chrono::prelude::*;
use rayon::prelude::*;
use itertools::{izip, Itertools};
use serde::{Serialize, Deserialize};

use crate::utils::math::square_ls;
use super::{image::*, simple_fits::*};

#[derive(Clone)]
pub struct BadPixel {
    pub x: isize,
    pub y: isize,
}

#[derive(Default)]
pub struct BadPixels{
    pub items: Vec<BadPixel>,
}

impl BadPixels {
    pub fn save_to_file(&self, file_name: &Path) -> anyhow::Result<()> {
        let mut file = BufWriter::new(File::create(file_name)?);
        for pixel in &self.items {
            writeln!(file, "{} {}", pixel.x, pixel.y)?;
        }
        Ok(())
    }

    pub fn load_from_file(&mut self, file_name: &Path) -> anyhow::Result<()> {
        let file = BufReader::new(File::open(file_name)?);
        self.items.clear();
        for line in file.lines().map_while(Result::ok) {
            let mut splitted = line.splitn(2, " ");
            let (Some(x_str), Some(y_str)) = (splitted.next(), splitted.next()) else { continue; };
            let (Ok(x), Ok(y)) = (x_str.parse(), y_str.parse()) else { continue; };
            self.items.push(BadPixel{x, y});
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfaType { None, BGGR, RGBG, GRBG, RGGB }

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CfaColor { None, R, G, B }

type CfaArray = [&'static [CfaColor; 2]];

impl CfaType {
    pub fn get_array(&self) -> &'static CfaArray {
        use CfaColor::*;
        match self {
            CfaType::BGGR => &[&[B, G], &[G, R]],
            CfaType::RGBG => &[&[R, G], &[G, B]],
            CfaType::GRBG => &[&[G, R], &[B, G]],
            CfaType::RGGB => &[&[R, G], &[G, B]],
            CfaType::None => &[&[None, None]],
        }
    }

    pub fn from_str(cfa_str: &str) -> Self {
        match cfa_str {
            "BGGR" => CfaType::BGGR,
            "RGBG" => CfaType::RGBG,
            "GRBG" => CfaType::GRBG,
            "RGGB" => CfaType::RGGB,
            _      => CfaType::None,
        }
    }

    fn to_str(&self) -> Option<&'static str> {
        match self {
            CfaType::None => None,
            CfaType::BGGR => Some("BGGR"),
            CfaType::RGBG => Some("RGBG"),
            CfaType::GRBG => Some("GRBG"),
            CfaType::RGGB => Some("RGGB"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Copy, Default)]
pub enum FrameType {
    #[default]
    Undef,
    Lights,
    Flats,
    Darks,
    Biases,
}

impl FrameType {
    pub fn from_str(text: &str, def: FrameType) -> Self {
        match text {
            "Light" => FrameType::Lights,
            "Flat"  => FrameType::Flats,
            "Dark"  => FrameType::Darks,
            "Bias"  => FrameType::Biases,
            _       => def,
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            FrameType::Undef  => "Undefined",
            FrameType::Lights => "Light",
            FrameType::Flats  => "Flat",
            FrameType::Darks  => "Dark",
            FrameType::Biases => "Bias",
        }
    }

    pub fn to_readable_str(&self) -> &'static str {
        match self {
            FrameType::Lights => "Saving LIGHT frames",
            FrameType::Flats  => "Saving FLAT frames",
            FrameType::Darks  => "Saving DARK frames",
            FrameType::Biases => "Saving BIAS frames",
            FrameType::Undef  => "Unknows save frames state :("
        }
    }
}

bitflags! {
    #[derive(Serialize, Deserialize, Clone, Copy)]
    pub struct CalibrMethods: u32 {
        const BY_DARK           = 1;
        const BY_BIAS           = 2;
        const BY_FLAT           = 4;
        const DEFECTIVE_PIXELS  = 8;
        const HOT_PIXELS_SEARCH = 16;
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RawImageInfo {
    pub time:           Option<DateTime<Utc>>,
    pub width:          usize,
    pub height:         usize,
    pub gain:           i32,
    pub offset:         i32,
    pub max_value:      u16,
    pub cfa:            CfaType,
    pub bin:            u8,
    pub frame_type:     FrameType,
    pub exposure:       f64,
    pub integr_time:    Option<f64>, // for master files
    pub camera:         String,
    pub ccd_temp:       Option<f64>,
    pub focal_len:      Option<f64>,
    pub pixel_size_x:   Option<f64>, // um
    pub pixel_size_y:   Option<f64>, // um
    pub calibr_methods: CalibrMethods,
    pub dec:            Option<f64>, // deg
    pub ra:             Option<f64>, // deg
}

impl RawImageInfo {
    pub fn new_from_fits_header(image_hdu: &Header) -> Self {
        let bitdepth     = image_hdu.get_i64("BITDEPTH").unwrap_or(image_hdu.bitpix() as i64) as i32;
        let width        = image_hdu.dims()[0];
        let height       = image_hdu.dims()[1];
        let exposure     = image_hdu.get_f64("EXPTIME").unwrap_or_default();
        let integr_time  = image_hdu.get_f64("TOTALEXP");
        let bayer        = image_hdu.get_str("BAYERPAT").unwrap_or_default();
        let bin          = image_hdu.get_f64("XBINNING").unwrap_or(1.0) as u8;
        let gain         = image_hdu.get_f64("GAIN").unwrap_or(0.0) as i32;
        let offset       = image_hdu.get_f64("OFFSET").unwrap_or(0.0) as i32;
        let frame_str    = image_hdu.get_str("FRAME");
        let time_str     = image_hdu.get_str("DATE-OBS").unwrap_or_default();
        let camera       = image_hdu.get_str("INSTRUME").unwrap_or_default().to_string();
        let ccd_temp     = image_hdu.get_f64("CCD-TEMP");
        let focal_len    = image_hdu.get_f64("FOCALLEN");
        let pixel_size_x = image_hdu.get_f64("PIXSIZE1");
        let pixel_size_y = image_hdu.get_f64("PIXSIZE2");
        let dec          = image_hdu.get_f64("DEC");
        let ra           = image_hdu.get_f64("RA");

        let max_value = if bitdepth > 0 {
            ((1 << bitdepth) - 1) as u16
        } else {
            u16::MAX
        };
        let cfa = CfaType::from_str(bayer);
        let frame_type = FrameType::from_str(
            frame_str.unwrap_or_default(),
            FrameType::Lights
        );

        let time =
            NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%S%.3f")
                .map(|dt| Utc.from_utc_datetime(&dt))
                .ok();

        Self {
            time, width, height, gain, offset, cfa, bin,
            max_value, frame_type, exposure, integr_time,
            camera, ccd_temp, focal_len,
            pixel_size_x, pixel_size_y,
            calibr_methods: CalibrMethods::empty(),
            dec, ra
        }
    }

    pub fn seve_to_fits_header(&self, hdu : &mut Header) {
        hdu.set_f64("EXPTIME",  self.exposure);
        if let Some(integr_exp) = self.integr_time {
            hdu.set_f64("TOTALEXP", integr_exp);
        }
        hdu.set_str("ROWORDER", "TOP-DOWN");
        hdu.set_str("FRAME",    self.frame_type.to_str());
        hdu.set_i64("XBINNING", self.bin as i64);
        hdu.set_i64("YBINNING", self.bin as i64);
        hdu.set_i64("GAIN",     self.gain as i64);
        hdu.set_i64("OFFSET",   self.offset as i64);
        hdu.set_str("INSTRUME", &self.camera);
        if let Some(bayer) = self.cfa.to_str() {
            hdu.set_str("BAYERPAT", bayer);
        }
        if let Some(ccd_temp) = self.ccd_temp {
            hdu.set_f64("CCD-TEMP", ccd_temp);
        }
    }
}

pub struct RawImage {
    info:    RawImageInfo,
    data:    Vec<u16>,
    cfa_arr: &'static CfaArray,
}

impl Clone for RawImage {
    fn clone(&self) -> Self {
        Self {
            info: self.info.clone(),
            data: self.data.clone(),
            cfa_arr: self.cfa_arr
        }
    }
}

impl RawImage {
    pub fn new(
        info:    RawImageInfo,
        data:    Vec<u16>,
        cfa_arr: &'static CfaArray) -> Self {
        Self { info, data, cfa_arr }
    }

    pub fn save_to_fits_file(&self, file_name: &Path) -> anyhow::Result<()> {
        let mut file = File::create(file_name)?;
        let writer = FitsWriter::new();
        let mut hdu = Header::new_2d(self.info.width, self.info.height);
        self.info.seve_to_fits_header(&mut hdu);
        writer.write_header_and_data_u16(&mut file, &hdu, &self.data)?;
        Ok(())
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.data
    }

    pub fn info(&self) -> &RawImageInfo {
        &self.info
    }

    pub fn set_offset(&mut self, offset: i32) {
        self.info.offset = offset;
    }

    pub fn row(&self, y: usize) -> &[u16] {
        let pos = y * self.info.width;
        &self.data[pos..pos+self.info.width]
    }

    fn row_mut(&mut self, y: usize) -> &mut [u16] {
        let pos = y * self.info.width;
        &mut self.data[pos..pos+self.info.width]
    }

    #[inline(always)]
    fn get(&self, x: isize, y: isize) -> Option<u16> {
        if x < 0
        || y < 0
        || x >= self.info.width as isize
        || y >= self.info.height as isize {
            return None;
        }
        Some(unsafe {
            *self.data.get_unchecked(y as usize * self.info.width + x as usize)
        })
    }

    #[inline(always)]
    fn set(&mut self, x: isize, y: isize, value: u16) {
        if x < 0
        || y < 0
        || x >= self.info.width as isize
        || y >= self.info.height as isize {
            panic!(
                "Wrong coords =({}, {}), image width = {}, image height = {}",
                x, y, self.info.width, self.info.height,
            );
        }
        self.data[y as usize * self.info.width + x as usize] = value;
    }

    pub fn cfa_row(&self, y: usize) -> &'static [CfaColor; 2] {
        self.cfa_arr[y % self.cfa_arr.len()]
    }

    fn cfa_get(&self, x: isize, y: isize) -> Option<CfaColor> {
        if x < 0
        || y < 0
        || x >= self.info.width as isize
        || y >= self.info.height as isize {
            None
        } else {
            let row = self.cfa_arr[y as usize % self.cfa_arr.len()];
            Some(row[x as usize % row.len()])
        }
    }

    fn rect_iter(&self, mut x1: isize, mut y1: isize, mut x2: isize, mut y2: isize) -> RawRectIterator {
        if x1 < 0 { x1 = 0; }
        if y1 < 0 { y1 = 0; }
        if x2 >= self.info.width as isize { x2 = self.info.width as isize - 1; }
        if y2 >= self.info.height as isize { y2 = self.info.height as isize - 1; }
        RawRectIterator {
            raw: self,
            iter: RawRectIterator::init_iter(self, x1 as usize, x2 as usize, y1 as usize),
            cfa_iter: RawRectIterator::init_cfa_iter(self, x1 as usize, y1 as usize),
            x1: x1 as usize,
            x2: x2 as usize,
            y2: y2 as usize,
            x: x1 as usize,
            y: y1 as usize,
        }
    }

    pub fn find_hot_pixels_in_master_dark(&self) -> BadPixels {
        log::debug!("Calculating hot pixels border:");

        fn calc_border(diffs: &mut [i32]) -> i32 {
            if diffs.len() < 100 {
                return i32::MAX;
            }

            let p60_pos = 60 * diffs.len() / 100;
            let p60_value = *diffs.select_nth_unstable(p60_pos).1;

            let p70_pos = 70 * diffs.len() / 100;
            let p70_value = *diffs.select_nth_unstable(p70_pos).1;

            let p80_pos = 80 * diffs.len() / 100;
            let p80_value = *diffs.select_nth_unstable(p80_pos).1;

            let p90_pos = 90 * diffs.len() / 100;
            let p90_value = *diffs.select_nth_unstable(p90_pos).1;

            let x_values = [60.0, 70.0, 80.0, 90.0];
            let y_values = [p60_value as f64, p70_value as f64, p80_value as f64, p90_value as f64];

            let max_pos = diffs.len() - 10;
            let max_slice = diffs.select_nth_unstable(max_pos).2;
            let max = max_slice.iter().sum::<i32>() / max_slice.len() as i32;

            let result = if let Some(coeffs) = square_ls(&x_values, &y_values) {
                let p100_value = coeffs.calc(100.0) as i32;
                log::debug!("p100_value={}", p100_value);

                if 3 * p100_value >= max {
                    i32::MAX
                } else {
                    (3 * p100_value + max) / 4
                }
            } else  {
                i32::MAX
            };

            log::debug!(
                "diffs.len={}, p60_value={}, p70_value={}, p80_value={}, p90_value={}, max={}, result={}",
                diffs.len(), p60_value, p70_value, p80_value, p90_value, max, result
            );

            result
        }

        #[inline(always)]
        fn process_col_or_row(
            data: &[u16],
            mut fun: impl FnMut(u16, usize/*index*/, u16, u16, u16, u16, u16, u16)
        ) {
            fun(data[0], 0, data[1], data[2], data[3], data[4], data[5], data[6]);
            fun(data[1], 1, data[0], data[2], data[3], data[4], data[5], data[6]);
            fun(data[2], 2, data[0], data[1], data[3], data[4], data[5], data[6]);
            for (i, (v1, v2, v3, v4, v5, v6, v7))
            in data.iter().tuple_windows().enumerate() {
                fun(*v4, i + 3, *v1, *v2, *v3, *v5, *v6, *v7);
            }
            let width = data.len();
            let row_end = &data[width-7..];
            fun(row_end[4], width-3, row_end[0], row_end[1], row_end[2], row_end[3], row_end[5], row_end[6]);
            fun(row_end[5], width-2, row_end[0], row_end[1], row_end[2], row_end[3], row_end[4], row_end[6]);
            fun(row_end[6], width-1, row_end[0], row_end[1], row_end[2], row_end[3], row_end[4], row_end[5]);
        }

        #[inline(always)]
        fn process_rows(
            raw:     &RawImage,
            step:    usize,
            mut fun: impl FnMut(u16, usize/*x*/, usize/*y*/, u16, u16, u16, u16, u16, u16)
        ) {
            for y in (0..raw.info.height).step_by(step) {
                let row = raw.row(y);
                process_col_or_row(row, |v, index, v1, v2, v3, v4, v5, v6| {
                    fun(v, index, y, v1, v2, v3, v4, v5, v6);
                });
            }
        }

        let mut tmp_result = HashSet::new();
        let mut diffs = Vec::with_capacity(self.data.len() / 3);

        process_rows(
            self,
            3,
            |v, _x, _y, v1, v2, v3, v4, v5, v6| {
                let aver = v1 as i32 + v2 as i32 + v3 as i32 + v4 as i32 + v5 as i32 + v6 as i32;
                let diff = (v as i32) * 6 - aver;
                if diff > 0 {
                    diffs.push(diff)
                }
            }
        );

        let border = calc_border(&mut diffs);

        let mut hits = 0;
        process_rows(
            self,
            1,
            |v, x, y, v1, v2, v3, v4, v5, v6| {
                let aver = v1 as i32 + v2 as i32 + v3 as i32 + v4 as i32 + v5 as i32 + v6 as i32;
                let diff = (v as i32) * 6 - aver;
                if diff > border {
                    tmp_result.insert((x, y));
                    hits += 1;
                }
            }
        );

        #[inline(always)]
        fn process_cols(
            raw:     &RawImage,
            step:    usize,
            mut fun: impl FnMut(u16, usize/*x*/, usize/*y*/, u16, u16, u16, u16, u16, u16)
        ) {
            let mut col = Vec::new();
            for x in (0..raw.info.width).step_by(step) {
                col.clear();
                let col_data = &raw.data[x..];
                for v in col_data.iter().step_by(raw.info.width) {
                    col.push(*v);
                }
                process_col_or_row(&col, |v, index, v1, v2, v3, v4, v5, v6| {
                    fun(v, x, index, v1, v2, v3, v4, v5, v6);
                });
            }
        }

        diffs.clear();

        process_cols(
            self,
            3,
            |v, _x, _y, v1, v2, v3, v4, v5, v6| {
                let aver = v1 as i32 + v2 as i32 + v3 as i32 + v4 as i32 + v5 as i32 + v6 as i32;
                let diff = (v as i32) * 6 - aver;
                if diff > 0 {
                    diffs.push(diff)
                }
            }
        );

        let border = calc_border(&mut diffs);

        let mut hits = 0;
        process_cols(
            self,
            1,
            |v, x, y, v1, v2, v3, v4, v5, v6| {
                let aver = v1 as i32 + v2 as i32 + v3 as i32 + v4 as i32 + v5 as i32 + v6 as i32;
                let diff = (v as i32) * 6 - aver;
                if diff > border {
                    tmp_result.insert((x, y));
                    hits += 1;
                }
            }
        );

        let pixels: Vec<_> = tmp_result
            .iter()
            .map(|(x, y)| BadPixel{ x: *x as isize, y: *y as isize })
            .collect();

        log::debug!("Hot pixels count={}", pixels.len());

        BadPixels{ items: pixels }
    }

    pub fn find_hot_pixels_in_light(&self) -> Vec<BadPixel> {
        let process_color = |color: CfaColor, x_step: usize, y_step: usize, result: &mut Vec<BadPixel>| {
            let cfa_arr = self.info.cfa.get_array();
            let y_start =
                if color == CfaColor::None { 0 }
                else if cfa_arr[0][0] == color || cfa_arr[0][1] == color { 0 }
                else if cfa_arr[1][0] == color || cfa_arr[1][1] == color { 1 }
                else { panic!("Internal error"); };
            let mut diffs: Vec<_> = self.data
                .par_chunks_exact(self.info.width)
                .enumerate()
                .skip(y_start)
                .step_by(4*y_step) // skip each 4 row for faster statistics
                .flat_map_iter(|(y, row)| {
                    let cfa_row = self.cfa_row(y);
                    let x_start = if cfa_row[0] == color { 0 } else { 1 };
                    row.iter()
                        .skip(x_start)
                        .step_by(x_step)
                        .tuple_windows()
                        .filter_map(move |(v1, v2, v3)| {
                            if v2 > v1 && v2 > v3 {
                                let aver = ((*v1 as u32 + *v3 as u32) / 2) as u16;
                                Some(*v2 - aver)
                            } else {
                                None
                            }
                        })
                })
                .collect();
            if diffs.is_empty() { return; }
            let pos = 99 * diffs.len() / 100;
            let percentile_val = *diffs.select_nth_unstable(pos).1;
            let border = 150 * percentile_val as u32 / 100;
            let border = border
                .max(self.info.max_value as u32 / 1000)
                .min(u16::MAX as u32 / 2) as u16;
            let tmp_result: Vec<_> = self.data
                .par_chunks_exact(self.info.width)
                .enumerate()
                .skip(y_start)
                .step_by(y_step)
                .flat_map_iter(|(y, row)| {
                    let cfa_row = self.cfa_row(y);
                    let x_start = if cfa_row[0] == color { 0 } else { 1 };
                    row.iter()
                        .enumerate()
                        .skip(x_start)
                        .step_by(x_step)
                        .tuple_windows()
                        .filter_map(move |((_, v1), (x, p), (_, v2))| {
                            if p > v1 && p > v2 && p-v1 > border && p-v2 > border {
                                let mut min = u16::MAX;
                                for offset in 2 ..= 3 {
                                    let offset = offset as isize * 2;
                                    let mut sum = 0_u32;
                                    let mut cnt = 0_u32;
                                    for my in -1 ..= 1 { for mx in -1 ..= 1 {
                                        if my == 0 && mx == 0 { continue; }
                                        let test_x = x as isize + mx * offset;
                                        let test_y = x as isize + my * offset;
                                        if let Some(v) = self.get(test_x, test_y) {
                                            sum += v as u32;
                                            cnt += 1;
                                        }
                                    }}
                                    if cnt != 0 {
                                        let aver = sum / cnt;
                                        min = min.min(aver as u16);
                                    }
                                }
                                let diff1 = *p as i32 - *v1 as i32;
                                let diff2 = *v1 as i32 - min as i32;
                                let diff3 = *p as i32 - *v2 as i32;
                                let diff4 = *v2 as i32 - min as i32;
                                if diff1 > 2*diff2 && diff3 > 2*diff4 {
                                    Some(BadPixel {
                                        x: x as isize,
                                        y: y as isize,
                                    })
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                })
                .collect();
            result.extend_from_slice(&tmp_result);
        };
        let mut result = Vec::new();
        if self.info.cfa != CfaType::None {
            process_color(CfaColor::R, 2, 2, &mut result);
            process_color(CfaColor::G, 2, 1, &mut result);
            process_color(CfaColor::B, 2, 2, &mut result);
        } else {
            process_color(CfaColor::None, 1, 1, &mut result);
        }

        for px in &result {
            debug_assert!(px.y < self.info.height as _);
            debug_assert!(px.x < self.info.width as _);
        }

        result
    }

    pub fn remove_bad_pixels(&mut self, bad_pixels: &[BadPixel]) {
        let mut pixels_to_fix = HashSet::new();
        for pixel in bad_pixels {
            pixels_to_fix.insert((pixel.x, pixel.y));
        }

        let mut fixed_pixels = Vec::new();
        for _step in 0..3 {
            fixed_pixels.clear();
            for (px, py) in &pixels_to_fix {
                let bad_pixel_color = self.cfa_get(*px, *py);
                let mut cnt = 0_u32;
                let mut sum = 0_u32;
                let range = match bad_pixel_color {
                    Some(CfaColor::G)|
                    Some(CfaColor::None) => 1,
                    _                    => 2
                };
                for dy in -range..=range {
                    let y = *py + dy;
                    for dx in -range..=range {
                        let x = *px + dx;
                        if self.cfa_get(x, y) == bad_pixel_color
                        && !pixels_to_fix.contains(&(x, y)) {
                            if let Some(v) = self.get(x, y) {
                                sum += v as u32;
                                cnt += 1;
                            }
                        }
                    }
                }
                if cnt != 0 {
                    self.set(*px, *py, (sum / cnt) as u16);
                    fixed_pixels.push((*px, *py));
                }
            }
            for fixed in &fixed_pixels {
                pixels_to_fix.remove(fixed);
            }
            if pixels_to_fix.is_empty() {
                break;
            }
        }
    }

    fn check_master_frame_is_compatible(
        &self,
        master_frame: &RawImage,
        frame_types:  &[FrameType],
    ) -> anyhow::Result<()> {
        if !frame_types.contains(&master_frame.info.frame_type) {
            anyhow::bail!(
                "Wrong frame type. Expected {:?}, found {:?}",
                frame_types,
                master_frame.info.frame_type,
            );
        }

        if self.info.width != master_frame.info.width
        || self.info.height != master_frame.info.height {
            anyhow::bail!(
                "Different sizes (light frame: {}x{}, calibration frame: {}x{})",
                self.info.width, self.info.height,
                master_frame.info.width, master_frame.info.height,
            );
        }

        if self.info.cfa != master_frame.info.cfa {
            anyhow::bail!(
                "Different CFA (light frame: {:?}, calibration frame: {:?})",
                self.info.cfa,
                master_frame.info.cfa,
            )
        }

        Ok(())
    }

    pub fn subtract_dark_or_bias(&mut self, dark: &RawImage) -> anyhow::Result<()> {
        self.check_master_frame_is_compatible(dark, &[FrameType::Darks, FrameType::Biases])?;
        debug_assert!(self.data.len() == dark.data.len());
        let dark_sum: i64 = dark.as_slice().iter().map(|v| *v as i64).sum();
        let dark_aver = (dark_sum / dark.data.len() as i64) as i32;

        let raw_sum: i64 = self.as_slice().iter().map(|v| *v as i64).sum();
        let raw_aver = (raw_sum / self.data.len() as i64) as i32;

        let diff = raw_aver - dark_aver;
        for (s, d) in self.data.iter_mut().zip(&dark.data) {
            let mut value = *s as i32;
            let dark_value = *d as i32;
            value -= dark_value;
            value += diff;
            if value < 0 { value = 0; }
            if value > u16::MAX as i32 { value = u16::MAX as i32; }
            *s = value as u16;
        }
        self.info.offset += diff;
        Ok(())
    }

    pub fn apply_flat(&mut self, flat: &RawImage) -> anyhow::Result<()> {
        self.check_master_frame_is_compatible(flat, &[FrameType::Flats])?;
        debug_assert!(self.data.len() == flat.data.len());
        let zero = self.info.offset as i64;
        let flat_zero = flat.info.offset as i64;
        self.data.par_iter_mut().zip(flat.data.par_iter()).for_each(|(s, f)| {
            let flat_value = *f as i64 - flat_zero;
            let mut value = *s as i64;
            value -= zero;
            value = value * u16::MAX as i64 / flat_value;
            value += zero;
            if value < 0 { value = 0; }
            if value > u16::MAX as i64 { value = u16::MAX as i64; }
            *s = value as u16;
        });
        Ok(())
    }

    pub fn normalize_flat(&mut self) {
        let mut l_values = Vec::new();
        let mut r_values = Vec::new();
        let mut g_values = Vec::new();
        let mut b_values = Vec::new();
        for y in 0..self.info.height {
            let cfa_arr = self.cfa_row(y);
            let row = self.row(y);
            for (v, c) in row.iter().zip(cfa_arr.iter().cycle()) {
                match *c {
                    CfaColor::None => l_values.push(*v),
                    CfaColor::R => r_values.push(*v),
                    CfaColor::G => g_values.push(*v),
                    CfaColor::B => b_values.push(*v),
                }
            }
        }
        let get_99 = |values: &mut Vec<u16>| -> i64 {
            if values.is_empty() {
                0
            } else {
                let pos = 99 * values.len() / 100;
                values.select_nth_unstable(pos);
                let result = values[pos];
                values.clear();
                values.shrink_to_fit();
                result as i64
            }
        };
        let zero = self.info.offset as i64;
        let l_max = get_99(&mut l_values) - zero;
        let r_max = get_99(&mut r_values) - zero;
        let g_max = get_99(&mut g_values) - zero;
        let b_max = get_99(&mut b_values) - zero;
        for y in 0..self.info.height {
            let cfa_arr = self.cfa_row(y);
            let row = self.row_mut(y);
            for (v, c) in row.iter_mut().zip(cfa_arr.iter().cycle()) {
                let max = match *c {
                    CfaColor::None => l_max,
                    CfaColor::R => r_max,
                    CfaColor::G => g_max,
                    CfaColor::B => b_max,
                };
                let val = *v as i64 - zero;
                let normalized: i64 = (u16::MAX as i64 * val) / max;
                let normalized = normalized.max(0).min(u16::MAX as i64);
                *v = normalized as u16;
            }
        }
        self.info.offset = 0;
    }

    pub fn filter_flat(&mut self) {
        let mut new_data = vec![0; self.data.len()];
        new_data
            .par_chunks_exact_mut(self.info.width)
            .enumerate()
            .for_each(|(y, dst_row)| {
                let cfa_row = self.cfa_row(y);
                for (x, (dst, c))
                in dst_row.iter_mut().zip(cfa_row.iter().cycle()).enumerate() {
                    let mut cnt = 0_u32;
                    let mut sum = 0_u32;
                    let x = x as isize;
                    let y = y as isize;
                    for (_x, _y, v, cfa_c) in self.rect_iter(x-2, y-2, x+2, y+2) {
                        if *c != cfa_c { continue; }
                        sum += v as u32;
                        cnt += 1;
                    }
                    if cnt != 0 {
                        *dst = ((sum + cnt/2) / cnt) as u16;
                    } else {
                        *dst = self.get(x, y).unwrap_or_default();
                    }
                }
            });
        self.data = new_data;
    }

    pub fn calc_noise(&self) -> Option<f32> {
        let rect_size = (self.info.width / 200).clamp(16, 42);
        let step = 7;
        let rows = self.info.height / rect_size;
        let cols = self.info.width / rect_size;
        let mut values = Vec::new();
        let mut diffs = Vec::new();
        let cfa_color = if self.info.cfa == CfaType::None {
            CfaColor::None
        } else {
            CfaColor::G
        };
        for row in (0..rows).step_by(step) {
            let y1 = rect_size * row;
            let y2 = y1 + rect_size - 1;
            for col in (0..cols).step_by(step) {
                let x1 = rect_size * col;
                let x2 = x1 + rect_size - 1;
                values.clear();
                for (_, _, v, color)
                in self.rect_iter(x1 as isize, y1 as isize, x2 as isize, y2 as isize) {
                    if color != cfa_color { continue; }
                    values.push(v);
                }
                if values.is_empty() { continue; }
                for _ in 0..5 {
                    let median_pos = values.len() / 2;
                    let median = *values.select_nth_unstable(median_pos).1 as f64;
                    let sum: f64 = values
                        .iter()
                        .map(|v| {
                            let diff = *v as f64 - median;
                            diff * diff
                        })
                        .sum();
                    let std_dev = f64::sqrt(sum / values.len() as f64);
                    let max = (median + 3.0 * std_dev) as i32;
                    let len_before = values.len();
                    values.retain(|v| (*v as i32) <= max);
                    if values.is_empty() || len_before == values.len() {
                        break;
                    }
                }
                if !values.is_empty() {
                    let sum: u64 = values.iter().map(|v| *v as u64).sum();
                    let aver = sum as f64 / values.len() as f64;
                    for v in &values {
                        let diff = *v as f64 - aver;
                        diffs.push(diff * diff);
                    }
                }
            }
        }
        if !diffs.is_empty() {
            let sum: f64 = diffs.iter().sum();
            Some(f64::sqrt(sum / diffs.len() as f64) as f32)
        } else {
            None
        }
    }

    pub fn demosaic_into(&self, dst_img: &mut Image, mt: bool) {
        match self.info.cfa {
            CfaType::None =>
                self.copy_into_monochrome(dst_img),
            _ =>
                self.demosaic_linear(mt, dst_img),
        }
        dst_img.raw_info = Some(self.info.clone());
    }

    pub fn copy_into_monochrome(&self, dst_img: &mut Image) {
        dst_img.make_monochrome(
            self.info.width,
            self.info.height,
            self.info.offset,
            self.info.max_value
        );
        dst_img.l
            .as_slice_mut()
            .copy_from_slice(&self.data);
    }

    fn demosaic_linear(&self, mt: bool, result: &mut Image) {
        result.make_color(
            self.info.width,
            self.info.height,
            self.info.offset,
            self.info.max_value
        );

        fn demosaic_row(
            r_row: &mut [u16],
            g_row: &mut [u16],
            b_row: &mut [u16],
            img:   &RawImage,
            y:     usize
        ) {
            let mut row1 = img.row(y-1).as_ptr();
            let mut row2 = img.row(y).as_ptr();
            let mut row3 = img.row(y+1).as_ptr();
            let row_cfa = img.cfa_row(y);

            let mut r = r_row[1..].as_mut_ptr();
            let mut g = g_row[1..].as_mut_ptr();
            let mut b = b_row[1..].as_mut_ptr();

            for (_, (c21, c22)) in izip!(
                0..img.info.width-2,
                row_cfa.iter().cycle().tuple_windows()
            ) { unsafe {
                match *c22 {
                    CfaColor::R => {
                        let v11 = row1;
                        let v12 = row1.offset(1);
                        let v13 = row1.offset(2);
                        let v21 = row2;
                        let v22 = row2.offset(1);
                        let v23 = row2.offset(2);
                        let v31 = row3;
                        let v32 = row3.offset(1);
                        let v33 = row3.offset(2);
                        *r = *v22;
                        *g = ((*v12 as usize + *v21 as usize + *v23 as usize + *v32 as usize + 2) / 4) as u16;
                        *b = ((*v11 as usize + *v13 as usize + *v31 as usize + *v33 as usize + 2) / 4) as u16;
                    },
                    CfaColor::G => {
                        let v12 = row1.offset(1);
                        let v21 = row2;
                        let v22 = row2.offset(1);
                        let v23 = row2.offset(2);
                        let v32 = row3.offset(1);
                        *g = *v22;
                        if *c21 == CfaColor::R {
                            *r = ((*v21 as usize + *v23 as usize + 1) / 2) as u16;
                            *b = ((*v12 as usize + *v32 as usize + 1) / 2) as u16;
                        } else {
                            *b = ((*v21 as usize + *v23 as usize + 1) / 2) as u16;
                            *r = ((*v12 as usize + *v32 as usize + 1) / 2) as u16;
                        }
                    },
                    CfaColor::B => {
                        let v11 = row1;
                        let v12 = row1.offset(1);
                        let v13 = row1.offset(2);
                        let v21 = row2;
                        let v22 = row2.offset(1);
                        let v23 = row2.offset(2);
                        let v31 = row3;
                        let v32 = row3.offset(1);
                        let v33 = row3.offset(2);
                        *b = *v22;
                        *g = ((*v12 as usize + *v21 as usize + *v23 as usize + *v32 as usize + 2) / 4) as u16;
                        *r = ((*v11 as usize + *v13 as usize + *v31 as usize + *v33 as usize + 2) / 4) as u16;
                    },
                    _ => {},
                }
                row1 = row1.offset(1);
                row2 = row2.offset(1);
                row3 = row3.offset(1);
                r = r.offset(1);
                g = g.offset(1);
                b = b.offset(1);
            }}
        }

        if !mt {
            for (y, (r_row, g_row, b_row)) in izip!(
                result.r.as_slice_mut().chunks_exact_mut(self.info.width),
                result.g.as_slice_mut().chunks_exact_mut(self.info.width),
                result.b.as_slice_mut().chunks_exact_mut(self.info.width),
            ).enumerate() {
                if y == 0 || y == self.info.height-1 { continue; }
                demosaic_row(r_row, g_row, b_row, self, y);
            }
        } else {
            result.r.as_slice_mut().par_chunks_exact_mut(self.info.width)
                .zip(result.g.as_slice_mut().par_chunks_exact_mut(self.info.width))
                .zip(result.b.as_slice_mut().par_chunks_exact_mut(self.info.width))
                .enumerate()
                .for_each(|(y, ((r_row, g_row), b_row))| {
                    if y == 0 || y == self.info.height-1 { return; }
                    demosaic_row(r_row, g_row, b_row, self, y);
                });
        }

        let mut demosaic_pixel_at_border = |x, y, color: CfaColor| {
            let layer = match color {
                CfaColor::R => &mut result.r,
                CfaColor::G => &mut result.g,
                CfaColor::B => &mut result.b,
                _ => unreachable!(),
            };
            let mut sum = 0_usize;
            let mut cnt = 0_usize;
            for dy in -1..=1 {
                let sy = y + dy;
                for dx in -1..=1 {
                    let sx = x + dx;
                    if self.cfa_get(sx, sy) == Some(color) {
                        if let Some(v) = self.get(sx, sy) {
                            sum += v as usize;
                            cnt += 1;
                        }
                    }
                }
            }
            let v = (sum + cnt/2) / cnt;
            layer.set(x, y, v as u16);
        };
        for x in 0..self.info.width {
            demosaic_pixel_at_border(x as isize, 0, CfaColor::R);
            demosaic_pixel_at_border(x as isize, 0, CfaColor::G);
            demosaic_pixel_at_border(x as isize, 0, CfaColor::B);
            demosaic_pixel_at_border(x as isize, self.info.height as isize - 1, CfaColor::R);
            demosaic_pixel_at_border(x as isize, self.info.height as isize - 1, CfaColor::G);
            demosaic_pixel_at_border(x as isize, self.info.height as isize - 1, CfaColor::B);
        }
        for y in 1..self.info.height-1 {
            demosaic_pixel_at_border(0, y as isize, CfaColor::R);
            demosaic_pixel_at_border(0, y as isize, CfaColor::G);
            demosaic_pixel_at_border(0, y as isize, CfaColor::B);
            demosaic_pixel_at_border(self.info.width as isize - 1, y as isize, CfaColor::R);
            demosaic_pixel_at_border(self.info.width as isize - 1, y as isize, CfaColor::G);
            demosaic_pixel_at_border(self.info.width as isize - 1, y as isize, CfaColor::B);
        }
    }

    pub fn set_calibr_methods(&mut self, calibr_methods: CalibrMethods) {
        self.info.calibr_methods = calibr_methods;
    }
}


type RawRectIteratorCfaIter<'a> = std::iter::Skip<std::iter::Cycle<std::slice::Iter<'a, CfaColor>>>;

pub struct RawRectIterator<'a> {
    raw: &'a RawImage,
    iter: std::slice::Iter<'a, u16>,
    cfa_iter: RawRectIteratorCfaIter<'a>,
    x1: usize,
    x2: usize,
    y2: usize,
    x: usize,
    y: usize,
}

impl<'a> RawRectIterator<'a> {
    fn init_iter(raw: &'a RawImage, x1: usize, x2: usize, y: usize) -> std::slice::Iter<'a, u16> {
        let row = raw.row(y);
        row[x1 ..= x2].iter()
    }

    fn init_cfa_iter(raw: &'a RawImage, x: usize, y: usize) -> RawRectIteratorCfaIter<'a> {
        let row = raw.cfa_row(y);
        row.iter().cycle().skip(x % row.len())
    }
}

impl Iterator for RawRectIterator<'_> {
    type Item = (usize, usize, u16, CfaColor);

    fn next(&mut self) -> Option<Self::Item> {
        match (self.iter.next(), self.cfa_iter.next()) {
            (Some(v), Some(c)) => {
                let x = self.x;
                self.x += 1;
                Some((x, self.y, *v, *c))
            },
            _ => {
                self.y += 1;
                if self.y > self.y2 {
                    return None;
                }

                self.x = self.x1;
                self.iter = Self::init_iter(self.raw, self.x1, self.x2, self.y);
                self.cfa_iter = Self::init_cfa_iter(self.raw, self.x1, self.y);

                self.next()
            },
        }
    }
}
