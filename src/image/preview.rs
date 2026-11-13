use std::sync::Arc;

use itertools::{izip, Itertools};

use crate::{
    utils::math::linear_interpolate, options::preview::{PreviewColorMode, PreviewScale}
};

use super::{cam_db::*, histogram::Histogram, image::{Image, ImageLayer}, stars::StarItems};

#[derive(PartialEq, Clone, Default)]
pub struct PreviewParams {
    pub dark_lvl:         f64,
    pub light_lvl:        f64,
    pub gamma:            f64,
    pub pr_area_width:    usize,
    pub pr_area_height:   usize,
    pub scale:            PreviewScale,
    pub remove_gradient:  bool,
    pub color:            PreviewColorMode,
    pub wb:               Option<[f64; 3]>,
    pub stars:            bool,
}

impl PreviewParams {
    pub fn get_preview_img_size_for_scale(
        &self,
        scale:       PreviewScale,
        orig_width:  usize,
        orig_height: usize
    ) -> (usize, usize) {
        match scale {
            PreviewScale::FitWindow => {
                let img_ratio = orig_width as f64 / orig_height as f64;
                let gui_ratio = self.pr_area_width as f64 / self.pr_area_height as f64;
                if img_ratio > gui_ratio {
                    (self.pr_area_width, (self.pr_area_width as f64 / img_ratio) as usize)
                } else {
                    ((self.pr_area_height as f64 * img_ratio) as usize, self.pr_area_height)
                }
            }
            PreviewScale::Original|PreviewScale::P200|PreviewScale::P300|PreviewScale::P400 =>
                (orig_width, orig_height),
            PreviewScale::P75 =>
                (3*orig_width/4, 3*orig_height/4),
            PreviewScale::P50 =>
                (orig_width/2, orig_height/2),
            PreviewScale::P33 =>
                (orig_width/3, orig_height/3),
            PreviewScale::P25 =>
                (orig_width/4, orig_height/4),
            PreviewScale::P15 =>
                (15*orig_width/100, 15*orig_height/100),
            PreviewScale::P10 =>
                (10*orig_width/100, 10*orig_height/100),
            PreviewScale::P5 =>
                (5*orig_width/100, 5*orig_height/100),
            PreviewScale::CenterAndCorners => {
                let min_size = self.pr_area_width.min(self.pr_area_height);
                let min_size_protected = min_size.max(42);
                (min_size_protected, min_size_protected)
            }
        }
    }

    pub fn get_preview_img_size(&self, orig_width: usize, orig_height: usize) -> (usize, usize) {
        self.get_preview_img_size_for_scale(self.scale, orig_width, orig_height)
    }

    pub fn calc_reduct_ratio(&self, img_width: usize, img_height: usize) -> usize {
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
            PreviewScale::P400 => 1,
            PreviewScale::P300 => 1,
            PreviewScale::P200 => 1,
            PreviewScale::Original => 1,
            PreviewScale::P75 => 1,
            PreviewScale::P50 => 2,
            PreviewScale::P33 => 3,
            PreviewScale::P25|PreviewScale::P15|PreviewScale::P10|PreviewScale::P5 => 4,
            PreviewScale::CenterAndCorners => 1,
        }
    }
}

#[derive(Default, Debug, PartialEq, Clone)]
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
    pub sensor_name:    String, // sensor for auto WB
}

/// RGB data of a whole image for the preview: used by `PreviewScale::CenterAndCorners`,
/// the "save preview as JPEG" export and the plate solve preview. Returns `None` for an
/// empty image or an empty histogram. `cache` must live between calls to be of any use
pub fn get_preview_rgb_data(
    image:  &Image,
    hist:   &Histogram,
    params: &PreviewParams,
    stars:  Option<&StarItems>,
    cache:  &mut PreviewCache,
) -> Option<PreviewRgbData> {
    if (hist.l.is_none() && hist.b.is_none()) || image.is_empty() {
        return None;
    }

    let reduct_ratio = params.calc_reduct_ratio(image.width(), image.height());
    log::debug!("preview reduct_ratio = {}", reduct_ratio);

    let (tables, sensor) = prepare_tables(hist, params, image, cache);

    let (mut bytes, width, height) = to_grb_bytes(
        image,
        params,
        tables,
        reduct_ratio
    );

    if params.stars && let Some(stars) = stars
    && params.scale != PreviewScale::CenterAndCorners {
        show_stars(
            stars,
            &mut bytes,
            image.width(),
            image.height(),
            reduct_ratio
        );
    }

    Some(PreviewRgbData {
        width,
        height,
        bytes:          SharedBytes::new(bytes),
        orig_width:     image.width(),
        orig_height:    image.height(),
        is_color_image: image.is_color(),
        sensor_name:    sensor,
    })
}

fn show_stars(
    stars: &StarItems,
    bytes: &mut [u8],
    width: usize,
    height: usize,
    reduct_ratio: usize
) {
    let width = width / reduct_ratio;
    let height = height / reduct_ratio;
    show_part_stars(stars, bytes, width, height, reduct_ratio, 0, 0);
}

// Same as show_stars, but for a part of the reduced image: star points outside
// the part (which starts at part_left/part_top reduced pixels) are skipped
fn show_part_stars(
    stars:        &StarItems,
    bytes:        &mut [u8],
    width:        usize,
    height:       usize,
    reduct_ratio: usize,
    part_left:    usize,
    part_top:     usize
) {
    let good_color = (0, 255, 0);
    let bad_color = (255, 32, 32);
    for star in stars {
        let (r, g, b) = if !star.overexposed { good_color } else { bad_color };
        for (x, y) in &star.points {
            let x = *x as isize / reduct_ratio as isize - part_left as isize;
            let y = *y as isize / reduct_ratio as isize - part_top as isize;
            if x < 0 || y < 0 || x >= width as isize || y >= height as isize { continue; }
            let offset = 3 * (y as usize * width + x as usize);
            bytes[offset] = r;
            bytes[offset+1] = g;
            bytes[offset+2] = b;
        }
    }
}

/// RGB bytes of a reduced-resolution part of an image (see `Image::iter_div*`).
/// `part` is the part image, `src` is the full source image (its color mode and
/// camera are used), `hist` is the FULL image histogram: display levels must not
/// depend on the visible part. `part_origin` is the part's top-left corner in
/// reduced image pixels. `cache` keeps the gamma tables between calls.
pub fn get_preview_part_rgb_bytes(
    part:         &Image,
    src:          &Image,
    hist:         &Histogram,
    params:       &PreviewParams,
    reduct_ratio: usize,
    part_origin:  (usize, usize),
    stars:        Option<&StarItems>,
    cache:        &mut PreviewCache,
) -> Option<Vec<u8>> {
    if part.is_empty() || (hist.l.is_none() && hist.b.is_none()) {
        return None;
    }

    let (tables, _) = prepare_tables(hist, params, src, cache);

    // In single-channel color modes the part holds only the selected layer,
    // so take the size from that layer (Image::width/height fall back l -> r)
    let (width, height) = match (src.is_color(), params.color) {
        (true, PreviewColorMode::Green) => (part.g.width(), part.g.height()),
        (true, PreviewColorMode::Blue)  => (part.b.width(), part.b.height()),
        _                               => (part.width(), part.height()),
    };
    let mut rgb_bytes = if src.is_color() && params.color == PreviewColorMode::Rgb {
        to_grb_bytes_no_reduct_rgb(part, &tables.r, &tables.g, &tables.b, width, height)
    } else if src.is_color() {
        let (layer, table) = match params.color {
            PreviewColorMode::Red   => (&part.r, &tables.r),
            PreviewColorMode::Green => (&part.g, &tables.g),
            PreviewColorMode::Blue  => (&part.b, &tables.b),
            _ => unreachable!(),
        };
        to_grb_bytes_no_reduct_mono(layer, table, width, height)
    } else {
        to_grb_bytes_no_reduct_mono(&part.l, &tables.l, width, height)
    };

    if params.stars && let Some(stars) = stars {
        show_part_stars(stars, &mut rgb_bytes, width, height, reduct_ratio, part_origin.0, part_origin.1);
    }

    Some(rgb_bytes)
}

#[derive(Debug, Default, PartialEq, Clone)]
struct PreviewLevels {
    r: DarkLightLevels,
    g: DarkLightLevels,
    b: DarkLightLevels,
    l: DarkLightLevels,
}

/// Gamma correction tables for the preview rgb conversion.
/// Owned by `PreviewCache`: rebuilt only when the display levels or gamma/wb
/// change, and the old vecs' capacity is reused on rebuild
#[derive(Default)]
struct GammaTables {
    r:      Vec<u8>,
    g:      Vec<u8>,
    b:      Vec<u8>,
    l:      Vec<u8>,
    levels: PreviewLevels,
    gamma:  f64,
    wb:     [f64; 3],
}

impl GammaTables {
    /// Reuses `old` when it was built from the same levels/gamma/wb,
    /// otherwise rebuilds the tables in place (reusing the vecs' capacity)
    fn new_or_keep<'a>(old: &'a mut Option<Self>, levels: &PreviewLevels, gamma: f64, wb: [f64; 3]) -> &'a mut Self {
        let is_new = old.is_none();
        let t = old.get_or_insert_with(Self::default);
        if is_new || t.levels != *levels || t.gamma != gamma || t.wb != wb {
            Self::fill_all_tables(t, levels, gamma, wb);
        }
        t
    }

    /// Everything `fill_table()` gets its values from: the key compared in
    /// `new_or_keep()` must cover all of it, or the cache serves tables built
    /// from other parameters
    fn fill_all_tables(tables: &mut Self, levels: &PreviewLevels, gamma: f64, wb: [f64; 3]) {
        Self::fill_table(&mut tables.r, levels.r.dark, levels.r.light, gamma, wb[0]);
        Self::fill_table(&mut tables.g, levels.g.dark, levels.g.light, gamma, wb[1]);
        Self::fill_table(&mut tables.b, levels.b.dark, levels.b.light, gamma, wb[2]);
        Self::fill_table(&mut tables.l, levels.l.dark, levels.l.light, gamma, 1.0);
        tables.levels = levels.clone();
        tables.gamma = gamma;
        tables.wb = wb;
    }

    fn fill_table(table: &mut Vec<u8>, min_value: f64, max_value: f64, gamma: f64, k: f64) {
        table.clear();
        if min_value == 0.0 && max_value == 0.0 {
            return;
        }
        table.reserve((u16::MAX as usize) + 1);
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
    }
}

/// Reusable state of the preview rgb conversion. Both entry points
/// (`get_preview_rgb_data`, `get_preview_part_rgb_bytes`) take it by `&mut`,
/// so a caller cannot skip the gamma table cache by accident
#[derive(Default)]
pub struct PreviewCache {
    tables: Option<GammaTables>,
}

impl PreviewCache {
    /// Gamma tables for the given levels/gamma/wb, rebuilt only when they changed
    fn tables(&mut self, levels: &PreviewLevels, gamma: f64, wb: [f64; 3]) -> &mut GammaTables {
        GammaTables::new_or_keep(&mut self.tables, levels, gamma, wb)
    }
}

/// What the gamma tables are built from: display levels taken from the FULL image
/// histogram, WB taken from the source camera; both feed the `cache` key
fn prepare_tables<'a>(
    hist:   &Histogram,
    params: &PreviewParams,
    src:    &Image,
    cache:  &'a mut PreviewCache,
) -> (&'a mut GammaTables, String) {
    let levels = calc_levels(hist, params, src.max_value() as f64, src.levels_locked());
    log::debug!("preview levels = {:?}", levels);

    let (wb, sensor) = get_wb_and_sensor(
        &params.wb,
        src.raw_info
            .as_ref()
            .map(|info| info.camera.as_str())
            .unwrap_or_default(),
        src.levels_locked()
    );
    log::debug!("preview wb and sensor = {:?} {}", wb, sensor);

    (cache.tables(&levels, params.gamma, wb), sensor)
}

fn calc_levels(
    hist:          &Histogram,
    params:        &PreviewParams,
    light_max:     f64,
    levels_locked: bool,
) -> PreviewLevels {
    const WB_PERCENTILE:        f64 = 45.0;
    const DARK_MIN_PERCENTILE:  f64 = 1.0;
    const DARK_MAX_PERCENTILE:  f64 = 60.0;
    const LIGHT_MIN_PERCENTILE: f64 = 95.0;

    // Display-referred images need no stretching: use the full range
    if levels_locked {
        let full = DarkLightLevels { dark: 0.0, light: (u16::MAX - 1) as f64 };
        return PreviewLevels {
            r: full.clone(),
            g: full.clone(),
            b: full.clone(),
            l: full,
        };
    }

    let light_lvl = params.light_lvl.powf(0.05);

    // An empty channel carries no information: fall back to the neutral defaults
    let l_levels = if let Some(hist) = hist.l.as_ref().filter(|h| !h.is_empty()) {
        let dark_min = hist.get_percentile(DARK_MIN_PERCENTILE).unwrap_or(0) as f64;
        let dark_max = hist.get_percentile(DARK_MAX_PERCENTILE).unwrap_or(0) as f64;
        let light_min = hist.get_percentile(LIGHT_MIN_PERCENTILE).unwrap_or(0) as f64;
        let mut dark = linear_interpolate(params.dark_lvl, 1.0, 0.0, dark_min, dark_max);
        let mut light = linear_interpolate(light_lvl, 1.0, 0.0, light_min, light_max);
        dark = f64::clamp(dark, 0.0, u16::MAX as f64);
        light = f64::clamp(light, 0.0, u16::MAX as f64);
        if (light - dark) < 2.0 { light += 1.0; dark -= 1.0; }
        DarkLightLevels { dark, light }
    } else {
        DarkLightLevels::default()
    };

    let (g_levels, g_wb) = if let Some(hist) = hist.g.as_ref().filter(|h| !h.is_empty()) {
        let dark_min = hist.get_percentile(DARK_MIN_PERCENTILE).unwrap_or(0) as f64;
        let dark_max = hist.get_percentile(DARK_MAX_PERCENTILE).unwrap_or(0) as f64;
        let light_min = hist.get_percentile(LIGHT_MIN_PERCENTILE).unwrap_or(0) as f64;
        let mut dark = linear_interpolate(params.dark_lvl, 1.0, 0.0, dark_min, dark_max);
        let mut light = linear_interpolate(light_lvl, 1.0, 0.0, light_min, light_max);
        dark = f64::clamp(dark, 0.0, u16::MAX as f64);
        light = f64::clamp(light, 0.0, u16::MAX as f64);
        if (light - dark) < 2.0 { light += 1.0; dark -= 1.0; }
        let wb = hist.get_percentile(WB_PERCENTILE).unwrap_or(0) as f64;
        (DarkLightLevels { dark, light }, wb)
    } else {
        (DarkLightLevels::default(), 0.0)
    };

    let g_range = g_levels.light - g_levels.dark;

    let r_levels = if let Some(hist) = hist.r.as_ref().filter(|h| !h.is_empty()) {
        let wb = hist.get_percentile(WB_PERCENTILE).unwrap_or(0) as f64;
        let dark = g_levels.dark + (wb - g_wb);
        DarkLightLevels { dark, light: dark + g_range }
    } else {
        // Without its own WB the channel is indistinguishable from G
        g_levels.clone()
    };

    let b_levels = if let Some(hist) = hist.b.as_ref().filter(|h| !h.is_empty()) {
        let wb = hist.get_percentile(WB_PERCENTILE).unwrap_or(0) as f64;
        let dark = g_levels.dark + (wb - g_wb);
        DarkLightLevels { dark, light: dark + g_range }
    } else {
        g_levels.clone()
    };

    PreviewLevels {
        r: r_levels,
        g: g_levels,
        b: b_levels,
        l: l_levels,
    }
}

fn get_wb_and_sensor(
    wb:            &Option<[f64; 3]>,
    camera:        &str,
    levels_locked: bool,
) -> ([f64; 3], String) {
    let cam_info = get_cam_info(camera);
    let sensor = cam_info.as_ref()
        .map(|cam_info| cam_info.sensor.to_string())
        .unwrap_or_default();
    // Display-referred images already have their colors set: keep WB neutral
    if levels_locked {
        return ([1.0, 1.0, 1.0], sensor);
    }
    let auto_wb_coeffs = cam_info.as_ref().map(|cam_info| cam_info.wb).unwrap_or([1.0, 1.0, 1.0]);
    let mut r_wb = wb.map(|wb| wb[0]).unwrap_or(auto_wb_coeffs[0] as f64);
    let mut g_wb = wb.map(|wb| wb[1]).unwrap_or(auto_wb_coeffs[1] as f64);
    let mut b_wb = wb.map(|wb| wb[2]).unwrap_or(auto_wb_coeffs[2] as f64);
    let mut min_wb = r_wb.min(g_wb).min(b_wb);
    if min_wb <= 0.0 { min_wb = 1.0; }
    r_wb /= min_wb;
    g_wb /= min_wb;
    b_wb /= min_wb;
    ([r_wb, g_wb, b_wb], sensor)
}

fn to_grb_bytes(
    image:        &Image,
    params:       &PreviewParams,
    tables:       &GammaTables,
    reduct_ratio: usize,
) -> (Vec<u8>, usize, usize) {
    let (result_width, result_height) = params.get_preview_img_size(image.width(), image.height());
    let (cell_width, cell_height) = corner_rect_size(result_width, result_height);
    // Below `GRID` cells per axis the cells overlap in the source, so the frame is drawn whole
    let use_corners = params.scale == PreviewScale::CenterAndCorners
        && image.width()  >= GRID * cell_width
        && image.height() >= GRID * cell_height;

    let rgb_bytes = if image.is_color() && params.color == PreviewColorMode::Rgb {
        match (use_corners, reduct_ratio) {
            (true, _) => to_grb_bytes_corners_rgb(image, &tables.r, &tables.g, &tables.b, result_width, result_height),
            (_, 1) => to_grb_bytes_no_reduct_rgb(image, &tables.r, &tables.g, &tables.b, image.width(), image.height()),
            (_, 2) => to_grb_bytes_reduct2_rgb  (image, &tables.r, &tables.g, &tables.b),
            (_, 3) => to_grb_bytes_reduct3_rgb  (image, &tables.r, &tables.g, &tables.b),
            (_, 4) => to_grb_bytes_reduct4_rgb  (image, &tables.r, &tables.g, &tables.b),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    } else {
        let (layer, table) = match (image.is_color(), params.color) {
            (false, _)                   => (&image.l, &tables.l),
            (_, PreviewColorMode::Red)   => (&image.r, &tables.r),
            (_, PreviewColorMode::Green) => (&image.g, &tables.g),
            (_, PreviewColorMode::Blue)  => (&image.b, &tables.b),
            _ => unreachable!(),
        };
        match (use_corners, reduct_ratio) {
            (true, _) => to_grb_bytes_corners_mono(layer, table, result_width, result_height),
            (_, 1) => to_grb_bytes_no_reduct_mono(layer, table, image.width(), image.height()),
            (_, 2) => to_grb_bytes_reduct2_mono  (layer, table),
            (_, 3) => to_grb_bytes_reduct3_mono  (layer, table),
            (_, 4) => to_grb_bytes_reduct4_mono  (layer, table),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    };

    let (width, height) = if use_corners {
        (result_width, result_height)
    } else {
        (image.width()/reduct_ratio, image.height()/reduct_ratio)
    };

    (rgb_bytes, width, height)
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

// Cells per preview side in the CenterAndCorners mode
const GRID: usize = 3;

/// Corner/center cell size for a `width` x `height` preview. It is derived from the widget
/// size only, so it can exceed a small source image (see `to_grb_bytes`)
fn corner_rect_size(width: usize, height: usize) -> (usize, usize) {
    const SPACING: usize = 6;
    (
        usize::saturating_sub(width, 2 * SPACING) / GRID,
        usize::saturating_sub(height, 2 * SPACING) / GRID
    )
}

fn get_rects_for_corners_and_center(
    src_width: usize,
    src_height: usize,
    width:  usize,
    height: usize
) -> Vec<CopyRectParams> {
    fn pos(img_size: usize, rect_size: usize, pos: usize) -> usize {
        match pos {
            0 => 0,
            1 => usize::saturating_sub(img_size, rect_size) / 2,
            2 => usize::saturating_sub(img_size, rect_size),
            _ => unreachable!(),
        }
    }
    let mut result = Vec::new();
    let (rect_width, rect_height) = corner_rect_size(width, height);
    // Defensive: the callers guarantee the cells fit in the source
    let rect_width = usize::min(rect_width, src_width);
    let rect_height = usize::min(rect_height, src_height);
    for i in 0..GRID {
        for j in 0..GRID {
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
    let mut rgb_bytes = vec![0; 3 * width * height];
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
    let mut rgb_bytes = vec![0; 3 * width * height];
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
) -> Vec<u8> {
    let iter = image.iter_div2_rgb(None);
    let mut rgb_bytes = Vec::with_capacity(3 * iter.size_hint().0);
    for (r, g, b) in iter {
        rgb_bytes.push(r_table[r as usize]);
        rgb_bytes.push(g_table[g as usize]);
        rgb_bytes.push(b_table[b as usize]);
    }
    rgb_bytes
}

fn to_grb_bytes_reduct2_mono(
    layer: &ImageLayer<u16>,
    table: &[u8],
) -> Vec<u8> {
    let iter = layer.iter_div2(None);
    let mut rgb_bytes = Vec::with_capacity(3 * iter.size_hint().0);
    for l in iter {
        let l = table[l as usize];
        rgb_bytes.push(l);
        rgb_bytes.push(l);
        rgb_bytes.push(l);
    }
    rgb_bytes
}

fn to_grb_bytes_reduct3_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
) -> Vec<u8> {
    let iter = image.iter_div3_rgb(None);
    let mut rgb_bytes = Vec::with_capacity(3 * iter.size_hint().0);
    for (r, g, b) in iter {
        rgb_bytes.push(r_table[r as usize]);
        rgb_bytes.push(g_table[g as usize]);
        rgb_bytes.push(b_table[b as usize]);
    }
    rgb_bytes
}

fn to_grb_bytes_reduct3_mono(
    layer: &ImageLayer<u16>,
    table: &[u8],
) -> Vec<u8> {
    let iter = layer.iter_div3(None);
    let mut rgb_bytes = Vec::with_capacity(3 * iter.size_hint().0);
    for l in iter {
        let l = table[l as usize];
        rgb_bytes.push(l);
        rgb_bytes.push(l);
        rgb_bytes.push(l);
    }
    rgb_bytes
}

fn to_grb_bytes_reduct4_rgb(
    image:   &Image,
    r_table: &[u8],
    g_table: &[u8],
    b_table: &[u8],
) -> Vec<u8> {
    let iter = image.iter_div4_rgb(None);
    let mut rgb_bytes = Vec::with_capacity(3 * iter.size_hint().0);
    for (r, g, b) in iter {
        rgb_bytes.push(r_table[r as usize]);
        rgb_bytes.push(g_table[g as usize]);
        rgb_bytes.push(b_table[b as usize]);
    }
    rgb_bytes
}

fn to_grb_bytes_reduct4_mono(
    layer: &ImageLayer<u16>,
    table: &[u8],
) -> Vec<u8> {
    let iter = layer.iter_div4(None);
    let mut rgb_bytes = Vec::with_capacity(3 * iter.size_hint().0);
    for l in iter {
        let l = table[l as usize];
        rgb_bytes.push(l);
        rgb_bytes.push(l);
        rgb_bytes.push(l);
    }
    rgb_bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::histogram::HistogramChan;

    fn chan(freq: &[u32]) -> Option<HistogramChan> {
        Some(HistogramChan {
            mean: 0.0,
            std_dev: 0.0,
            count: freq.iter().map(|&v| v as usize).sum(),
            freq: freq.to_vec(),
        })
    }

    // `take_from_freq` truncation of a whole channel: count == 0 while the channel is `Some`
    fn empty_chan() -> Option<HistogramChan> {
        chan(&[0; 8])
    }

    #[test]
    fn calc_levels_of_empty_channels_is_neutral() {
        let hist = Histogram {
            max: 7,
            r: empty_chan(),
            g: empty_chan(),
            b: empty_chan(),
            l: empty_chan(),
        };
        let levels = calc_levels(&hist, &PreviewParams::default(), 7.0, false);
        for lv in [&levels.l, &levels.g, &levels.r, &levels.b] {
            assert_eq!(*lv, DarkLightLevels::default());
        }
    }

    #[test]
    fn calc_levels_with_empty_r_keeps_g_levels() {
        // The driver's max_value truncated the whole R channel while G kept data
        let hist = Histogram {
            max: 7,
            r: empty_chan(),
            g: chan(&[0, 10, 10, 10, 10, 10, 10, 10]),
            b: None,
            l: chan(&[0, 10, 10, 10, 10, 10, 10, 10]),
        };
        let levels = calc_levels(&hist, &PreviewParams::default(), 7.0, false);
        assert_eq!(levels.r, levels.g);
        assert!(levels.r.dark >= 0.0 && levels.r.light <= 7.0 + 2.0);
    }

    #[test]
    fn calc_levels_locked_uses_full_range() {
        let hist = Histogram::new();
        let levels = calc_levels(&hist, &PreviewParams::default(), 4095.0, true);
        assert_eq!(levels.l, DarkLightLevels { dark: 0.0, light: (u16::MAX - 1) as f64 });
    }
}
