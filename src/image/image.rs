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

    pub fn coord_iter(&self) -> CoordIterator<T> {
        CoordIterator::<T> {
            x: 0,
            y: 0,
            width: self.width,
            iter: self.data.iter(),
        }
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
    fn init_iter(img: &ImageLayer<T>, x1: usize, x2: usize, y: usize) -> std::slice::Iter<T> {
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