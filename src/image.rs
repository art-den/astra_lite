use std::{path::Path, io::{BufWriter, BufReader}, fs::File};

use itertools::*;
use rayon::prelude::*;

use crate::{math::*, image_info::Histogram, options::PreviewColor};

pub struct ImageLayer<T> {
    data: Vec<T>,
    width: usize,
    height: usize,
    width_1: i64,
    height_1: i64,
}

pub const CRD_DIV: i64 = 256;

impl<T: Copy + Default> ImageLayer<T> {
    pub fn new_empty() -> Self {
        Self { data: Vec::new(), width: 0, height: 0, width_1: 0, height_1: 0 }
    }

    pub fn new_with_size(width: usize, height: usize) -> Self {
        let mut data = Vec::new();
        data.resize(width * height, T::default());
        Self { data, width, height, width_1: width as i64 - 1, height_1: height as i64 - 1 }
    }

    fn clear(&mut self) {
        self.data.clear();
        self.data.shrink_to_fit();
        self.width = 0;
        self.height = 0;
        self.width_1 = 0;
        self.height_1 = 0;
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.data.resize(width * height, T::default());
        self.width = width;
        self.height = height;
        self.width_1 = width as i64 - 1;
        self.height_1 = height as i64 - 1;
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 && self.height == 0
    }

    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    pub fn as_slice_mut(&mut self) -> &mut [T] {
        &mut self.data
    }

    pub fn row(&self, y: usize) -> &[T] {
        let pos = y * self.width;
        &self.data[pos..pos+self.width]
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [T] {
        let pos = y * self.width;
        &mut self.data[pos..pos+self.width]
    }

    pub fn set(&mut self, x: isize, y: isize, value: T) {
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
            panic!("Wrong coordinates: x={}, y={}", x, y);
        }
        self.data[(y as usize) * self.width + (x as usize)] = value;
    }

    pub fn get(&self, x: isize, y: isize) -> Option<T> {
        if x < 0 || y < 0 || x >= self.width as isize {
            None
        } else {
            self.data.get(x as usize + y as usize * self.width).copied()
        }
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn rect_iter(&self, mut x1: isize, mut y1: isize, mut x2: isize, mut y2: isize) -> RectIterator<T> {
        if x1 < 0 { x1 = 0; }
        if y1 < 0 { y1 = 0; }
        if x2 >= self.width as isize { x2 = self.width as isize - 1; }
        if y2 >= self.height as isize { y2 = self.height as isize - 1; }
        RectIterator::<T> {
            x1: x1 as usize,
            x2: x2 as usize,
            y: y1 as usize,
            y2: y2 as usize,
            img: self,
            iter: RectIterator::init_iter(self, x1 as usize, x2 as usize, y1 as usize)
        }
    }
}

impl ImageLayer<u16> {
    pub fn calc_noise(&self) -> f32 {
        let mut diffs = Vec::with_capacity(self.data.len()/10);
        for (v1, v2, v3, v4, v5, m, v6, v7, v8, v9, v10)
        in self.data.iter().tuples().step_by(7) {
            let aver = (
                *v1 as u32 + *v2 as u32 +
                *v3 as u32 + *v4 as u32 +
                *v5 as u32 + *v6 as u32 +
                *v7 as u32 + *v8 as u32 +
                *v9 as u32 + *v10 as u32 + 5
            ) / 10;
            let m = *m as u32;
            let diff = if m > aver { m - aver } else { aver - m };
            diffs.push(diff as u16);
        }
        let max_pos = 80 * diffs.len() / 100; // 80%
        diffs.select_nth_unstable(max_pos);
        let mut sum = 0_u64;
        for v in &diffs[..max_pos] {
            let v = *v as u64;
            sum += v * v;
        }
        f64::sqrt(sum as f64 / max_pos as f64) as f32
    }

    pub fn calc_background(&self, mt: bool) -> u16 {
        let sum: u64 = if mt {
            self.data.iter().map(|v| *v as u64).sum()
        } else {
            self.data.par_iter().map(|v| *v as u64).sum()
        };
        (sum / self.data.len() as u64) as u16
    }

    pub fn get_idiv16_crd(&self, x: i64, y: i64) -> Option<u16> {
        let x_i = x / CRD_DIV;
        let y_i = y / CRD_DIV;
        let x_p1 = x as usize % CRD_DIV as usize;
        let x_p0 = CRD_DIV as usize - x_p1;
        let y_p1 = y as usize % CRD_DIV as usize;
        let y_p0 = CRD_DIV as usize - y_p1;
        let v = if x_i >= 0 && y_i >= 0 && x_i < self.width_1 && y_i < self.height_1 {
            let pos = x_i as usize + y_i as usize * self.width;
            let v00 = unsafe { *self.data.get_unchecked(pos) };
            let v10 = unsafe { *self.data.get_unchecked(pos+1) };
            let v01 = unsafe { *self.data.get_unchecked(pos + self.width) };
            let v11 = unsafe { *self.data.get_unchecked(pos + self.width+1) };
            let v0 = (v00 as usize * x_p0) + (v10 as usize * x_p1);
            let v1 = (v01 as usize * x_p0) + (v11 as usize * x_p1);
            v0 * y_p0 + v1 * y_p1
        } else {
            let v00 = self.get(x_i as isize, y_i as isize);
            let v10 = self.get(x_i as isize+1, y_i as isize);
            let v01 = self.get(x_i as isize, y_i as isize+1);
            let v11 = self.get(x_i as isize+1, y_i as isize+1);
            let v0 = match (v00, v10) {
                (Some(v00), Some(v10)) => Some((v00 as usize * x_p0) + (v10 as usize * x_p1)),
                (Some(v00), None)      => Some((v00 as usize) * CRD_DIV as usize),
                (None, Some(v10))      => Some((v10 as usize) * CRD_DIV as usize),
                _                      => None,
            };
            let v1 = match (v01, v11) {
                (Some(v01), Some(v11)) => Some((v01 as usize * x_p0) + (v11 as usize * x_p1)),
                (Some(v01), None)      => Some((v01 as usize) * CRD_DIV as usize),
                (None, Some(v11))      => Some((v11 as usize) * CRD_DIV as usize),
                _                      => None,
            };
            match (v0, v1) {
                (Some(v0), Some(v1)) => v0 * y_p0 + v1 * y_p1,
                (Some(v0), None)     => v0 * CRD_DIV as usize,
                (None, Some(v1))     => v1 * CRD_DIV as usize,
                _                    => return None,
            }
        };
        let mut result = v / (CRD_DIV as usize * CRD_DIV as usize);
        if result > u16::MAX as usize { result = u16::MAX as usize; }
        Some(result as u16)
    }

    pub fn get_f64_crd(&self, x: f64, y: f64) -> Option<u16> {
        self.get_idiv16_crd(
            (x * CRD_DIV as f64) as i64,
            (y * CRD_DIV as f64) as i64
        )
    }

    pub fn remove_gradient(&mut self) {
        if self.is_empty() { return; }
        let Some(gradient) = calc_gradient(self) else { return; };

        let v00 = gradient.calc_z(0.0, 0.0);
        let v10 = gradient.calc_z(self.width as f64, 0.0);
        let v01 = gradient.calc_z(0.0, self.height as f64);
        let v11 = gradient.calc_z(self.width as f64, self.height as f64);
        let min = [v00, v10, v01, v11].into_iter().min_by(cmp_f64).unwrap_or_default();
        let max = [v00, v10, v01, v11].into_iter().max_by(cmp_f64).unwrap_or_default();
        if max - min < 5.0 { return; } // do not remove gradient if difference in corners is small
        self.data
            .par_chunks_exact_mut(self.width)
            .enumerate()
            .for_each(|(y, row)| {
                let Some(line) = gradient.intersect_by_xz_plane(y as f64) else { return; };
                let z1 = line.get(0.0).round() as i32;
                let z2 = line.get(self.width as f64).round() as i32;
                let z_diff = i32::abs(z1-z2);
                if z_diff < self.width as i32 {
                    let height = z_diff as usize + 1;
                    let mut sum = self.width/2;
                    let mut z = z1;
                    let dz = if z1 < z2 {1} else {-1};
                    for value in row {
                        let mut v = *value as i32;
                        v -= z;
                        if v < 0 { v = 0; }
                        else if v > u16::MAX as i32 { v = u16::MAX as i32; }
                        *value = v as u16;
                        // simple Bresenham's algorithm
                        sum += height;
                        if sum >= self.width {
                            sum -= self.width;
                            z += dz;
                        }
                    }
                } else {
                    for (x, value) in row.iter_mut().enumerate() {
                        let mut v = *value as f64;
                        v -= line.get(x as f64);
                        *value = v as u16;
                    }
                }
            });
    }
}

impl GradientCalcSource for ImageLayer<u16> {
    fn image_width(&self) -> usize {
        self.width
    }

    fn image_height(&self) -> usize {
        self.height
    }

    fn get_rect_values(&self, x1: usize, y1: usize, x2: usize, y2: usize, result: &mut Vec<u16>) {
        result.clear();
        for y in y1..=y2 {
            let row = self.row(y);
            result.extend_from_slice(&row[x1..=x2]);
        }
    }
}

pub struct RectIterator<'a, T> {
    x1: usize,
    x2: usize,
    y: usize,
    y2: usize,
    iter: std::slice::Iter<'a, T>,
    img: &'a ImageLayer<T>,
}

impl<'a, T: Copy + Default> RectIterator<'a, T> {
    fn init_iter(img: &ImageLayer<T>, x1: usize, x2: usize, y: usize) -> std::slice::Iter<T> {
        let row = img.row(y);
        row[x1 ..= x2].iter()
    }
}

impl<'a, T: Copy + Default> Iterator for RectIterator<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.iter.next();
        if next.is_some() {
            next.copied()
        } else {
            self.y += 1;
            if self.y > self.y2 {
                return None;
            }
            self.iter = Self::init_iter(self.img, self.x1, self.x2, self.y);
            self.next()
        }
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct Image {
    pub r: ImageLayer<u16>,
    pub g: ImageLayer<u16>,
    pub b: ImageLayer<u16>,
    pub l: ImageLayer<u16>,
    width: usize,
    height: usize,
    zero: i32,
    max_value: u16,
}

impl Image {
    pub fn new_empty() -> Self {
        Self {
            l: ImageLayer::new_empty(),
            r: ImageLayer::new_empty(),
            g: ImageLayer::new_empty(),
            b: ImageLayer::new_empty(),
            width: 0,
            height: 0,
            zero: 0,
            max_value: 0,
        }
    }

    pub fn make_color(
        &mut self,
        width:     usize,
        height:    usize,
        zero:      i32,
        max_value: u16
    ) {
        self.l.clear();
        self.r.resize(width, height);
        self.g.resize(width, height);
        self.b.resize(width, height);
        self.width = width;
        self.height = height;
        self.zero = zero;
        self.max_value = max_value;
    }

    pub fn make_monochrome(
        &mut self,
        width:     usize,
        height:    usize,
        zero:      i32,
        max_value: u16
    ) {
        self.l.resize(width, height);
        self.r.clear();
        self.g.clear();
        self.b.clear();
        self.width = width;
        self.height = height;
        self.zero = zero;
        self.max_value = max_value;
    }

    pub fn clear(&mut self) {
        self.l.clear();
        self.r.clear();
        self.g.clear();
        self.b.clear();
        self.width = 0;
        self.height = 0;
        self.zero = 0;
        self.max_value = 0;
    }

    pub fn zero(&self) -> i32 {
        self.zero
    }

    pub fn is_empty(&self) -> bool {
        self.l.is_empty() &&
        self.r.is_empty() &&
        self.g.is_empty() &&
        self.b.is_empty()
    }

    pub fn is_color(&self) -> bool {
        self.l.is_empty() &&
        !self.r.is_empty() &&
        !self.g.is_empty() &&
        !self.b.is_empty()
    }

    pub fn is_monochrome(&self) -> bool {
        !self.l.is_empty() &&
        self.r.is_empty() &&
        self.g.is_empty() &&
        self.b.is_empty()
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn max_value(&self) -> u16 {
        self.max_value
    }

    pub fn to_grb_bytes(
        &self,
        l_levels:     &DarkLightLevels,
        r_levels:     &DarkLightLevels,
        g_levels:     &DarkLightLevels,
        b_levels:     &DarkLightLevels,
        gamma:        f64,
        reduct_ratio: usize,
        color:        PreviewColor,
    ) -> RgbU8Data {
        if self.is_empty() {
            return RgbU8Data::default();
        }
        let args = ImageToU8BytesArgs {
            width:          self.width,
            height:         self.height,
            is_color_image: self.is_color(),
        };
        let r_table = Self::create_gamma_table(r_levels.dark, r_levels.light, gamma);
        let g_table = Self::create_gamma_table(g_levels.dark, g_levels.light, gamma);
        let b_table = Self::create_gamma_table(b_levels.dark, b_levels.light, gamma);
        let l_table = Self::create_gamma_table(l_levels.dark, l_levels.light, gamma);
        match reduct_ratio {
            1 => self.to_grb_bytes_no_reduct(&r_table, &g_table, &b_table, &l_table, args, color),
            2 => self.to_grb_bytes_reduct2  (&r_table, &g_table, &b_table, &l_table, args, color),
            3 => self.to_grb_bytes_reduct3  (&r_table, &g_table, &b_table, &l_table, args, color),
            4 => self.to_grb_bytes_reduct4  (&r_table, &g_table, &b_table, &l_table, args, color),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
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

    fn to_grb_bytes_no_reduct(
        &self,
        r_table: &[u8],
        g_table: &[u8],
        b_table: &[u8],
        l_table: &[u8],
        args:    ImageToU8BytesArgs,
        color:   PreviewColor,
    ) -> RgbU8Data {
        let mut rgb_bytes = Vec::with_capacity(3 * args.width * args.height);
        let is_color_image = args.is_color_image && color == PreviewColor::Rgb;
        if is_color_image {
            for row in 0..args.height {
                let r_iter = self.r.row(row).iter();
                let g_iter = self.g.row(row).iter();
                let b_iter = self.b.row(row).iter();
                for (r, g, b) in
                izip!(r_iter, g_iter, b_iter) {
                    rgb_bytes.push(r_table[*r as usize]);
                    rgb_bytes.push(g_table[*g as usize]);
                    rgb_bytes.push(b_table[*b as usize]);
                }
            }
        } else {
            let (m_data, table) = match (args.is_color_image, color) {
                (false, _)               => (&self.l, l_table),
                (_, PreviewColor::Red)   => (&self.r, r_table),
                (_, PreviewColor::Green) => (&self.g, g_table),
                (_, PreviewColor::Blue)  => (&self.b, b_table),
                _ => unreachable!(),
            };
            for row in 0..args.height {
                for l in m_data.row(row).iter() {
                    let l = table[*l as usize];
                    rgb_bytes.push(l);
                    rgb_bytes.push(l);
                    rgb_bytes.push(l);
                }
            }
        }
        RgbU8Data {
            width: args.width,
            height: args.height,
            bytes: rgb_bytes,
            orig_width: self.width,
            orig_height: self.height,
            is_color_image: args.is_color_image,
        }
    }

    fn to_grb_bytes_reduct2(
        &self,
        r_table: &[u8],
        g_table: &[u8],
        b_table: &[u8],
        l_table: &[u8],
        args:    ImageToU8BytesArgs,
        color:   PreviewColor,
    ) -> RgbU8Data {
        let width = args.width / 2;
        let height = args.height / 2;
        let mut bytes = Vec::with_capacity(3 * width * height);
        let is_color_image = args.is_color_image && color == PreviewColor::Rgb;
        if is_color_image {
            for y in 0..height {
                let mut r0 = self.r.row(2*y).as_ptr();
                let mut r1 = self.r.row(2*y+1).as_ptr();
                let mut g0 = self.g.row(2*y).as_ptr();
                let mut g1 = self.g.row(2*y+1).as_ptr();
                let mut b0 = self.b.row(2*y).as_ptr();
                let mut b1 = self.b.row(2*y+1).as_ptr();
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
                    bytes.push(r_table[r as usize]);
                    bytes.push(g_table[g as usize]);
                    bytes.push(b_table[b as usize]);
                    r0 = r0.wrapping_offset(2);
                    r1 = r1.wrapping_offset(2);
                    g0 = g0.wrapping_offset(2);
                    g1 = g1.wrapping_offset(2);
                    b0 = b0.wrapping_offset(2);
                    b1 = b1.wrapping_offset(2);
                }
            }
        } else {
            let (m_data, table) = match (args.is_color_image, color) {
                (false, _)               => (&self.l, l_table),
                (_, PreviewColor::Red)   => (&self.r, r_table),
                (_, PreviewColor::Green) => (&self.g, g_table),
                (_, PreviewColor::Blue)  => (&self.b, b_table),
                _ => unreachable!(),
            };
            for y in 0..height {
                let mut l0 = m_data.row(2*y).as_ptr();
                let mut l1 = m_data.row(2*y+1).as_ptr();
                for _ in 0..width {
                    let l = unsafe {(
                        *l0 as u32 + *l0.offset(1) as u32 +
                        *l1 as u32 + *l1.offset(1) as u32 + 2
                    ) / 4};
                    let l = table[l as usize];
                    bytes.push(l);
                    bytes.push(l);
                    bytes.push(l);
                    l0 = l0.wrapping_offset(2);
                    l1 = l1.wrapping_offset(2);
                }
            }
        }
        RgbU8Data {
            width, height, bytes,
            orig_width: self.width,
            orig_height: self.height,
            is_color_image: args.is_color_image,
        }
    }

    fn to_grb_bytes_reduct3(
        &self,
        r_table: &[u8],
        g_table: &[u8],
        b_table: &[u8],
        l_table: &[u8],
        args:    ImageToU8BytesArgs,
        color:   PreviewColor,
    ) -> RgbU8Data {
        let width = args.width / 3;
        let height = args.height / 3;
        let mut bytes = Vec::with_capacity(3 * width * height);
        let is_color_image = args.is_color_image && color == PreviewColor::Rgb;
        if is_color_image {
            for y in 0..height {
                let mut r0 = self.r.row(3*y).as_ptr();
                let mut r1 = self.r.row(3*y+1).as_ptr();
                let mut r2 = self.r.row(3*y+2).as_ptr();
                let mut g0 = self.g.row(3*y).as_ptr();
                let mut g1 = self.g.row(3*y+1).as_ptr();
                let mut g2 = self.g.row(3*y+2).as_ptr();
                let mut b0 = self.b.row(3*y).as_ptr();
                let mut b1 = self.b.row(3*y+1).as_ptr();
                let mut b2 = self.b.row(3*y+2).as_ptr();
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
                    bytes.push(r_table[r as usize]);
                    bytes.push(g_table[g as usize]);
                    bytes.push(b_table[b as usize]);
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
        } else {
            let (m_data, table) = match (args.is_color_image, color) {
                (false, _)               => (&self.l, l_table),
                (_, PreviewColor::Red)   => (&self.r, r_table),
                (_, PreviewColor::Green) => (&self.g, g_table),
                (_, PreviewColor::Blue)  => (&self.b, b_table),
                _ => unreachable!(),
            };
            for y in 0..height {
                let mut l0 = m_data.row(3*y).as_ptr();
                let mut l1 = m_data.row(3*y+1).as_ptr();
                let mut l2 = m_data.row(3*y+2).as_ptr();
                for _ in 0..width {
                    let l = unsafe {(
                        *l0 as u32 + *l0.offset(1) as u32 + *l0.offset(2) as u32 +
                        *l1 as u32 + *l1.offset(1) as u32 + *l1.offset(2) as u32 +
                        *l2 as u32 + *l2.offset(1) as u32 + *l2.offset(2) as u32 + 4
                    ) / 9};
                    let l = table[l as usize];
                    bytes.push(l);
                    bytes.push(l);
                    bytes.push(l);
                    l0 = l0.wrapping_offset(3);
                    l1 = l1.wrapping_offset(3);
                    l2 = l2.wrapping_offset(3);
                }
            }
        }
        RgbU8Data {
            width, height, bytes,
            orig_width: self.width,
            orig_height: self.height,
            is_color_image: args.is_color_image,
        }
    }

    fn to_grb_bytes_reduct4(
        &self,
        r_table: &[u8],
        g_table: &[u8],
        b_table: &[u8],
        l_table: &[u8],
        args:    ImageToU8BytesArgs,
        color:   PreviewColor,
    ) -> RgbU8Data {
        let width = args.width / 4;
        let height = args.height / 4;
        let mut bytes = Vec::with_capacity(3 * width * height);
        let is_color_image = args.is_color_image && color == PreviewColor::Rgb;
        if is_color_image {
            for y in 0..height {
                let mut r0 = self.r.row(4*y).as_ptr();
                let mut r1 = self.r.row(4*y+1).as_ptr();
                let mut r2 = self.r.row(4*y+2).as_ptr();
                let mut r3 = self.r.row(4*y+3).as_ptr();
                let mut g0 = self.g.row(4*y).as_ptr();
                let mut g1 = self.g.row(4*y+1).as_ptr();
                let mut g2 = self.g.row(4*y+2).as_ptr();
                let mut g3 = self.g.row(4*y+3).as_ptr();
                let mut b0 = self.b.row(4*y).as_ptr();
                let mut b1 = self.b.row(4*y+1).as_ptr();
                let mut b2 = self.b.row(4*y+2).as_ptr();
                let mut b3 = self.b.row(4*y+3).as_ptr();
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
                    bytes.push(r_table[r as usize]);
                    bytes.push(g_table[g as usize]);
                    bytes.push(b_table[b as usize]);
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
        } else {
            let (m_data, table) = match (args.is_color_image, color) {
                (false, _)               => (&self.l, l_table),
                (_, PreviewColor::Red)   => (&self.r, r_table),
                (_, PreviewColor::Green) => (&self.g, g_table),
                (_, PreviewColor::Blue)  => (&self.b, b_table),
                _ => unreachable!(),
            };
            for y in 0..height {
                let mut l0 = m_data.row(4*y).as_ptr();
                let mut l1 = m_data.row(4*y+1).as_ptr();
                let mut l2 = m_data.row(4*y+2).as_ptr();
                let mut l3 = m_data.row(4*y+3).as_ptr();
                for _ in 0..width {
                    let l = unsafe {(
                        *l0 as u32 + *l0.offset(1) as u32 + *l0.offset(2) as u32 + *l0.offset(3) as u32 +
                        *l1 as u32 + *l1.offset(1) as u32 + *l1.offset(2) as u32 + *l1.offset(3) as u32 +
                        *l2 as u32 + *l2.offset(1) as u32 + *l2.offset(2) as u32 + *l2.offset(3) as u32 +
                        *l3 as u32 + *l3.offset(1) as u32 + *l3.offset(2) as u32 + *l3.offset(3) as u32 + 8
                    ) / 16};
                    let l = table[l as usize];
                    bytes.push(l);
                    bytes.push(l);
                    bytes.push(l);
                    l0 = l0.wrapping_offset(4);
                    l1 = l1.wrapping_offset(4);
                    l2 = l2.wrapping_offset(4);
                    l3 = l3.wrapping_offset(4);
                }
            }
        }
        RgbU8Data {
            width, height, bytes,
            orig_width: self.width,
            orig_height: self.height,
            is_color_image: args.is_color_image,
        }
    }

    pub fn remove_gradient(&mut self) {
        self.l.remove_gradient();
        self.r.remove_gradient();
        self.g.remove_gradient();
        self.b.remove_gradient();
    }

    pub fn load_from_file(&mut self, file_name: &Path) -> anyhow::Result<()> {
        let ext = file_name.extension().unwrap_or_default().to_str().unwrap_or_default();
        if ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff") {
            self.load_from_tiff_file(file_name)
        } else {
            anyhow::bail!("Format is not supported")
        }
    }

    pub fn save_to_tiff(&self, file_name: &Path) -> anyhow::Result<()> {
        use tiff::encoder::*;

        let mut file = BufWriter::new(File::create(file_name)?);
        let mut decoder = TiffEncoder::new(&mut file)?;
        if self.is_monochrome() {
            let tiff = decoder.new_image::<colortype::Gray16>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.write_data(self.l.as_slice())?;
        }
        else if self.is_color() {
            let mut tiff = decoder.new_image::<colortype::RGB16>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.rows_per_strip(64)?;
            let mut strip_data = Vec::new();
            let mut pos = 0_usize;
            loop {
                let mut samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }
                samples_count /= 3;
                strip_data.clear();
                let r_strip = &self.r.data[pos..pos+samples_count];
                let g_strip = &self.g.data[pos..pos+samples_count];
                let b_strip = &self.b.data[pos..pos+samples_count];
                for (r, g, b) in izip!(r_strip, g_strip, b_strip) {
                    strip_data.push(*r);
                    strip_data.push(*g);
                    strip_data.push(*b);
                }
                tiff.write_strip(&strip_data)?;
                pos += samples_count;
            }
            tiff.finish()?;
        } else {
            panic!("Internal error");
        }
        Ok(())
    }

    pub fn load_from_tiff_file(&mut self, file_name: &Path) -> anyhow::Result<()> {
        use tiff::decoder::*;

        fn assign_img_data<S: Copy>(
            src:    &[S],
            img:    &mut Image,
            y1:     usize,
            y2:     usize,
            is_rgb: bool,
            cvt:    fn (from: S) -> u16
        ) -> anyhow::Result<()> {
            let from = y1 * img.width;
            let to = y2 * img.width;
            if is_rgb {
                let r_dst = &mut img.r.as_slice_mut()[from..to];
                let g_dst = &mut img.g.as_slice_mut()[from..to];
                let b_dst = &mut img.b.as_slice_mut()[from..to];
                for (dr, dg, db, (sr, sg, sb))
                in izip!(r_dst, g_dst, b_dst, src.iter().tuples()) {
                    *dr = cvt(*sr);
                    *dg = cvt(*sg);
                    *db = cvt(*sb);
                }
            } else {
                let l_dst = &mut img.l.as_slice_mut()[from..to];
                for (d, s) in izip!(l_dst, src.iter()) {
                    *d = cvt(*s);
                }
            }
            Ok(())
        }

        let file = BufReader::new(File::open(file_name)?);
        let mut decoder = Decoder::new(file)?;
        let (width, height) = decoder.dimensions()?;
        let is_rgb = match decoder.colortype()? {
            tiff::ColorType::Gray(_) => {
                self.make_monochrome(width as usize, height as usize, 0, u16::MAX);
                false
            }
            tiff::ColorType::RGB(_) => {
                self.make_color(width as usize, height as usize, 0, u16::MAX);
                true
            }
            ct =>
                anyhow::bail!("Color type {:?} unsupported", ct)
        };

        let chunk_size_y = decoder.chunk_dimensions().1 as usize;
        let chunks_cnt = decoder.strip_count()? as usize;
        for chunk_index in 0..chunks_cnt {
            let chunk = decoder.read_chunk(chunk_index as u32)?;
            let y1 = (chunk_index * chunk_size_y) as usize;
            let y2 = (y1 + chunk_size_y).min(self.height);
            match chunk {
                DecodingResult::U8(data) =>
                    assign_img_data(
                        &data,
                        self,
                        y1, y2,
                        is_rgb,
                        |v| v as u16 * 256
                    ),

                DecodingResult::U16(data) =>
                    assign_img_data(
                        &data,
                        self,
                        y1, y2,
                        is_rgb,
                        |v| v
                    ),

                DecodingResult::F32(data) =>
                    assign_img_data(
                        &data,
                        self,
                        y1, y2,
                        is_rgb,
                        |v| (v as f64 * u16::MAX as f64) as u16
                    ),

                DecodingResult::F64(data) =>
                    assign_img_data(
                        &data,
                        self,
                        y1, y2,
                        is_rgb,
                        |v| (v * u16::MAX as f64) as u16
                    ),

                _ =>
                    Err(anyhow::anyhow!("Format unsupported"))
            }?;
        }

        Ok(())
    }
}

#[derive(Default)]
pub struct DarkLightLevels {
    pub dark:  f64,
    pub light: f64,
}

#[derive(Default)]
pub struct RgbU8Data {
    pub width:          usize,
    pub height:         usize,
    pub orig_width:     usize,
    pub orig_height:    usize,
    pub bytes:          Vec<u8>,
    pub is_color_image: bool,
}

struct ImageToU8BytesArgs {
    width:          usize,
    height:         usize,
    is_color_image: bool,
}

///////////////////////////////////////////////////////////////////////////////

#[derive(Default)]
struct ImageAdderChan {
    data:   Vec<i32>,
    median: Option<i32>,
}

pub struct ImageAdder {
    r: ImageAdderChan,
    g: ImageAdderChan,
    b: ImageAdderChan,
    l: ImageAdderChan,
    cnt: Vec<u16>,
    width: usize,
    height: usize,
    max_value: u16,
    total_exp: f64,
    frames_cnt: u32,
}

impl ImageAdder {
    pub fn new() -> Self {
        Self {
            r: ImageAdderChan::default(),
            g: ImageAdderChan::default(),
            b: ImageAdderChan::default(),
            l: ImageAdderChan::default(),
            cnt: Vec::new(),
            width: 0,
            height: 0,
            max_value: 0,
            total_exp: 0.0,
            frames_cnt: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.l.data.is_empty() &&
        self.r.data.is_empty() &&
        self.g.data.is_empty() &&
        self.b.data.is_empty() &&
        self.cnt.is_empty()
    }

    pub fn clear(&mut self) {
        self.l = ImageAdderChan::default();
        self.r = ImageAdderChan::default();
        self.g = ImageAdderChan::default();
        self.b = ImageAdderChan::default();
        self.cnt.clear();
        self.cnt.shrink_to_fit();
        self.width = 0;
        self.height = 0;
        self.max_value = 0;
        self.total_exp = 0.0;
        self.frames_cnt = 0;
    }

    pub fn add(
            &mut self,
            image:    &Image,
            hist:     &Histogram,
            transl_x: f64,
            transl_y: f64,
            angle:    f64,
            exposure: f64,
        ) {
        debug_assert!(!image.is_empty());

        if self.width == 0 && self.height == 0 {
            let data_len = image.width * image.height;
            if image.is_color() {
                self.r.data.resize(data_len, 0);
                self.g.data.resize(data_len, 0);
                self.b.data.resize(data_len, 0);
            } else {
                self.l.data.resize(data_len, 0);
            }
            self.cnt.resize(data_len, 0);
            self.width = image.width;
            self.height = image.height;
            self.max_value = image.max_value;
            self.total_exp = 0.0;
            self.frames_cnt = 0;
        }
        if image.is_color() {
            let median_r = hist.r.as_ref().map(|h| h.get_percentile(50)).unwrap_or_default() as i32;
            let median_g = hist.g.as_ref().map(|h| h.get_percentile(50)).unwrap_or_default() as i32;
            let median_b = hist.b.as_ref().map(|h| h.get_percentile(50)).unwrap_or_default() as i32;
            Self::add_layer(&mut self.r, &mut self.cnt, &image.r, median_r, transl_x, transl_y, angle, false);
            Self::add_layer(&mut self.g, &mut self.cnt, &image.g, median_g, transl_x, transl_y, angle, false);
            Self::add_layer(&mut self.b, &mut self.cnt, &image.b, median_b, transl_x, transl_y, angle, true);
        } else {
            let median_l = hist.l.as_ref().map(|h| h.get_percentile(50)).unwrap_or_default() as i32;
            Self::add_layer(&mut self.l, &mut self.cnt, &image.l, median_l, transl_x, transl_y, angle, true);
        }
        self.total_exp += exposure;
        self.frames_cnt += 1;
    }

    fn add_layer(
        dst:        &mut ImageAdderChan,
        cnt:        &mut [u16],
        src:        &ImageLayer<u16>,
        src_median: i32,
        transl_x:   f64,
        transl_y:   f64,
        angle:      f64,
        update_cnt: bool
    ) {
        let center_x = (src.width as f64 - 1.0) / 2.0;
        let center_y = (src.height as f64 - 1.0) / 2.0;
        let cos_a = f64::cos(-angle);
        let sin_a = f64::sin(-angle);
        let dst_median = if let Some(median) = dst.median {
            median
        } else {
            dst.median = Some(src_median);
            src_median
        };
        let offs = dst_median - src_median;
        dst.data.par_chunks_exact_mut(src.width)
            .zip(cnt.par_chunks_exact_mut(src.width))
            .enumerate()
            .for_each(|(y, (dst_row, cnt_row))| {
                let y = y as f64 - transl_y;
                let dy = y - center_y;
                for (x, (dst_v, cnt_v)) in dst_row.iter_mut().zip(cnt_row).enumerate() {
                    let x = x as f64 - transl_x;
                    let dx = x - center_x;
                    let rot_x = center_x + dx * cos_a - dy * sin_a;
                    let rot_y = center_y + dy * cos_a + dx * sin_a;
                    let src_v = src.get_f64_crd(rot_x, rot_y);
                    if let Some(v) = src_v {
                        *dst_v += v as i32 + offs;
                        if update_cnt { *cnt_v += 1; }
                    }
                }
            });
    }

    pub fn save_to_tiff(&self, file_name: &Path) -> anyhow::Result<()> {
        use tiff::encoder::*;
        let mut file = BufWriter::new(File::create(file_name)?);
        let mut decoder = TiffEncoder::new(&mut file)?;
        let mult = 1_f64 / 65536_f64;
        let calc_value = |s, c| {
            if c == 0 {
                f32::NAN
            } else {
                (mult * s as f64 / c as f64) as f32
            }
        };
        if !self.l.data.is_empty() {
            let mut tiff = decoder.new_image::<colortype::Gray32Float>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.rows_per_strip(64)?;
            let mut strip_data = Vec::new();
            let mut pos = 0_usize;
            loop {
                let samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }
                strip_data.clear();
                let l_strip = &self.l.data[pos..pos+samples_count];
                let cnt_strip = &self.cnt[pos..pos+samples_count];
                for (s, c) in l_strip.iter().zip(cnt_strip) {
                    strip_data.push(calc_value(*s, *c));
                }
                tiff.write_strip(&strip_data)?;
                pos += samples_count;
            }
            tiff.finish()?;
        } else {
            let mut tiff = decoder.new_image::<colortype::RGB32Float>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.rows_per_strip(64)?;
            let mut strip_data = Vec::new();
            let mut pos = 0_usize;
            loop {
                let mut samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }
                samples_count /= 3;
                strip_data.clear();
                let r_strip = &self.r.data[pos..pos+samples_count];
                let g_strip = &self.g.data[pos..pos+samples_count];
                let b_strip = &self.b.data[pos..pos+samples_count];
                let cnt_strip = &self.cnt[pos..pos+samples_count];
                for (r, g, b, c) in izip!(r_strip, g_strip, b_strip, cnt_strip) {
                    strip_data.push(calc_value(*r, *c));
                    strip_data.push(calc_value(*g, *c));
                    strip_data.push(calc_value(*b, *c));
                }
                tiff.write_strip(&strip_data)?;
                pos += samples_count;
            }
            tiff.finish()?;
        }
        Ok(())
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn total_exposure(&self) -> f64 {
        self.total_exp
    }

    pub fn copy_to_image(&self, image: &mut Image, mt: bool) {
        let copy_layer = |src: &[i32], dst: &mut ImageLayer<u16>| {
            if src.is_empty() {
                dst.clear();
                return;
            }
            dst.resize(self.width, self.height);
            for (d, s, c) in izip!(dst.as_slice_mut(), src, &self.cnt) {
                let mut value = if *c != 0 { *s / *c as i32 } else { 0 };
                if value < 0 { value = 0; }
                if value > u16::MAX as i32 { value = u16::MAX as i32; }
                *d = value as u16;
            }
        };
        image.width = self.width;
        image.height = self.height;
        image.max_value = self.max_value;
        image.zero = 0;
        if !mt {
            copy_layer(&self.r.data, &mut image.r);
            copy_layer(&self.g.data, &mut image.g);
            copy_layer(&self.b.data, &mut image.b);
            copy_layer(&self.l.data, &mut image.l);
        } else {
            rayon::scope(|s| {
                s.spawn(|_| copy_layer(&self.r.data, &mut image.r));
                s.spawn(|_| copy_layer(&self.g.data, &mut image.g));
                s.spawn(|_| copy_layer(&self.b.data, &mut image.b));
                s.spawn(|_| copy_layer(&self.l.data, &mut image.l));
            });
        }
    }

}

//////////////////////////////////////////////////////////////////////////////

trait GradientCalcSource {
    fn image_width(&self) -> usize;
    fn image_height(&self) -> usize;
    fn get_rect_values(&self, x1: usize, y1: usize, x2: usize, y2: usize, result: &mut Vec<u16>);
}

fn calc_gradient(source: &dyn GradientCalcSource) -> Option<Plane> {
    let width = source.image_width();
    let height = source.image_height();
    let min_size = usize::min(width, height);
    let cell_size = min_size / 30;
    let border = cell_size / 3;
    let cells_cnt = (min_size - 2 * border) / cell_size;
    let corner_cells_cnt = usize::max(cells_cnt / 4, 1);
    let mut cell_data = Vec::new();
    let mut points = Vec::new();
    let mut add_cell = |x, y| {
        source.get_rect_values(
            x - cell_size/2,
            y - cell_size/2,
            x + cell_size/2,
            y + cell_size/2,
            &mut cell_data
        );
        let bound1 = cell_data.len()/3;
        let bound2 = 2*cell_data.len()/3;

        cell_data.select_nth_unstable(bound2);
        cell_data[..bound2].select_nth_unstable(bound1);
        let middle = &cell_data[bound1..bound2];
        let aver = middle.iter().map(|v| *v as f64).sum::<f64>() / middle.len() as f64;

        points.push(Point3D {
            x: x as f64,
            y: y as f64,
            z: aver,
        });
    };
    let mut add_corner_cell = |x, y| {
        add_cell(x,       y       );
        add_cell(width-x, y       );
        add_cell(x,       height-y);
        add_cell(width-x, height-y);
    };
    for i in 0..corner_cells_cnt {
        let x = border + cell_size/2 + i * cell_size;
        let y = border + cell_size/2;
        add_corner_cell(x, y);
    }
    for i in 1..corner_cells_cnt-1 {
        let x = border + cell_size/2;
        let y = border + cell_size/2 + i * cell_size;
        add_corner_cell(x, y);
    }
    let z_aver = points.iter().map(|p| p.z).sum::<f64>() / points.len() as f64;
    for p in &mut points {
        p.z -= z_aver;
    }
    calc_fitting_plane_z_dist(&points)
}