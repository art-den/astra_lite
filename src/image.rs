use std::{path::Path, io::BufWriter, fs::File};

use itertools::*;
use rayon::prelude::*;

use crate::{math::*};

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
        in self.data.iter().step_by(7).tuples() {
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

        let (v00, v10, v01, v11) =
            if x_i >= 0 && y_i >= 0 && x_i < self.width_1 && y_i < self.height_1 {
                let pos = x_i as usize + y_i as usize * self.width;
                unsafe {(
                    Some(*self.data.get_unchecked(pos)),
                    Some(*self.data.get_unchecked(pos+1)),
                    Some(*self.data.get_unchecked(pos + self.width)),
                    Some(*self.data.get_unchecked(pos + self.width+1))
                )}
            } else {
                (
                    self.get(x_i as isize, y_i as isize),
                    self.get(x_i as isize+1, y_i as isize),
                    self.get(x_i as isize, y_i as isize+1),
                    self.get(x_i as isize+1, y_i as isize+1)
                )
            };

        let x_p1 = x % CRD_DIV;
        let x_p0 = CRD_DIV - x_p1;

        let v0 = match (v00, v10) {
            (Some(v00), Some(v10)) => Some((v00 as i64 * x_p0) + (v10 as i64 * x_p1)),
            (Some(v00), None)      => Some((v00 as i64) * CRD_DIV),
            (None, Some(v10))      => Some((v10 as i64) * CRD_DIV),
            _                      => None,
        };
        let v1 = match (v01, v11) {
            (Some(v01), Some(v11)) => Some((v01 as i64 * x_p0) + (v11 as i64 * x_p1)),
            (Some(v01), None)      => Some((v01 as i64) * CRD_DIV),
            (None, Some(v11))      => Some((v11 as i64) * CRD_DIV),
            _                      => None,
        };

        let y_p1 = y % CRD_DIV;
        let y_p0 = CRD_DIV - y_p1;

        let v = match (v0, v1) {
            (Some(v0), Some(v1)) => v0 * y_p0 + v1 * y_p1,
            (Some(v0), None)     => v0 * CRD_DIV,
            (None, Some(v1))     => v1 * CRD_DIV,
            _                    => return None,
        };

        let mut result = v / (CRD_DIV * CRD_DIV);
        if result > u16::MAX as i64 { result = u16::MAX as i64; }
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

    pub fn new_color(
        width:     usize,
        height:    usize,
        zero:      i32,
        max_value: u16
    ) -> Image {
        Image {
            l: ImageLayer::new_empty(),
            r: ImageLayer::new_with_size(width, height),
            g: ImageLayer::new_with_size(width, height),
            b: ImageLayer::new_with_size(width, height),
            width,
            height,
            zero,
            max_value
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
            let data: Vec<_> = izip!(self.r.as_slice(), self.g.as_slice(), self.b.as_slice())
                .map(|(r, g, b)| {
                    let r = (*r as i32 + self.zero).max(0).min(u16::MAX as i32);
                    let g = (*g as i32 + self.zero).max(0).min(u16::MAX as i32);
                    let b = (*b as i32 + self.zero).max(0).min(u16::MAX as i32);
                    [r as u16, g as u16, b as u16]
                })
                .flatten()
                .collect();
            let tiff = decoder.new_image::<colortype::RGB16>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.write_data(&data)?;
        } else {
            panic!("Internal error");
        }
        Ok(())
    }

    pub fn to_grb_bytes(
        &self,
        l_black_level: Option<i32>,
        r_black_level: Option<i32>,
        g_black_level: Option<i32>,
        b_black_level: Option<i32>,
        gamma:         f64,
        reduct_ratio:  usize,
    ) -> RgbU8Data {
        if self.is_empty() {
            return RgbU8Data::default();
        }
        let args = ImageToU8BytesArgs {
            width:          self.width,
            height:         self.height,
            is_color_image: self.is_color(),
            max_value:      self.max_value,
            l_black_level:  l_black_level.unwrap_or(self.zero),
            r_black_level:  r_black_level.unwrap_or(self.zero),
            g_black_level:  g_black_level.unwrap_or(self.zero),
            b_black_level:  b_black_level.unwrap_or(self.zero),
            gamma,
        };
        let table = Self::create_gamma_table(args.max_value as f32, args.gamma);
        match reduct_ratio {
            1 => self.to_grb_bytes_no_reduct(&table, args),
            2 => self.to_grb_bytes_reduct2(&table, args),
            3 => self.to_grb_bytes_reduct3(&table, args),
            4 => self.to_grb_bytes_reduct4(&table, args),
            _ => panic!("Wrong reduct_ratio ({})", reduct_ratio),
        }
    }

    fn create_gamma_table(max_value: f32, gamma: f64) -> Vec<u8> {
        let pow_value = 1.0 / gamma as f32;
        let k = 255_f32 / max_value.powf(pow_value);
        let mut table = Vec::new();
        for i in 0..65536 {
            table.push((k * (i as f32).powf(pow_value)) as u8);
        }
        table
    }

    fn to_grb_bytes_no_reduct(
        &self,
        table: &[u8],
        args:  ImageToU8BytesArgs,
    ) -> RgbU8Data {
        let mut rgb_bytes = Vec::with_capacity(3 * args.width * args.height);
        if args.is_color_image {
            for row in 0..args.height {
                let r_iter = self.r.row(row).iter();
                let g_iter = self.g.row(row).iter();
                let b_iter = self.b.row(row).iter();
                for (r, g, b) in
                izip!(r_iter, g_iter, b_iter) {
                    rgb_bytes.push(table[(*r as i32 - args.r_black_level).min(65535).max(0) as usize]);
                    rgb_bytes.push(table[(*g as i32 - args.g_black_level).min(65535).max(0) as usize]);
                    rgb_bytes.push(table[(*b as i32 - args.b_black_level).min(65535).max(0) as usize]);
                }
            }
        } else {
            for row in 0..args.height {
                for l in self.l.row(row).iter() {
                    let l = table[(*l as i32 - args.l_black_level).min(65535).max(0) as usize];
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
        }
    }

    fn to_grb_bytes_reduct2(
        &self,
        table: &[u8],
        args:  ImageToU8BytesArgs,
    ) -> RgbU8Data {
        let width = args.width / 2;
        let height = args.height / 2;
        let mut bytes = Vec::with_capacity(3 * width * height);
        if args.is_color_image {
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
                    bytes.push(table[(r as i32 - args.r_black_level).min(65535).max(0) as usize]);
                    bytes.push(table[(g as i32 - args.g_black_level).min(65535).max(0) as usize]);
                    bytes.push(table[(b as i32 - args.b_black_level).min(65535).max(0) as usize]);
                    r0 = r0.wrapping_offset(2);
                    r1 = r1.wrapping_offset(2);
                    g0 = g0.wrapping_offset(2);
                    g1 = g1.wrapping_offset(2);
                    b0 = b0.wrapping_offset(2);
                    b1 = b1.wrapping_offset(2);
                }
            }
        } else {
            for y in 0..height {
                let mut l0 = self.l.row(2*y).as_ptr();
                let mut l1 = self.l.row(2*y+1).as_ptr();
                for _ in 0..width {
                    let l = unsafe {(
                        *l0 as u32 + *l0.offset(1) as u32 +
                        *l1 as u32 + *l1.offset(1) as u32 + 2
                    ) / 4};
                    let l = table[(l as i32 - args.l_black_level).min(65535).max(0) as usize];
                    bytes.push(l);
                    bytes.push(l);
                    bytes.push(l);
                    l0 = l0.wrapping_offset(2);
                    l1 = l1.wrapping_offset(2);
                }
            }
        }
        RgbU8Data { width, height, bytes, orig_width: self.width, orig_height: self.height }
    }

    fn to_grb_bytes_reduct3(
        &self,
        table: &[u8],
        args:  ImageToU8BytesArgs,
    ) -> RgbU8Data {
        let width = args.width / 3;
        let height = args.height / 3;
        let mut bytes = Vec::with_capacity(3 * width * height);
        if args.is_color_image {
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
                    bytes.push(table[(r as i32 - args.r_black_level).min(65535).max(0) as usize]);
                    bytes.push(table[(g as i32 - args.g_black_level).min(65535).max(0) as usize]);
                    bytes.push(table[(b as i32 - args.b_black_level).min(65535).max(0) as usize]);
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
            for y in 0..height {
                let mut l0 = self.l.row(3*y).as_ptr();
                let mut l1 = self.l.row(3*y+1).as_ptr();
                let mut l2 = self.l.row(3*y+2).as_ptr();
                for _ in 0..width {
                    let l = unsafe {(
                        *l0 as u32 + *l0.offset(1) as u32 + *l0.offset(2) as u32 +
                        *l1 as u32 + *l1.offset(1) as u32 + *l1.offset(2) as u32 +
                        *l2 as u32 + *l2.offset(1) as u32 + *l2.offset(2) as u32 + 4
                    ) / 9};
                    let l = table[(l as i32 - args.l_black_level).min(65535).max(0) as usize];
                    bytes.push(l);
                    bytes.push(l);
                    bytes.push(l);
                    l0 = l0.wrapping_offset(3);
                    l1 = l1.wrapping_offset(3);
                    l2 = l2.wrapping_offset(3);
                }
            }
        }
        RgbU8Data { width, height, bytes, orig_width: self.width, orig_height: self.height }
    }

    fn to_grb_bytes_reduct4(
        &self,
        table: &[u8],
        args:  ImageToU8BytesArgs,
    ) -> RgbU8Data {
        let width = args.width / 4;
        let height = args.height / 4;
        let mut bytes = Vec::with_capacity(3 * width * height);
        if args.is_color_image {
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
                    bytes.push(table[(r as i32 - args.r_black_level).min(65535).max(0) as usize]);
                    bytes.push(table[(g as i32 - args.g_black_level).min(65535).max(0) as usize]);
                    bytes.push(table[(b as i32 - args.b_black_level).min(65535).max(0) as usize]);
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
            for y in 0..height {
                let mut l0 = self.l.row(4*y).as_ptr();
                let mut l1 = self.l.row(4*y+1).as_ptr();
                let mut l2 = self.l.row(4*y+2).as_ptr();
                let mut l3 = self.l.row(4*y+3).as_ptr();
                for _ in 0..width {
                    let l = unsafe {(
                        *l0 as u32 + *l0.offset(1) as u32 + *l0.offset(2) as u32 + *l0.offset(3) as u32 +
                        *l1 as u32 + *l1.offset(1) as u32 + *l1.offset(2) as u32 + *l1.offset(3) as u32 +
                        *l2 as u32 + *l2.offset(1) as u32 + *l2.offset(2) as u32 + *l2.offset(3) as u32 +
                        *l3 as u32 + *l3.offset(1) as u32 + *l3.offset(2) as u32 + *l3.offset(3) as u32 + 8
                    ) / 16};
                    let l = table[(l as i32 - args.l_black_level).min(65535).max(0) as usize];
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
        RgbU8Data { width, height, bytes, orig_width: self.width, orig_height: self.height }
    }

    pub fn remove_gradient(&mut self) {
        self.l.remove_gradient();
        self.r.remove_gradient();
        self.g.remove_gradient();
        self.b.remove_gradient();
    }
}

#[derive(Default)]
pub struct RgbU8Data {
    pub width:       usize,
    pub height:      usize,
    pub orig_width:  usize,
    pub orig_height: usize,
    pub bytes:       Vec<u8>,
}

struct ImageToU8BytesArgs {
    width:          usize,
    height:         usize,
    is_color_image: bool,
    max_value:      u16,
    l_black_level:  i32,
    r_black_level:  i32,
    g_black_level:  i32,
    b_black_level:  i32,
    gamma:          f64,
}

///////////////////////////////////////////////////////////////////////////////

pub struct ImageAdder {
    r: Vec<i32>,
    g: Vec<i32>,
    b: Vec<i32>,
    l: Vec<i32>,
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
            r: Vec::new(),
            g: Vec::new(),
            b: Vec::new(),
            l: Vec::new(),
            cnt: Vec::new(),
            width: 0,
            height: 0,
            max_value: 0,
            total_exp: 0.0,
            frames_cnt: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.l.is_empty() &&
        self.r.is_empty() &&
        self.g.is_empty() &&
        self.b.is_empty() &&
        self.cnt.is_empty()
    }

    pub fn clear(&mut self) {
        self.l.clear();
        self.l.shrink_to_fit();
        self.r.clear();
        self.r.shrink_to_fit();
        self.g.clear();
        self.g.shrink_to_fit();
        self.b.clear();
        self.b.shrink_to_fit();
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
            image: &Image,
            transl_x: f64,
            transl_y: f64,
            angle: f64,
            exposure: f64,
            mt: bool
        ) {
        debug_assert!(!image.is_empty());

        if self.width == 0 && self.height == 0 {
            let data_len = image.width * image.height;
            if image.is_color() {
                self.r.resize(data_len, 0);
                self.g.resize(data_len, 0);
                self.b.resize(data_len, 0);
            } else {
                self.l.resize(data_len, 0);
            }
            self.cnt.resize(data_len, 0);
            self.width = image.width;
            self.height = image.height;
            self.max_value = image.max_value;
            self.total_exp = 0.0;
            self.frames_cnt = 0;
        }
        if image.is_color() {
            Self::add_layer(&mut self.r, &mut self.cnt, &image.r, image.zero, transl_x, transl_y, angle, false, mt);
            Self::add_layer(&mut self.g, &mut self.cnt, &image.g, image.zero, transl_x, transl_y, angle, false, mt);
            Self::add_layer(&mut self.b, &mut self.cnt, &image.b, image.zero, transl_x, transl_y, angle, true, mt);
        } else {
            Self::add_layer(&mut self.l, &mut self.cnt, &image.l, image.zero, transl_x, transl_y, angle, true, mt);
        }
        self.total_exp += exposure;
        self.frames_cnt += 1;
    }

    fn add_layer(
        dst:        &mut [i32],
        cnt:        &mut [u16],
        src:        &ImageLayer<u16>,
        src_zero:   i32,
        transl_x:   f64,
        transl_y:   f64,
        angle:      f64,
        update_cnt: bool,
        mt:         bool
    ) {
        let center_x = (src.width as f64 - 1.0) / 2.0;
        let center_y = (src.height as f64 - 1.0) / 2.0;
        let cos_a = f64::cos(-angle);
        let sin_a = f64::sin(-angle);

        let add_row = |y: usize, dst_row: &mut [i32], cnt_row: &mut [u16]| {
            let y = y as f64 - transl_y;
            let dy = y - center_y;
            for (x, (dst_v, cnt_v)) in dst_row.iter_mut().zip(cnt_row).enumerate() {
                let x = x as f64 - transl_x;
                let dx = x - center_x;
                let rot_x = center_x + dx * cos_a - dy * sin_a;
                let rot_y = center_y + dy * cos_a + dx * sin_a;
                let src_v = src.get_f64_crd(rot_x, rot_y);
                if let Some(v) = src_v {
                    *dst_v += v as i32 - src_zero;
                    if update_cnt { *cnt_v += 1; }
                }
            }
        };

        if !mt {
            for (y, (dst_row, cnt_row)) in izip!(
                dst.chunks_exact_mut(src.width),
                cnt.chunks_exact_mut(src.width)
            ).enumerate() {
                add_row(y, dst_row, cnt_row)
            }
        } else {
            dst.par_chunks_exact_mut(src.width)
                .zip(cnt.par_chunks_exact_mut(src.width))
                .enumerate()
                .for_each(|(y, (dst_row, cnt_row))| {
                    add_row(y, dst_row, cnt_row)
                });
        }
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
        if !self.l.is_empty() {
            let mut tiff = decoder.new_image::<colortype::Gray32Float>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.rows_per_strip(16)?;
            let mut strip_data = Vec::new();
            let mut pos = 0_usize;
            loop {
                let samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }
                strip_data.clear();
                let l_strip = &self.l[pos..pos+samples_count];
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
            tiff.rows_per_strip(16)?;
            let mut strip_data = Vec::new();
            let mut pos = 0_usize;
            loop {
                let mut samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }
                samples_count /= 3;
                strip_data.clear();
                let r_strip = &self.r[pos..pos+samples_count];
                let g_strip = &self.g[pos..pos+samples_count];
                let b_strip = &self.b[pos..pos+samples_count];
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
            copy_layer(&self.r, &mut image.r);
            copy_layer(&self.g, &mut image.g);
            copy_layer(&self.b, &mut image.b);
            copy_layer(&self.l, &mut image.l);
        } else {
            rayon::scope(|s| {
                s.spawn(|_| copy_layer(&self.r, &mut image.r));
                s.spawn(|_| copy_layer(&self.g, &mut image.g));
                s.spawn(|_| copy_layer(&self.b, &mut image.b));
                s.spawn(|_| copy_layer(&self.l, &mut image.l));
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
        cell_data.select_nth_unstable(bound1);
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