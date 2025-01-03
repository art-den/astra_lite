use std::sync::Arc;

use itertools::{izip, Itertools};

use crate::{
    utils::math::linear_interpolate, PreviewColorMode, PreviewScale
};

use super::{cam_db::*, histogram::Histogram, image::{Image, ImageLayer}};

#[derive(PartialEq, Clone)]
pub struct PreviewParams {
    pub dark_lvl:         f64,
    pub light_lvl:        f64,
    pub gamma:            f64,
    pub pr_area_width:    usize,
    pub pr_area_height:   usize,
    pub scale:            PreviewScale,
    pub orig_frame_in_ls: bool,
    pub remove_gradient:  bool,
    pub color:            PreviewColorMode,
    pub wb:               Option<[f64; 3]>,
}

impl PreviewParams {
    pub fn get_preview_img_size(&self, orig_width: usize, orig_height: usize) -> (usize, usize) {
        match self.scale {
            PreviewScale::FitWindow => {
                let img_ratio = orig_width as f64 / orig_height as f64;
                let gui_ratio = self.pr_area_width as f64 / self.pr_area_height as f64;
                if img_ratio > gui_ratio {
                    (self.pr_area_width, (self.pr_area_width as f64 / img_ratio) as usize)
                } else {
                    ((self.pr_area_height as f64 * img_ratio) as usize, self.pr_area_height)
                }
            }
            PreviewScale::Original =>
                (orig_width, orig_height),
            PreviewScale::P75 =>
                (3*orig_width/4, 3*orig_height/4),
            PreviewScale::P50 =>
                (orig_width/2, orig_height/2),
            PreviewScale::P33 =>
                (orig_width/3, orig_height/3),
            PreviewScale::P25 =>
                (orig_width/4, orig_height/4),
            PreviewScale::CenterAndCorners => {
                let min_size = self.pr_area_width.min(self.pr_area_height);
                let min_size_protected = min_size.max(42);
                (min_size_protected, min_size_protected)
            }
        }
    }

    fn calc_reduct_ratio(&self, img_width: usize, img_height: usize) -> usize {
        match self.scale {
            PreviewScale::FitWindow => {
                if img_width/4 > self.pr_area_width && img_height/4 > self.pr_area_height {
                    4
                } else if img_width/3 > self.pr_area_width && img_height/3 > self.pr_area_height {
                    3
                } else if img_width/2 > self.pr_area_width && img_height/2 > self.pr_area_height {
                    2
                } else {
                    1
                }
            },
            PreviewScale::Original => 1,
            PreviewScale::P75 => 1,
            PreviewScale::P50 => 2,
            PreviewScale::P33 => 3,
            PreviewScale::P25 => 4,
            PreviewScale::CenterAndCorners => 1,
        }
    }

}

#[derive(Default, Debug)]
pub struct DarkLightLevels {
    pub dark:  f64,
    pub light: f64,
}

#[derive(Clone)]
pub struct SharedBytes {
    data: Arc<Vec<u8>>,
}

impl SharedBytes {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data: Arc::new(data)
        }
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        self.data.as_slice()
    }
}

pub struct PreviewRgbData {
    pub width:          usize,
    pub height:         usize,
    pub orig_width:     usize,
    pub orig_height:    usize,
    pub bytes:          SharedBytes,
    pub is_color_image: bool,   // originally color
    pub sensor_name:    String, // censor for auto WB
}

pub fn get_preview_rgb_data(
    image:  &Image,
    hist:   &Histogram,
    params: &PreviewParams,
) -> Option<PreviewRgbData> {
    if (hist.l.is_none() && hist.b.is_none()) || image.is_empty() {
        return None;
    }

    let reduct_ratio = params.calc_reduct_ratio(image.width(), image.height());
    log::debug!("preview reduct_ratio = {}", reduct_ratio);

    let levels = calc_levels(
        hist,
        params,
        image.max_value() as f64
    );
    log::debug!("preview levels = {:?}", levels);

    let (wb, censor) = get_wb_and_sensor(&params.wb, &image.raw_info.as_ref().map(|info| info.camera.as_str()).unwrap_or_default());
    log::debug!("preview wb and sensor = {:?} {}", wb, censor);

    let (bytes, width, height) = to_grb_bytes(
        image,
        params,
        &levels,
        reduct_ratio,
        &wb
    );

    Some(PreviewRgbData {
        width,
        height,
        bytes:          SharedBytes::new(bytes),
        orig_width:     image.width(),
        orig_height:    image.height(),
        is_color_image: image.is_color(),
        sensor_name:    censor,
    })
}

#[derive(Debug)]
struct PreviewLevels {
    r: DarkLightLevels,
    g: DarkLightLevels,
    b: DarkLightLevels,
    l: DarkLightLevels,
}

fn calc_levels(
    hist:      &Histogram,
    params:    &PreviewParams,
    light_max: f64,
) -> PreviewLevels {
    const WB_PERCENTILE:        f64 = 45.0;
    const DARK_MIN_PERCENTILE:  f64 = 1.0;
    const DARK_MAX_PERCENTILE:  f64 = 60.0;
    const LIGHT_MIN_PERCENTILE: f64 = 95.0;

    let light_lvl = params.light_lvl.powf(0.05);

    let l_levels = if let Some(hist) = &hist.l {
        let dark_min = hist.get_percentile(DARK_MIN_PERCENTILE) as f64;
        let dark_max = hist.get_percentile(DARK_MAX_PERCENTILE) as f64;
        let light_min = hist.get_percentile(LIGHT_MIN_PERCENTILE) as f64;
        let mut dark = linear_interpolate(params.dark_lvl, 1.0, 0.0, dark_min, dark_max);
        let mut light = linear_interpolate(light_lvl, 1.0, 0.0, light_min, light_max);
        if (light - dark) < 2.0 { light += 1.0; dark -= 1.0; }
        DarkLightLevels { dark, light }
    } else {
        DarkLightLevels::default()
    };

    let (g_levels, g_wb) = if let Some(hist) = &hist.g {
        let dark_min = hist.get_percentile(DARK_MIN_PERCENTILE) as f64;
        let dark_max = hist.get_percentile(DARK_MAX_PERCENTILE) as f64;
        let light_min = hist.get_percentile(LIGHT_MIN_PERCENTILE) as f64;
        let mut dark = linear_interpolate(params.dark_lvl, 1.0, 0.0, dark_min, dark_max);
        let mut light = linear_interpolate(light_lvl, 1.0, 0.0, light_min, light_max);
        if (light - dark) < 2.0 { light += 1.0; dark -= 1.0; }
        let wb = hist.get_percentile(WB_PERCENTILE) as f64;
        (DarkLightLevels { dark, light }, wb)
    } else {
        (DarkLightLevels::default(), 0.0)
    };

    let g_range = g_levels.light - g_levels.dark;

    let r_levels = if let Some(hist) = &hist.r {
        let wb = hist.get_percentile(WB_PERCENTILE) as f64;
        let dark = g_levels.dark + (wb - g_wb);
        DarkLightLevels { dark, light: dark + g_range }
    } else {
        DarkLightLevels::default()
    };

    let b_levels = if let Some(hist) = &hist.b {
        let wb = hist.get_percentile(WB_PERCENTILE) as f64;
        let dark = g_levels.dark + (wb - g_wb);
        DarkLightLevels { dark, light: dark + g_range }
    } else {
        DarkLightLevels::default()
    };

    PreviewLevels {
        r: r_levels,
        g: g_levels,
        b: b_levels,
        l: l_levels,
    }
}

fn get_wb_and_sensor(wb: &Option<[f64; 3]>, camera: &str) -> ([f64; 3], String) {
    let cam_info = get_cam_info(camera);
    let auto_wb_coeffs = cam_info.as_ref().map(|cam_info| cam_info.wb).unwrap_or([1.0, 1.0, 1.0]);
    let mut r_wb = wb.map(|wb| wb[0]).unwrap_or(auto_wb_coeffs[0] as f64);
    let mut g_wb = wb.map(|wb| wb[1]).unwrap_or(auto_wb_coeffs[1] as f64);
    let mut b_wb = wb.map(|wb| wb[2]).unwrap_or(auto_wb_coeffs[2] as f64);
    let mut min_wb = r_wb.min(g_wb).min(b_wb);
    if min_wb <= 0.0 { min_wb = 1.0; }
    r_wb /= min_wb;
    g_wb /= min_wb;
    b_wb /= min_wb;
    let sensor = cam_info.as_ref()
        .map(|cam_info|
            cam_info.sensor.to_string()
        ).unwrap_or_default();
    ([r_wb, g_wb, b_wb], sensor)
}

fn to_grb_bytes(
    image:        &Image,
    params:       &PreviewParams,
    levels:       &PreviewLevels,
    reduct_ratio: usize,
    wb:           &[f64; 3],
) -> (Vec<u8>, usize, usize) {
    let r_table = create_gamma_table(levels.r.dark, levels.r.light, params.gamma, wb[0]);
    let g_table = create_gamma_table(levels.g.dark, levels.g.light, params.gamma, wb[1]);
    let b_table = create_gamma_table(levels.b.dark, levels.b.light, params.gamma, wb[2]);
    let l_table = create_gamma_table(levels.l.dark, levels.l.light, params.gamma, 1.0);
    let (result_width, result_height) = params.get_preview_img_size(image.width(), image.height());

    let rgb_bytes = if image.is_color() && params.color == PreviewColorMode::Rgb {
        match (params.scale, reduct_ratio) {
            (PreviewScale::CenterAndCorners, _) => to_grb_bytes_corners_rgb(image, &r_table, &g_table, &b_table, result_width, result_height),
            (_, 1) => to_grb_bytes_no_reduct_rgb(image, &r_table, &g_table, &b_table, image.width(), image.height()),
            (_, 2) => to_grb_bytes_reduct2_rgb  (image, &r_table, &g_table, &b_table, image.width(), image.height()),
            (_, 3) => to_grb_bytes_reduct3_rgb  (image, &r_table, &g_table, &b_table, image.width(), image.height()),
            (_, 4) => to_grb_bytes_reduct4_rgb  (image, &r_table, &g_table, &b_table, image.width(), image.height()),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    } else {
        let (layer, table) = match (image.is_color(), params.color) {
            (false, _)                   => (&image.l, l_table),
            (_, PreviewColorMode::Red)   => (&image.r, r_table),
            (_, PreviewColorMode::Green) => (&image.g, g_table),
            (_, PreviewColorMode::Blue)  => (&image.b, b_table),
            _ => unreachable!(),
        };
        match (params.scale, reduct_ratio) {
            (PreviewScale::CenterAndCorners, _) => to_grb_bytes_corners_mono(layer, &table, result_width, result_height),
            (_, 1) => to_grb_bytes_no_reduct_mono(layer, &table, image.width(), image.height()),
            (_, 2) => to_grb_bytes_reduct2_mono  (layer, &table, image.width(), image.height()),
            (_, 3) => to_grb_bytes_reduct3_mono  (layer, &table, image.width(), image.height()),
            (_, 4) => to_grb_bytes_reduct4_mono  (layer, &table, image.width(), image.height()),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    };

    let (width, heigth) = if params.scale == PreviewScale::CenterAndCorners {
        (result_width, result_height)
    } else {
        (image.width()/reduct_ratio, image.height()/reduct_ratio)
    };

    (rgb_bytes, width, heigth)
}

fn create_gamma_table(min_value: f64, max_value: f64, gamma: f64, k: f64) -> Vec<u8> {
    let mut table = Vec::new();
    if min_value == 0.0 && max_value == 0.0 {
        return table;
    }
    for i in 0..=u16::MAX {
        let v = linear_interpolate(i as f64, min_value, max_value, 0.0, 1.0);
        let v = v * k;
        let table_v = if v < 0.0 {
            0.0
        } else if v > 1.0 {
            u8::MAX as f64
        } else {
            v.powf(1.0 / gamma) * u8::MAX as f64
        };
        table.push(table_v as u8);
    }
    table
}

#[derive(Debug)]
struct CopyRectParams {
    src_left: usize,
    src_top: usize,
    dst_left: usize,
    dst_top: usize,
    width: usize,
    height: usize,
}

fn get_rects_for_corners_and_center(
    src_width: usize,
    src_height: usize,
    width:  usize,
    height: usize
) -> Vec<CopyRectParams> {
    const SPACING: usize = 6;
    fn pos(img_size: usize, rect_size: usize, pos: usize) -> usize {
        match pos {
            0 => 0,
            1 => (img_size - rect_size) / 2,
            2 => img_size - rect_size,
            _ => unreachable!(),
        }
    }
    let mut result = Vec::new();
    let rect_width = (width - 2 * SPACING) / 3;
    let rect_height = (height - 2 * SPACING) / 3;
    for i in 0..3 {
        for j in 0..3 {
            result.push(CopyRectParams {
                src_left: pos(src_width, rect_width, i),
                src_top: pos(src_height, rect_height, j),
                dst_left: pos(width, rect_width, i),
                dst_top: pos(height, rect_height, j),
                width: rect_width,
                height: rect_height,
            });
        }
    }
    result
}

fn to_grb_bytes_corners_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
    width:   usize,
    height:  usize
) -> Vec<u8> {
    let mut rgb_bytes = Vec::new();
    rgb_bytes.resize(3 * width * height, 0);
    let rects = get_rects_for_corners_and_center(image.width(), image.height(), width, height);
    for rect in rects {
        for i in 0..rect.height {
            let src_y = rect.src_top + i;
            let dst_y = rect.dst_top + i;
            let r_row = &image.r.row(src_y)[rect.src_left..rect.src_left + rect.width];
            let g_row = &image.g.row(src_y)[rect.src_left..rect.src_left + rect.width];
            let b_row = &image.b.row(src_y)[rect.src_left..rect.src_left + rect.width];
            let dst_row_pos = 3 * (dst_y * width + rect.dst_left);
            let dst_row = &mut rgb_bytes[dst_row_pos..dst_row_pos + 3 * rect.width];
            for (r, g, b, (dst_r, dst_g, dst_b)) in
            izip!(r_row, g_row, b_row, dst_row.iter_mut().tuples()) {
                *dst_r = r_table[*r as usize];
                *dst_g = g_table[*g as usize];
                *dst_b = b_table[*b as usize];
            }
        }
    }
    rgb_bytes
}

fn to_grb_bytes_corners_mono(
    layer:  &ImageLayer<u16>,
    table:  &[u8],
    width:  usize,
    height: usize
) -> Vec<u8> {
    let mut rgb_bytes = Vec::new();
    rgb_bytes.resize(3 * width * height, 0);
    let rects = get_rects_for_corners_and_center(layer.width(), layer.height(), width, height);
    for rect in rects {
        for i in 0..rect.height {
            let src_y = rect.src_top + i;
            let dst_y = rect.dst_top + i;
            let src_row = &layer.row(src_y)[rect.src_left..rect.src_left + rect.width];
            let dst_row_pos = 3 * (dst_y * width + rect.dst_left);
            let dst_row = &mut rgb_bytes[dst_row_pos..dst_row_pos + 3 * rect.width];
            for (v, (dst_r, dst_g, dst_b)) in
            izip!(src_row, dst_row.iter_mut().tuples()) {
                *dst_r = table[*v as usize];
                *dst_g = table[*v as usize];
                *dst_b = table[*v as usize];
            }
        }
    }
    rgb_bytes
}

fn to_grb_bytes_no_reduct_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
    width:   usize,
    height:  usize
) -> Vec<u8> {
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for row in 0..height {
        let r_iter = image.r.row(row).iter();
        let g_iter = image.g.row(row).iter();
        let b_iter = image.b.row(row).iter();
        for (r, g, b) in
        izip!(r_iter, g_iter, b_iter) {
            rgb_bytes.push(r_table[*r as usize]);
            rgb_bytes.push(g_table[*g as usize]);
            rgb_bytes.push(b_table[*b as usize]);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_no_reduct_mono(
    layer:  &ImageLayer<u16>,
    table:  &[u8],
    width:  usize,
    height: usize
) -> Vec<u8> {
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for row in 0..height {
        for l in layer.row(row).iter() {
            let l = table[*l as usize];
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            rgb_bytes.push(l);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_reduct2_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
    width:   usize,
    height:  usize
) -> Vec<u8> {
    let width = width / 2;
    let height = height / 2;
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        let mut r0 = image.r.row(2*y).as_ptr();
        let mut r1 = image.r.row(2*y+1).as_ptr();
        let mut g0 = image.g.row(2*y).as_ptr();
        let mut g1 = image.g.row(2*y+1).as_ptr();
        let mut b0 = image.b.row(2*y).as_ptr();
        let mut b1 = image.b.row(2*y+1).as_ptr();
        for _ in 0..width {
            let r = unsafe {(
                *r0 as u32 + *r0.offset(1) as u32 +
                *r1 as u32 + *r1.offset(1) as u32 + 2
            ) / 4};
            let g = unsafe {(
                *g0 as u32 + *g0.offset(1) as u32 +
                *g1 as u32 + *g1.offset(1) as u32 + 2
            ) / 4};
            let b = unsafe {(
                *b0 as u32 + *b0.offset(1) as u32 +
                *b1 as u32 + *b1.offset(1) as u32 + 2
            ) / 4};
            rgb_bytes.push(r_table[r as usize]);
            rgb_bytes.push(g_table[g as usize]);
            rgb_bytes.push(b_table[b as usize]);
            r0 = r0.wrapping_offset(2);
            r1 = r1.wrapping_offset(2);
            g0 = g0.wrapping_offset(2);
            g1 = g1.wrapping_offset(2);
            b0 = b0.wrapping_offset(2);
            b1 = b1.wrapping_offset(2);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_reduct2_mono(
    layer:  &ImageLayer<u16>,
    table:  &[u8],
    width:  usize,
    height: usize
) -> Vec<u8> {
    let width = width / 2;
    let height = height / 2;
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        let mut l0 = layer.row(2*y).as_ptr();
        let mut l1 = layer.row(2*y+1).as_ptr();
        for _ in 0..width {
            let l = unsafe {(
                *l0 as u32 + *l0.offset(1) as u32 +
                *l1 as u32 + *l1.offset(1) as u32 + 2
            ) / 4};
            let l = table[l as usize];
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            l0 = l0.wrapping_offset(2);
            l1 = l1.wrapping_offset(2);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_reduct3_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
    width:   usize,
    height:  usize
) -> Vec<u8> {
    let width = width / 3;
    let height = height / 3;
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        let mut r0 = image.r.row(3*y).as_ptr();
        let mut r1 = image.r.row(3*y+1).as_ptr();
        let mut r2 = image.r.row(3*y+2).as_ptr();
        let mut g0 = image.g.row(3*y).as_ptr();
        let mut g1 = image.g.row(3*y+1).as_ptr();
        let mut g2 = image.g.row(3*y+2).as_ptr();
        let mut b0 = image.b.row(3*y).as_ptr();
        let mut b1 = image.b.row(3*y+1).as_ptr();
        let mut b2 = image.b.row(3*y+2).as_ptr();
        for _ in 0..width {
            let r = unsafe {(
                *r0 as u32 + *r0.offset(1) as u32 + *r0.offset(2) as u32 +
                *r1 as u32 + *r1.offset(1) as u32 + *r1.offset(2) as u32 +
                *r2 as u32 + *r2.offset(1) as u32 + *r2.offset(2) as u32 + 4
            ) / 9};
            let g = unsafe {(
                *g0 as u32 + *g0.offset(1) as u32 + *g0.offset(2) as u32 +
                *g1 as u32 + *g1.offset(1) as u32 + *g1.offset(2) as u32 +
                *g2 as u32 + *g2.offset(1) as u32 + *g2.offset(2) as u32 + 4
            ) / 9};
            let b = unsafe {(
                *b0 as u32 + *b0.offset(1) as u32 + *b0.offset(2) as u32 +
                *b1 as u32 + *b1.offset(1) as u32 + *b1.offset(2) as u32 +
                *b2 as u32 + *b2.offset(1) as u32 + *b2.offset(2) as u32 + 4
            ) / 9};
            rgb_bytes.push(r_table[r as usize]);
            rgb_bytes.push(g_table[g as usize]);
            rgb_bytes.push(b_table[b as usize]);
            r0 = r0.wrapping_offset(3);
            r1 = r1.wrapping_offset(3);
            r2 = r2.wrapping_offset(3);
            g0 = g0.wrapping_offset(3);
            g1 = g1.wrapping_offset(3);
            g2 = g2.wrapping_offset(3);
            b0 = b0.wrapping_offset(3);
            b1 = b1.wrapping_offset(3);
            b2 = b2.wrapping_offset(3);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_reduct3_mono(
    layer:  &ImageLayer<u16>,
    table:  &[u8],
    width:  usize,
    height: usize
) -> Vec<u8> {
    let width = width / 3;
    let height = height / 3;
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        let mut l0 = layer.row(3*y).as_ptr();
        let mut l1 = layer.row(3*y+1).as_ptr();
        let mut l2 = layer.row(3*y+2).as_ptr();
        for _ in 0..width {
            let l = unsafe {(
                *l0 as u32 + *l0.offset(1) as u32 + *l0.offset(2) as u32 +
                *l1 as u32 + *l1.offset(1) as u32 + *l1.offset(2) as u32 +
                *l2 as u32 + *l2.offset(1) as u32 + *l2.offset(2) as u32 + 4
            ) / 9};
            let l = table[l as usize];
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            l0 = l0.wrapping_offset(3);
            l1 = l1.wrapping_offset(3);
            l2 = l2.wrapping_offset(3);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_reduct4_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
    width:   usize,
    height:  usize
) -> Vec<u8> {
    let width = width / 4;
    let height = height / 4;
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        let mut r0 = image.r.row(4*y).as_ptr();
        let mut r1 = image.r.row(4*y+1).as_ptr();
        let mut r2 = image.r.row(4*y+2).as_ptr();
        let mut r3 = image.r.row(4*y+3).as_ptr();
        let mut g0 = image.g.row(4*y).as_ptr();
        let mut g1 = image.g.row(4*y+1).as_ptr();
        let mut g2 = image.g.row(4*y+2).as_ptr();
        let mut g3 = image.g.row(4*y+3).as_ptr();
        let mut b0 = image.b.row(4*y).as_ptr();
        let mut b1 = image.b.row(4*y+1).as_ptr();
        let mut b2 = image.b.row(4*y+2).as_ptr();
        let mut b3 = image.b.row(4*y+3).as_ptr();
        for _ in 0..width {
            let r = unsafe {(
                *r0 as u32 + *r0.offset(1) as u32 + *r0.offset(2) as u32 + *r0.offset(3) as u32 +
                *r1 as u32 + *r1.offset(1) as u32 + *r1.offset(2) as u32 + *r1.offset(3) as u32 +
                *r2 as u32 + *r2.offset(1) as u32 + *r2.offset(2) as u32 + *r2.offset(3) as u32 +
                *r3 as u32 + *r3.offset(1) as u32 + *r3.offset(2) as u32 + *r3.offset(3) as u32 + 8
            ) / 16};
            let g = unsafe {(
                *g0 as u32 + *g0.offset(1) as u32 + *g0.offset(2) as u32 + *g0.offset(3) as u32 +
                *g1 as u32 + *g1.offset(1) as u32 + *g1.offset(2) as u32 + *g1.offset(3) as u32 +
                *g2 as u32 + *g2.offset(1) as u32 + *g2.offset(2) as u32 + *g2.offset(3) as u32 +
                *g3 as u32 + *g3.offset(1) as u32 + *g3.offset(2) as u32 + *g3.offset(3) as u32 + 8
            ) / 16};
            let b = unsafe {(
                *b0 as u32 + *b0.offset(1) as u32 + *b0.offset(2) as u32 + *b0.offset(3) as u32 +
                *b1 as u32 + *b1.offset(1) as u32 + *b1.offset(2) as u32 + *b1.offset(3) as u32 +
                *b2 as u32 + *b2.offset(1) as u32 + *b2.offset(2) as u32 + *b2.offset(3) as u32 +
                *b3 as u32 + *b3.offset(1) as u32 + *b3.offset(2) as u32 + *b3.offset(3) as u32 + 8
            ) / 16};
            rgb_bytes.push(r_table[r as usize]);
            rgb_bytes.push(g_table[g as usize]);
            rgb_bytes.push(b_table[b as usize]);
            r0 = r0.wrapping_offset(4);
            r1 = r1.wrapping_offset(4);
            r2 = r2.wrapping_offset(4);
            r3 = r3.wrapping_offset(4);
            g0 = g0.wrapping_offset(4);
            g1 = g1.wrapping_offset(4);
            g2 = g2.wrapping_offset(4);
            g3 = g3.wrapping_offset(4);
            b0 = b0.wrapping_offset(4);
            b1 = b1.wrapping_offset(4);
            b2 = b2.wrapping_offset(4);
            b3 = b3.wrapping_offset(4);
        }
    }
    rgb_bytes
}

fn to_grb_bytes_reduct4_mono(
    layer:  &ImageLayer<u16>,
    table:  &[u8],
    width:  usize,
    height: usize
) -> Vec<u8> {
    let width = width / 4;
    let height = height / 4;
    let mut rgb_bytes = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        let mut l0 = layer.row(4*y).as_ptr();
        let mut l1 = layer.row(4*y+1).as_ptr();
        let mut l2 = layer.row(4*y+2).as_ptr();
        let mut l3 = layer.row(4*y+3).as_ptr();
        for _ in 0..width {
            let l = unsafe {(
                *l0 as u32 + *l0.offset(1) as u32 + *l0.offset(2) as u32 + *l0.offset(3) as u32 +
                *l1 as u32 + *l1.offset(1) as u32 + *l1.offset(2) as u32 + *l1.offset(3) as u32 +
                *l2 as u32 + *l2.offset(1) as u32 + *l2.offset(2) as u32 + *l2.offset(3) as u32 +
                *l3 as u32 + *l3.offset(1) as u32 + *l3.offset(2) as u32 + *l3.offset(3) as u32 + 8
            ) / 16};
            let l = table[l as usize];
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            rgb_bytes.push(l);
            l0 = l0.wrapping_offset(4);
            l1 = l1.wrapping_offset(4);
            l2 = l2.wrapping_offset(4);
            l3 = l3.wrapping_offset(4);
        }
    }
    rgb_bytes
}
