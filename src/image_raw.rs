use std::{path::Path, collections::HashSet, io::*, fs::File};
use fitsio::{images::*, FitsFile};
use rayon::prelude::*;
use itertools::{izip, Itertools};
use serde::{Serialize, Deserialize};
use bitstream_io::*;
use crate::{image::*, fits_reader::*};

const RAW_IMAGE_FILE_VERSION: u8 = 1;

#[derive(Clone)]
pub struct BadPixel {
    pub x: isize,
    pub y: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfaType { None, BGGR, RGBG, GRBG, RGGB }

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum CfaColor { None, R, G, B }

type CfaArray = [&'static [CfaColor; 2]];

impl CfaType {
    fn get_array(&self) -> &'static CfaArray {
        use CfaColor::*;
        match self {
            CfaType::BGGR => &[&[B, G], &[G, R]],
            CfaType::RGBG => &[&[R, G], &[G, B]],
            CfaType::GRBG => &[&[G, R], &[B, G]],
            CfaType::RGGB => &[&[R, G], &[G, B]],
            CfaType::None => &[&[None, None]],
        }
    }

    fn from_str(cfa_str: &str) -> Self {
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
    fn from_str(text: &str, def: FrameType) -> Self {
        match text {
            "Light" => FrameType::Lights,
            "Flat"  => FrameType::Flats,
            "Dark"  => FrameType::Darks,
            "Bias"  => FrameType::Biases,
            _       => def,

        }
    }

    fn to_str(&self) -> &'static str {
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

#[derive(Serialize, Deserialize, Clone)]
pub struct RawImageInfo {
    pub width: usize,
    pub height: usize,
    pub zero: i32,
    pub max_value: u16,
    pub cfa: CfaType,
    pub bin: u8,
    pub frame_type: FrameType,
    pub exposure: f64,
}

pub struct RawImage {
    info:    RawImageInfo,
    data:    Vec<u16>,
    cfa_arr: &'static CfaArray,
}

impl RawImage {
    pub fn new(width: usize, height: usize, max_value: u16) -> RawImage {
        let mut data = Vec::new();
        data.resize(width * height, 0);
        let info = RawImageInfo {
            width,
            height,
            zero: 0,
            cfa: CfaType::None,
            max_value,
            frame_type: FrameType::Lights,
            bin: 1,
            exposure: 0.0,
        };
        Self { info, data, cfa_arr: &[] }
    }

    pub fn new_from_info(info: RawImageInfo)  -> RawImage {
        let mut data = Vec::new();
        data.resize(info.width * info.height, 0);
        let cfa_arr = info.cfa.get_array();
        Self { info, data, cfa_arr }
    }

    pub fn new_from_fits_stream(
        mut stream:    impl SeekNRead,
        config_offset: Option<i32>,
    ) -> anyhow::Result<RawImage> {
        let reader = FitsReader::new(&mut stream)?;
        let Some(image_hdu) = reader.hdus.iter().find(|hdu| {
            hdu.dims().len() == 2
        }) else {
            anyhow::bail!("No RAW image found in fits data");
        };

        let width      = image_hdu.dims()[0];
        let height     = image_hdu.dims()[1];
        let exposure   = image_hdu.get_f64("EXPTIME" ).unwrap_or(0.0);
        let bayer      = image_hdu.get_str("BAYERPAT").unwrap_or_default();
        let bitdepth   = image_hdu.get_i64("BITDEPTH").unwrap_or(16) as i32;
        let bin        = image_hdu.get_i64("XBINNING").unwrap_or(1) as u8;
        let mut offset = image_hdu.get_i64("OFFSET"  ).unwrap_or(0) as i32;
        let frame_str  = image_hdu.get_str("FRAME"   );
        let data       = image_hdu.data_u16(&mut stream).unwrap();

        if let (0, Some(config_offset)) = (offset, config_offset) {
            offset = config_offset;
        }

        if bitdepth > 16 {
            anyhow::bail!("BITDEPTH = {} is not supported", bitdepth);
        }

        let max_value = ((1 << bitdepth) - 1) as u16;
        let cfa = CfaType::from_str(&bayer);
        let cfa_arr = cfa.get_array();
        let frame_type = FrameType::from_str(
            frame_str.as_deref().unwrap_or_default(),
            FrameType::Lights
        );

        let info = RawImageInfo {
            width,
            height,
            zero: offset,
            cfa,
            bin,
            max_value,
            frame_type,
            exposure,
        };
        Ok(Self {info, data, cfa_arr})
    }

    pub fn new_from_fits_file(file_name: &Path, offset: Option<i32>) -> anyhow::Result<RawImage> {
        let mut file = File::open(file_name)?;
        Self::new_from_fits_stream(&mut file, offset)
    }

    pub fn save_to_fits_file(&self, file_name: &Path) -> anyhow::Result<()> {
        _ = std::fs::remove_file(file_name);
        let dimensions = vec![
            self.info.height,
            self.info.width
        ];
        let image_description = ImageDescription {
            data_type: ImageType::UnsignedShort,
            dimensions: &dimensions,
        };
        let mut fptr =
            FitsFile::create(file_name)
                .with_custom_primary(&image_description)
                .open()?;
        let hdu = fptr.primary_hdu().unwrap();
        hdu.write_image(&mut fptr, self.data.as_slice())?;
        hdu.write_key(&mut fptr, "EXPTIME",  self.info.exposure)?;
        hdu.write_key(&mut fptr, "ROWORDER", "TOP-DOWN")?;
        hdu.write_key(&mut fptr, "FRAME",    self.info.frame_type.to_str())?;
        hdu.write_key(&mut fptr, "XBINNING", self.info.bin as i64)?;
        hdu.write_key(&mut fptr, "YBINNING", self.info.bin as i64)?;
        hdu.write_key(&mut fptr, "OFFSET",   self.info.zero as i64)?;
        if let Some(bayer) = self.info.cfa.to_str() {
            hdu.write_key(&mut fptr, "BAYERPAT", bayer)?;
        }
        Ok(())
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.data
    }

    pub fn info(&self) -> &RawImageInfo {
        &self.info
    }

    pub fn row(&self, y: usize) -> &[u16] {
        let pos = y * self.info.width;
        &self.data[pos..pos+self.info.width]
    }

    fn row_mut(&mut self, y: usize) -> &mut [u16] {
        let pos = y * self.info.width;
        &mut self.data[pos..pos+self.info.width]
    }

    fn col_iter(&self, x: usize) -> std::iter::StepBy<std::slice::Iter<u16>> {
        self.data[x..].iter().step_by(self.info.width)
    }

    fn get(&self, x: isize, y: isize) -> Option<u16> {
        if x < 0 || y < 0 || x >= self.info.width as isize {
            return None;
        }
        self.data.get(y as usize * self.info.width + x as usize).copied()
    }

    fn set(&mut self, x: isize, y: isize, value: u16) {
        if x < 0 || y < 0 || x >= self.info.width as isize || y >= self.info.height as isize{
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
            y1: y1 as usize,
            x2: x2 as usize,
            y2: y2 as usize,
            x: x1 as usize,
            y: y1 as usize,
        }
    }

    pub fn find_hot_pixels_in_master_dark(&self) -> Vec<BadPixel> {
        #[inline(always)]
        fn find_hot_pixels_step(
            raw:     &RawImage,
            step:    usize,
            mut fun: impl FnMut(u16, usize/*x*/, usize/*y*/, u16, u16, u16, u16, u16, u16)
        ) {
            for y in (0..raw.info.height).step_by(step) {
                let row = raw.row(y);
                fun(row[0], 0, y, row[1], row[2], row[3], row[4], row[5], row[6]);
                fun(row[1], 1, y, row[0], row[2], row[3], row[4], row[5], row[6]);
                fun(row[2], 2, y, row[0], row[1], row[3], row[4], row[5], row[6]);
                for (i, (v1, v2, v3, v4, v5, v6, v7))
                in row.iter().tuple_windows().enumerate() {
                    fun(*v4, i + 3, y, *v1, *v2, *v3, *v5, *v6, *v7);
                }
                let width = row.len()-7;
                let row_end = &row[width-7..];
                fun(row_end[4], width-3, y, row_end[0], row_end[1], row_end[2], row_end[3], row_end[5], row_end[6]);
                fun(row_end[5], width-2, y, row_end[0], row_end[1], row_end[2], row_end[3], row_end[4], row_end[6]);
                fun(row_end[6], width-1, y, row_end[0], row_end[1], row_end[2], row_end[3], row_end[4], row_end[5]);
            }
        }

        let mut diffs = Vec::with_capacity(self.data.len() / 3);
        find_hot_pixels_step(
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

        let pos = 95 * diffs.len() / 100;
        diffs.select_nth_unstable(pos);
        let border = diffs[pos] * 100;

        let mut tmp_result = HashSet::new();
        find_hot_pixels_step(
            self,
            1,
            |v, x, y, v1, v2, v3, v4, v5, v6| {
                let aver = v1 as i32 + v2 as i32 + v3 as i32 + v4 as i32 + v5 as i32 + v6 as i32;
                let diff = (v as i32) * 6 - aver;
                if diff > border {
                    tmp_result.insert((x, y));
                }
            }
        );

        tmp_result
            .iter()
            .map(|(x, y)| BadPixel{ x: *x as isize, y: *y as isize })
            .collect()
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
        frame_type:   FrameType,
    ) -> anyhow::Result<()> {
        if frame_type != master_frame.info.frame_type {
            anyhow::bail!(
                "Wrong frame type. Expected {:?}, found {:?}",
                frame_type,
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

    pub fn subtract_dark(&mut self, dark: &RawImage) -> anyhow::Result<()> {
        self.check_master_frame_is_compatible(dark, FrameType::Darks)?;
        debug_assert!(self.data.len() == dark.data.len());
        let dark_sum: i64 = dark
            .as_slice()
            .iter()
            .map(|v| *v as i64)
            .sum();
        let dark_aver = (dark_sum / dark.data.len() as i64) as i32;
        for (s, d) in self.data.iter_mut().zip(&dark.data) {
            let mut value = *s as i32;
            let dark_value = *d as i32 - dark.info.zero;
            value -= dark_value;
            if value < 0 { value = 0; }
            if value > u16::MAX as i32 { value = u16::MAX as i32; }
            *s = value as u16;
        }
        let new_max_value = self.info.max_value as i32 - (dark_aver - dark.info.zero);
        self.info.max_value = new_max_value as u16;
        Ok(())
    }

    pub fn apply_flat(&mut self, flat: &RawImage) -> anyhow::Result<()> {
        self.check_master_frame_is_compatible(flat, FrameType::Flats)?;
        debug_assert!(self.data.len() == flat.data.len());
        let zero = self.info.zero as i64;
        let flat_zero = flat.info.zero as i64;
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
        let zero = self.info.zero as i64;
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
        self.info.zero = 0;
    }

    pub fn filter_flat(&mut self) {
        let mut new_data = Vec::new();
        new_data.resize(self.data.len(), 0);
        new_data.par_chunks_exact_mut(self.info.width).enumerate().for_each(|(y, dst_row)| {
            let cfa_row = self.cfa_row(y);
            for (x, (dst, c)) in dst_row.iter_mut().zip(cfa_row.iter().cycle()).enumerate() {
                let mut cnt = 0_u32;
                let mut sum = 0_u32;
                let x = x as isize;
                let y = y as isize;
                for (_x, _y, v, cfa_c) in self.rect_iter(x-2, y-2, x+2, y+2) {
                    if *c != cfa_c { continue; }
                    sum += v as u32;
                    cnt += 1;
                }
                *dst = ((sum + cnt/2) / cnt) as u16;
            }
        });
        self.data = new_data;
    }

    pub fn demosaic(&self, mt: bool) -> Image {
        let mut result = Image::new_empty();
        self.demosaic_into(&mut result, mt);
        result
    }

    pub fn demosaic_into(&self, dst_img: &mut Image, mt: bool) {
        match self.info.cfa {
            CfaType::None =>
                self.copy_into_monochrome(dst_img),
            _ =>
                self.demosaic_linear(mt, dst_img),
        }
    }

    pub fn copy_into_monochrome(&self, dst_img: &mut Image) {
        dst_img.make_monochrome(
            self.info.width,
            self.info.height,
            self.info.zero,
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
            self.info.zero,
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

    pub fn save_to_internal_format(&self, file_name: &Path) -> anyhow::Result<()> {
        let mut file = BufWriter::new(File::create(file_name)?);
        file.write_all(&[RAW_IMAGE_FILE_VERSION])?;
        let info_text = serde_json::to_string(&self.info)?;
        file.write_all(&(info_text.len() as u32).to_be_bytes())?;
        file.write_all(info_text.as_bytes())?;
        let mut data_compressor = ValuesCompressor::new();
        let mut writer = BitWriter::endian(&mut file, BigEndian);
        for value in &self.data {
            data_compressor.write_u32(*value as u32, &mut writer)?;
        }
        writer.write(32, 0)?;
        writer.flush()?;
        Ok(())
    }

    pub fn load_from_internal_format(file_name: &Path) -> anyhow::Result<Self> {
        let mut file = BufReader::new(File::open(file_name)?);
        let mut vers_bytes = [0u8];
        file.read_exact(&mut vers_bytes)?;
        if vers_bytes[0] != RAW_IMAGE_FILE_VERSION {
            anyhow::bail!("Wrong RAW file version");
        }
        let mut len_bytes = [0u8; 4];
        file.read_exact(&mut len_bytes)?;
        let json_len = u32::from_be_bytes(len_bytes);
        let mut json_bytes = Vec::new();
        json_bytes.resize(json_len as usize, 0u8);
        file.read_exact(&mut json_bytes)?;
        let json = std::str::from_utf8(&json_bytes)?;
        let info: RawImageInfo = serde_json::from_str(json)?;
        let mut data_decompressor = ValuesDecompressor::new();
        let mut reader = BitReader::endian(&mut file, BigEndian);
        let mut data = Vec::with_capacity(info.width * info.height);
        for _ in 0..info.width * info.height {
            data.push(data_decompressor.read_u32(&mut reader)? as u16);
        }
        let cfa_arr = info.cfa.get_array();
        Ok(Self { info, data, cfa_arr })
    }
}

pub struct RawAdder {
    data:     Vec<u32>,
    info:     Option<RawImageInfo>,
    counter:  u32,
    zero_sum: i32,
}

impl RawAdder {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            info: None,
            counter: 0,
            zero_sum: 0,
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.data.shrink_to_fit();
        self.info = None;
        self.counter = 0;
        self.zero_sum = 0;
    }

    pub fn width(&self) -> usize {
        self.info.as_ref().map(|info| info.width).unwrap_or_default()
    }

    pub fn height(&self) -> usize {
        self.info.as_ref().map(|info| info.height).unwrap_or_default()
    }

    pub fn add(&mut self, raw: &RawImage) -> anyhow::Result<()> {
        if let Some(info) = &self.info {
            if info.width != raw.info.width
            || info.height != raw.info.height {
                anyhow::bail!(
                    "Size of images differ: adder {}x{}, raw {}x{}",
                    info.width, info.height,
                    raw.info.width, raw.info.height,
                );
            }
            if info.cfa != raw.info.cfa {
                anyhow::bail!("CFA of images differ");
            }
            if info.frame_type != raw.info.frame_type {
                anyhow::bail!("Frame type of images differ");
            }
        } else {
            self.info = Some(raw.info.clone());
            self.counter = 0;
            self.zero_sum = 0;
            self.data.resize(raw.data.len(), 0);
        }
        debug_assert!(self.data.len() == raw.data.len());
        for (s, d) in raw.data.iter().zip(&mut self.data) {
            *d += *s as u32;
        }
        self.counter += 1;
        self.zero_sum += raw.info.zero;

        Ok(())
    }

    pub fn get(&self) -> anyhow::Result<RawImage> {
        let Some(info) = &self.info else {
            anyhow::bail!("Raw added is empty");
        };
        let counter2 = self.counter/2;
        let data: Vec<_> = self.data
            .iter()
            .map(|v| ((*v + counter2) / self.counter) as u16)
            .collect();
        let cfa_arr = info.cfa.get_array();
        let mut info = info.clone();
        info.zero = (self.zero_sum + counter2 as i32) / self.counter as i32;
        Ok(RawImage { info, data, cfa_arr })
    }
}

type RawRectIteratorCfaIter<'a> = std::iter::Skip<std::iter::Cycle<std::slice::Iter<'a, CfaColor>>>;

pub struct RawRectIterator<'a> {
    raw: &'a RawImage,
    iter: std::slice::Iter<'a, u16>,
    cfa_iter: RawRectIteratorCfaIter<'a>,
    x1: usize,
    y1: usize,
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

impl<'a> Iterator for RawRectIterator<'a> {
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

const F32_COMPR_BUF_SIZE: usize = 8;

pub struct ValuesCompressor {
    data: [u32; F32_COMPR_BUF_SIZE],
    data_ptr: usize,
    prev: u32,
}

impl ValuesCompressor {
    pub fn new() -> Self {
        Self {
            data: [0_u32; F32_COMPR_BUF_SIZE],
            data_ptr: 0,
            prev: 0,
        }
    }

    pub fn write_u32<T: BitWrite>(&mut self, value: u32, writer: &mut T) -> std::io::Result<()> {
        self.data[self.data_ptr] = value ^ self.prev;
        self.prev = value;
        self.data_ptr += 1;
        if self.data_ptr == F32_COMPR_BUF_SIZE {
            self.flush(writer)?;
        }
        Ok(())
    }

    pub fn write_f32<T: BitWrite>(&mut self, value: f32, writer: &mut T) -> std::io::Result<()> {
        self.write_u32(value.to_bits(), writer)
    }

    pub fn flush<T: BitWrite>(&mut self, writer: &mut T) -> std::io::Result<()> {
        if self.data_ptr == 0 {
            return Ok(())
        }
        self.data[self.data_ptr..].fill(0);
        let min_lz = self.data
            .iter()
            .map(|v| v.leading_zeros())
            .min()
            .unwrap_or(0);
        let mut max_len = 32-min_lz;
        if max_len == 0 { max_len = 1; }
        writer.write(5, max_len-1)?;
        for v in self.data {
            writer.write(max_len, v)?;
        }
        self.data_ptr = 0;
        Ok(())
    }
}

pub struct ValuesDecompressor {
    values: [u32; F32_COMPR_BUF_SIZE],
    values_ptr: usize,
    prev_value: u32,
}

impl ValuesDecompressor {
    pub fn new() -> Self {
        Self {
            values: [0_u32; F32_COMPR_BUF_SIZE],
            values_ptr: F32_COMPR_BUF_SIZE,
            prev_value: 0,
        }
    }

    pub fn read_f32<T: BitRead>(&mut self, reader: &mut T) -> std::io::Result<f32> {
        Ok(f32::from_bits(self.read_u32(reader)?))
    }

    pub fn read_u32<T: BitRead>(&mut self, reader: &mut T) -> std::io::Result<u32> {
        if self.values_ptr == F32_COMPR_BUF_SIZE {
            self.values_ptr = 0;
            let len = reader.read::<u32>(5)? + 1;
            for v in &mut self.values {
                self.prev_value ^= reader.read::<u32>(len)?;
                *v = self.prev_value;
            }
        }
        let result = self.values[self.values_ptr];
        self.values_ptr += 1;
        Ok(result)
    }
}


#[inline(always)]
pub fn median3<T>(v1: T, v2: T, v3: T) -> T
where T: std::cmp::Ord {
    if (v1 > v2) ^ (v1 > v3) {
        return v1;
    } else if (v2 < v1) ^ (v2 < v3) {
        return v2;
    }
    return v3;
}

#[inline(always)]
pub fn median5<T>(v1: T, v2: T, v3: T, v4: T, v5: T) -> T
where T: std::cmp::Ord + Copy {
    let v6 = T::max(T::min(v1, v2), T::min(v3, v4));
    let v7 = T::min(T::max(v1, v2), T::max(v3, v4));
    return median3(v5, v6, v7);
}
