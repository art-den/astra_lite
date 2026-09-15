#![allow(dead_code)]

use itertools::*;
use rayon::prelude::*;
use crate::utils::math::*;

use super::raw::RawImageInfo;


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

    pub fn new_mono(data: Vec<T>, width: usize, height: usize) -> Self {
        assert!(data.len() == width * height);
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

    #[inline(always)]
    pub fn set(&mut self, x: isize, y: isize, value: T) {
        if x < 0
        || y < 0
        || x >= self.width as isize
        || y >= self.height as isize {
            panic!("Wrong coordinates: x={}, y={}", x, y);
        }
        self.data[(y as usize) * self.width + (x as usize)] = value;
    }

    #[inline(always)]
    pub fn get(&self, x: isize, y: isize) -> Option<T> {
        if x < 0
        || y < 0
        || x >= self.width as isize
        || y >= self.height as isize {
            None
        } else {
            Some(unsafe {
                *self.data.get_unchecked(x as usize + y as usize * self.width)
            })
        }
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn coord_iter(&self) -> CoordIterator<'_, T> {
        CoordIterator::<T> {
            x: 0,
            y: 0,
            width: self.width,
            iter: self.data.iter(),
        }
    }

    pub fn rect_iter(&self, mut x1: isize, mut y1: isize, mut x2: isize, mut y2: isize) -> RectIterator<'_, T> {
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
    pub fn iter_div2(&self) -> DivNLIterator<'_, 2> {
        DivNLIterator::new(self)
    }

    pub fn iter_div3(&self) -> DivNLIterator<'_, 3> {
        DivNLIterator::new(self)
    }

    pub fn iter_div4(&self) -> DivNLIterator<'_, 4> {
        DivNLIterator::new(self)
    }

    pub fn calc_noise(&self) -> f32 {
        let mut diffs = Vec::with_capacity(self.data.len()/10);
        // To roughly estimate the noise level, we can take every 7th tuple of 10 pixels
        for (v1, v2, v3, v4, v5, m, v6, v7, v8, v9, v10)
        in self.data.iter().tuples().step_by(7) {
            let avg = (
                *v1 as u32 + *v2 as u32 +
                *v3 as u32 + *v4 as u32 +
                *v5 as u32 + *v6 as u32 +
                *v7 as u32 + *v8 as u32 +
                *v9 as u32 + *v10 as u32 + 5
            ) / 10;
            let m = *m as u32;
            let diff = u32::abs_diff(m, avg);
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
            self.data.par_iter().map(|v| *v as u64).sum()
        } else {
            self.data.iter().map(|v| *v as u64).sum()
        };
        (sum / self.data.len() as u64) as u16
    }

    pub fn get_crd_i64(&self, x: i64, y: i64) -> Option<u16> {
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
        self.get_crd_i64(
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

impl<T: Copy + Default> RectIterator<'_, T> {
    fn init_iter(img: &ImageLayer<T>, x1: usize, x2: usize, y: usize) -> std::slice::Iter<'_, T> {
        let row = img.row(y);
        row[x1 ..= x2].iter()
    }
}

impl<T: Copy + Default> Iterator for RectIterator<'_, T> {
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

pub struct CoordIterator<'a, T> {
    x: usize,
    y: usize,
    width: usize,
    iter: std::slice::Iter<'a, T>,
}

impl<T: Copy + Default> Iterator for CoordIterator<'_, T> {
    type Item = (usize, usize, T);

    fn next(&mut self) -> Option<Self::Item> {
        match self.iter.next() {
            Some(v) => {
                let result = Some((self.x, self.y, *v));
                self.x += 1;
                if self.x == self.width {
                    self.x = 0;
                    self.y += 1;
                }
                result
            }
            None => None,
        }
    }
}

// Iterates over pixels of a monochrome layer reduced N times:
// each N*N block is averaged (same formula as the preview reduct functions).
// Uses raw pointers to avoid bounds checks and per-pixel address calculations;
// N is a const generic, so the block loops are unrolled at compile time.
pub struct DivNLIterator<'a, const N: usize> {
    layer:    &'a ImageLayer<u16>,
    // pointers to the left pixel of the current NxN block, one per its row
    rows:     [*const u16; N],
    advance:  isize,
    // remaining pixels in the current reduced row
    x:        usize,
    y:        usize,
    width:    usize,
    height:   usize,
}

impl<'a, const N: usize> DivNLIterator<'a, N> {
    fn new(layer: &'a ImageLayer<u16>) -> Self {
        assert!(N > 0);
        let width = layer.width();
        let data = layer.as_slice();
        let mut rows = [data.as_ptr(); N];
        for (i, p) in rows.iter_mut().enumerate() {
            *p = unsafe { data.as_ptr().add(i * width) };
        }
        Self {
            layer,
            rows,
            // pointer jump at the row end, skipping the last columns if width % N != 0
            advance: N as isize * (width - width / N) as isize,
            x: width / N,
            y: 0,
            width: width / N,
            height: layer.height() / N,
        }
    }

    // sum of the NxN block the pointers are pointing at
    // (not named `sum` to avoid resolving to Iterator::sum in `next`)
    fn calc_sum(&self) -> u32 {
        let mut sum = 0u32;
        unsafe {
            for i in 0..N {
                let p = self.rows[i];
                for j in 0..N {
                    sum += *p.add(j) as u32;
                }
            }
        }
        sum
    }

    fn advance_rows(rows: &mut [*const u16; N], count: isize) {
        // pointer updates stay within the data allocation (at most one-past-the-end)
        unsafe {
            for p in rows.iter_mut() {
                *p = p.offset(count);
            }
        }
    }
}

impl<const N: usize> Iterator for DivNLIterator<'_, N> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        if self.width == 0
        || self.y >= self.height {
            return None;
        }
        // all reads are inside the layer data: reduced rows/cols are dropped
        // for non-divisible sizes, so the last NxN block never crosses the border
        let sum = self.calc_sum();
        let k = N as u32 * N as u32;
        let v = ((sum + k/2) / k) as u16;
        Self::advance_rows(&mut self.rows, N as isize);
        self.x -= 1;
        if self.x == 0 {
            self.x = self.width;
            self.y += 1;
            Self::advance_rows(&mut self.rows, self.advance);
        }
        Some(v)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = if self.width == 0 || self.y >= self.height {
            0
        } else {
            (self.height - self.y - 1) * self.width + self.x
        };
        (remaining, Some(remaining))
    }
}

// Same as DivNLIterator, but averages each NxN block of the r, g and b
// layers of a color image and yields (r, g, b) tuples.
pub struct DivNRGBIterator<'a, const N: usize> {
    image:    &'a Image,
    r_rows:   [*const u16; N],
    g_rows:   [*const u16; N],
    b_rows:   [*const u16; N],
    advance:  isize,
    // remaining pixels in the current reduced row
    x:        usize,
    y:        usize,
    width:    usize,
    height:   usize,
}

impl<'a, const N: usize> DivNRGBIterator<'a, N> {
    fn new(image: &'a Image) -> Self {
        assert!(N > 0);
        let width = image.r.width();
        let r = image.r.as_slice();
        let g = image.g.as_slice();
        let b = image.b.as_slice();
        let mut r_rows = [r.as_ptr(); N];
        let mut g_rows = [g.as_ptr(); N];
        let mut b_rows = [b.as_ptr(); N];
        for i in 0..N {
            unsafe {
                r_rows[i] = r.as_ptr().add(i * width);
                g_rows[i] = g.as_ptr().add(i * width);
                b_rows[i] = b.as_ptr().add(i * width);
            }
        }
        Self {
            image,
            r_rows,
            g_rows,
            b_rows,
            // pointer jump at the row end, skipping the last columns if width % N != 0
            advance: N as isize * (width - width / N) as isize,
            x: width / N,
            y: 0,
            width: width / N,
            height: image.r.height() / N,
        }
    }

    // average of the NxN block the row pointers are pointing at
    fn avg(rows: &[*const u16; N]) -> u16 {
        let mut sum = 0u32;
        unsafe {
            for &p in rows {
                for i in 0..N {
                    sum += *p.add(i) as u32;
                }
            }
        }
        let k = N as u32 * N as u32;
        ((sum + k/2) / k) as u16
    }
}

impl<const N: usize> Iterator for DivNRGBIterator<'_, N> {
    type Item = (u16, u16, u16);

    fn next(&mut self) -> Option<Self::Item> {
        if self.width == 0
        || self.y >= self.height {
            return None;
        }
        // all reads are inside the layers data: reduced rows/cols are dropped
        // for non-divisible sizes, so the last NxN block never crosses the border
        let r = Self::avg(&self.r_rows);
        let g = Self::avg(&self.g_rows);
        let b = Self::avg(&self.b_rows);
        // pointer updates stay within the layers data (at most one-past-the-end)
        unsafe {
            for p in self.r_rows.iter_mut() { *p = p.add(N); }
            for p in self.g_rows.iter_mut() { *p = p.add(N); }
            for p in self.b_rows.iter_mut() { *p = p.add(N); }
        }
        self.x -= 1;
        if self.x == 0 {
            self.x = self.width;
            self.y += 1;
            unsafe {
                for p in self.r_rows.iter_mut() { *p = p.offset(self.advance); }
                for p in self.g_rows.iter_mut() { *p = p.offset(self.advance); }
                for p in self.b_rows.iter_mut() { *p = p.offset(self.advance); }
            }
        }
        Some((r, g, b))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = if self.width == 0 || self.y >= self.height {
            0
        } else {
            (self.height - self.y - 1) * self.width + self.x
        };
        (remaining, Some(remaining))
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct Image {
    pub r:        ImageLayer<u16>,
    pub g:        ImageLayer<u16>,
    pub b:        ImageLayer<u16>,
    pub l:        ImageLayer<u16>,
    pub raw_info: Option<RawImageInfo>,
    zero:         i32,
    max_value:    u16,
}

impl Image {
    pub fn new_empty() -> Self {
        Self {
            l: ImageLayer::new_empty(),
            r: ImageLayer::new_empty(),
            g: ImageLayer::new_empty(),
            b: ImageLayer::new_empty(),
            raw_info: None,
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
        self.zero = zero;
        self.max_value = max_value;
    }

    pub fn clear(&mut self) {
        self.l.clear();
        self.r.clear();
        self.g.clear();
        self.b.clear();
        self.zero = 0;
        self.max_value = 0;
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

    pub fn width(&self) -> usize {
        if self.l.width != 0 {
            self.l.width
        } else {
            self.r.width
        }
    }

    pub fn height(&self) -> usize {
        if self.l.height != 0 {
            self.l.height
        } else {
            self.r.height
        }
    }

    pub fn iter_div2_l(&self) -> DivNLIterator<'_, 2> {
        self.l.iter_div2()
    }

    pub fn iter_div3_l(&self) -> DivNLIterator<'_, 3> {
        self.l.iter_div3()
    }

    pub fn iter_div4_l(&self) -> DivNLIterator<'_, 4> {
        self.l.iter_div4()
    }

    pub fn iter_div2_rgb(&self) -> DivNRGBIterator<'_, 2> {
        DivNRGBIterator::new(self)
    }

    pub fn iter_div3_rgb(&self) -> DivNRGBIterator<'_, 3> {
        DivNRGBIterator::new(self)
    }

    pub fn iter_div4_rgb(&self) -> DivNRGBIterator<'_, 4> {
        DivNRGBIterator::new(self)
    }

    pub fn max_value(&self) -> u16 {
        self.max_value
    }

    pub fn set_max_value(&mut self, max_value: u16) {
        self.max_value = max_value;
    }

    pub fn remove_gradient(&mut self) {
        self.l.remove_gradient();
        self.r.remove_gradient();
        self.g.remove_gradient();
        self.b.remove_gradient();
    }
}


//////////////////////////////////////////////////////////////////////////////

trait GradientCalcSource {
    fn image_width(&self) -> usize;
    fn image_height(&self) -> usize;
    fn get_rect_values(&self, x1: usize, y1: usize, x2: usize, y2: usize, result: &mut Vec<u16>);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_color_image(width: usize, height: usize) -> Image {
        let mut img = Image::new_empty();
        img.make_color(width, height, 0, u16::MAX);
        for y in 0..height {
            for x in 0..width {
                img.r.set(x as isize, y as isize, (x * 7 + y * 13) as u16 % 4096);
                img.g.set(x as isize, y as isize, (x * 11 + y * 5) as u16 % 4096);
                img.b.set(x as isize, y as isize, (x * 3 + y * 17) as u16 % 4096);
            }
        }
        img
    }

    fn make_image(width: usize, height: usize, fill: impl Fn(usize, usize) -> u16) -> Image {
        let mut img = Image::new_empty();
        img.make_monochrome(width, height, 0, u16::MAX);
        for y in 0..height {
            for x in 0..width {
                img.l.set(x as isize, y as isize, fill(x, y));
            }
        }
        img
    }

    #[test]
    fn test_iter_div2_l() {
        let img = make_image(4, 4, |_, _| 8);
        for v in img.iter_div2_l() {
            assert_eq!(v, 8);
        }
        assert_eq!(img.iter_div2_l().count(), 4);
    }

    #[test]
    fn test_iter_div2_l_average() {
        let img = make_image(2, 2, |x, y| (x + y) as u16);
        // values: 0 1 / 1 2 => one pixel (0+1+1+2)/4 = 1
        let values: Vec<u16> = img.iter_div2_l().collect();
        assert_eq!(values, vec![1]);
    }

    #[test]
    fn test_iter_div2_l_odd_size() {
        // 5x3: reduced image is 2x1
        let img = make_image(5, 3, |_, _| 10);
        let values: Vec<u16> = img.iter_div2_l().collect();
        assert_eq!(values, vec![10, 10]);
    }

    #[test]
    fn test_iter_div_l_empty() {
        let img = Image::new_empty();
        assert_eq!(img.iter_div2_l().count(), 0);
        assert_eq!(img.iter_div3_l().count(), 0);
        assert_eq!(img.iter_div4_l().count(), 0);
    }

    #[test]
    fn test_iter_div_l_matches_reference() {
        for n in [2, 3, 4] {
            for (w, h) in [(4, 4), (5, 3), (7, 9), (1, 8), (8, 1), (12, 8), (13, 10)] {
                let img = make_image(w, h, |x, y| (x * 7 + y * 13) as u16 % 4096);
                let mut expected = Vec::with_capacity((w/n) * (h/n));
                for y in 0..h/n {
                    for x in 0..w/n {
                        let mut v = 0u32;
                        for dy in 0..n {
                            for dx in 0..n {
                                v += img.l.get((x*n + dx) as isize, (y*n + dy) as isize).unwrap() as u32;
                            }
                        }
                        expected.push(((v + (n*n) as u32 / 2) / (n*n) as u32) as u16);
                    }
                }
                let actual: Vec<u16> = match n {
                    2 => img.iter_div2_l().collect(),
                    3 => img.iter_div3_l().collect(),
                    _ => img.iter_div4_l().collect(),
                };
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn test_iter_div_rgb_matches_reference() {
        for n in [2, 3, 4] {
            for (w, h) in [(4, 4), (5, 3), (7, 9), (1, 8), (8, 1), (12, 8), (13, 10)] {
                let img = make_color_image(w, h);
                let mut expected = Vec::with_capacity((w/n) * (h/n));
                for y in 0..h/n {
                    for x in 0..w/n {
                        let mut r = 0u32;
                        let mut g = 0u32;
                        let mut b = 0u32;
                        for dy in 0..n {
                            for dx in 0..n {
                                r += img.r.get((x*n + dx) as isize, (y*n + dy) as isize).unwrap() as u32;
                                g += img.g.get((x*n + dx) as isize, (y*n + dy) as isize).unwrap() as u32;
                                b += img.b.get((x*n + dx) as isize, (y*n + dy) as isize).unwrap() as u32;
                            }
                        }
                        let k = (n*n) as u32;
                        expected.push((
                            ((r + k/2) / k) as u16,
                            ((g + k/2) / k) as u16,
                            ((b + k/2) / k) as u16,
                        ));
                    }
                }
                let actual: Vec<(u16, u16, u16)> = match n {
                    2 => img.iter_div2_rgb().collect(),
                    3 => img.iter_div3_rgb().collect(),
                    _ => img.iter_div4_rgb().collect(),
                };
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn test_iter_div_rgb_empty() {
        let img = Image::new_empty();
        assert_eq!(img.iter_div2_rgb().count(), 0);
        assert_eq!(img.iter_div3_rgb().count(), 0);
        assert_eq!(img.iter_div4_rgb().count(), 0);
    }

    #[test]
    fn test_iter_div2_rgb_size_hint() {
        let img = make_color_image(10, 6);
        let mut it = img.iter_div2_rgb();
        assert_eq!(it.size_hint(), (15, Some(15)));
        it.next(); it.next();
        assert_eq!(it.size_hint(), (13, Some(13)));
    }

    #[test]
    fn test_iter_div_l_size_hint() {
        let img = make_image(10, 6, |_, _| 0);
        let mut it = img.iter_div2_l();
        assert_eq!(it.size_hint(), (15, Some(15)));
        it.next(); it.next();
        assert_eq!(it.size_hint(), (13, Some(13)));
        let mut it = img.iter_div3_l();
        assert_eq!(it.size_hint(), (6, Some(6)));
        it.next();
        assert_eq!(it.size_hint(), (5, Some(5)));
        let it = img.iter_div4_l();
        assert_eq!(it.size_hint(), (2, Some(2)));
    }
}

fn calc_gradient(source: &dyn GradientCalcSource) -> Option<Plane> {
    let width = source.image_width();
    let height = source.image_height();
    let min_size = usize::min(width, height);
    let cell_size = min_size / 30;
    if cell_size <= 16 {
        return None;
    }

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
        let avg = middle.iter().map(|v| *v as f64).sum::<f64>() / middle.len() as f64;

        points.push(Point3D {
            x: x as f64,
            y: y as f64,
            z: avg,
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
