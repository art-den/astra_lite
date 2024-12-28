use std::sync::Arc;

use itertools::izip;

use crate::{
    core::frame_processing::PreviewParams, utils::math::linear_interpolate, PreviewColorMode, PreviewImgSize, PreviewScale
};

use super::{histogram::Histogram, image::{Image, ImageLayer}};

#[derive(Default)]
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

pub struct RgbU8Data {
    pub width:          usize,
    pub height:         usize,
    pub orig_width:     usize,
    pub orig_height:    usize,
    pub bytes:          SharedBytes,
    pub is_color_image: bool,
}

pub fn get_rgb_data_from_preview_image(
    image:  &Image,
    hist:   &Histogram,
    params: &PreviewParams,
) -> Option<RgbU8Data> {
    if hist.l.is_none() && hist.b.is_none() {
        return None;
    }

    let reduct_ratio = calc_reduct_ratio(
        params,
        image.width(),
        image.height()
    );
    log::debug!("reduct_ratio = {}", reduct_ratio);

    const WB_PERCENTILE:        usize = 45;
    const DARK_MIN_PERCENTILE:  usize = 1;
    const DARK_MAX_PERCENTILE:  usize = 60;
    const LIGHT_MIN_PERCENTILE: usize = 95;

    let light_max = image.max_value() as f64;
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

    to_grb_bytes(
        image,
        &l_levels,
        &r_levels,
        &g_levels,
        &b_levels,
        params.gamma,
        reduct_ratio,
        params.color,
    )
}

fn calc_reduct_ratio(options: &PreviewParams, img_width: usize, img_height: usize) -> usize {
    match &options.img_size {
        &PreviewImgSize::Fit{width, height} => {
            if img_width/4 > width && img_height/4 > height {
                4
            } else if img_width/3 > width && img_height/3 > height {
                3
            } else if img_width/2 > width && img_height/2 > height {
                2
            } else {
                1
            }
        },
        PreviewImgSize::Scale(PreviewScale::Original) => 1,
        PreviewImgSize::Scale(PreviewScale::P75) => 1,
        PreviewImgSize::Scale(PreviewScale::P50) => 2,
        PreviewImgSize::Scale(PreviewScale::P33) => 3,
        PreviewImgSize::Scale(PreviewScale::P25) => 4,
        PreviewImgSize::Scale(PreviewScale::FitWindow) => unreachable!(),
    }
}

pub fn to_grb_bytes(
    image:        &Image,
    l_levels:     &DarkLightLevels,
    r_levels:     &DarkLightLevels,
    g_levels:     &DarkLightLevels,
    b_levels:     &DarkLightLevels,
    gamma:        f64,
    reduct_ratio: usize,
    color_mode:   PreviewColorMode,
) -> Option<RgbU8Data> {
    if image.is_empty() {
        return None;
    }

    let r_table = create_gamma_table(r_levels.dark, r_levels.light, gamma);
    let g_table = create_gamma_table(g_levels.dark, g_levels.light, gamma);
    let b_table = create_gamma_table(b_levels.dark, b_levels.light, gamma);
    let l_table = create_gamma_table(l_levels.dark, l_levels.light, gamma);

    let rgb_bytes = if image.is_color() && color_mode == PreviewColorMode::Rgb {
        match reduct_ratio {
            1 => to_grb_bytes_no_reduct_rgb(image, &r_table, &g_table, &b_table, image.width(), image.height()),
            2 => to_grb_bytes_reduct2_rgb  (image, &r_table, &g_table, &b_table, image.width(), image.height()),
            3 => to_grb_bytes_reduct3_rgb  (image, &r_table, &g_table, &b_table, image.width(), image.height()),
            4 => to_grb_bytes_reduct4_rgb  (image, &r_table, &g_table, &b_table, image.width(), image.height()),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    } else {
        let (layer, table) = match (image.is_color(), color_mode) {
            (false, _)                   => (&image.l, l_table),
            (_, PreviewColorMode::Red)   => (&image.r, r_table),
            (_, PreviewColorMode::Green) => (&image.g, g_table),
            (_, PreviewColorMode::Blue)  => (&image.b, b_table),
            _ => unreachable!(),
        };
        match reduct_ratio {
            1 => to_grb_bytes_no_reduct_mono(layer, &table, image.width(), image.height()),
            2 => to_grb_bytes_reduct2_mono  (layer, &table, image.width(), image.height()),
            3 => to_grb_bytes_reduct3_mono  (layer, &table, image.width(), image.height()),
            4 => to_grb_bytes_reduct4_mono  (layer, &table, image.width(), image.height()),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    };

    Some(RgbU8Data {
        width:          image.width() / reduct_ratio,
        height:         image.height() / reduct_ratio,
        bytes:          SharedBytes::new(rgb_bytes),
        orig_width:     image.width(),
        orig_height:    image.height(),
        is_color_image: image.is_color(),
    })
}

fn create_gamma_table(min_value: f64, max_value: f64, gamma: f64) -> Vec<u8> {
    let mut table = Vec::new();
    if min_value == 0.0 && max_value == 0.0 {
        return table;
    }
    for i in 0..=u16::MAX {
        let v = linear_interpolate(i as f64, min_value, max_value, 0.0, 1.0);
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
