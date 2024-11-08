use gtk::{gdk::{cairo, gdk_pixbuf}, prelude::*};
use itertools::Itertools;
use rayon::prelude::*;
use crate::utils::math::Mat3;

pub struct PerspectivePainter {
    data: Vec::<u8>,
}

impl PerspectivePainter {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
        }
    }

    pub fn paint(
        &mut self,
        cairo: &cairo::Context,
        image: &gdk_pixbuf::Pixbuf,
        x0: i32, y0: i32,
        x1: i32, y1: i32,
        x2: i32, y2: i32,
        x3: i32, y3: i32,
    ) -> anyhow::Result<()> {
        let mut min_x = x0.min(x1).min(x2).min(x3);
        let mut max_x = x0.max(x1).max(x2).max(x3);
        let mut min_y = y0.min(y1).min(y2).min(y3);
        let mut max_y = y0.max(y1).max(y2).max(y3);
        if let Some(clip) = cairo.clip_rectangle() {
            let right = clip.x() + clip.width() - 1;
            let bottom = clip.y() + clip.height() - 1;
            if max_x < clip.x() || min_x > right
            || max_y < clip.y() || min_y > bottom {
                return Ok(());
            }

            min_x = min_x.max(clip.x());
            max_x = max_x.min(right);
            min_y = min_y.max(clip.y());
            max_y = max_y.min(bottom);
        }
        let width = (max_x - min_x + 1) as usize;
        let height = (max_y - min_y + 1) as usize;
        if width <= 1 || height <= 1 {
            return Ok(());
        }

        self.data.resize(4 * width * height, 0);
        let src_width = image.width();
        let src_height = image.height();
        let src_rowstride = image.rowstride();
        let src_pix_len = if image.has_alpha() { 4 } else { 3 };
        let src_bytes = image.read_pixel_bytes();
        let mat = Self::calc_matrix(
            x0 as _, y0 as _,
            x1 as _, y1 as _,
            x2 as _, y2 as _,
            x3 as _, y3 as _,
            src_width as _,
            src_height as _,
        );
        let a11 = mat.a11 as f32;
        let a12 = mat.a12 as f32;
        let a13 = mat.a13 as f32;
        let a21 = mat.a21 as f32;
        let a22 = mat.a22 as f32;
        let a23 = mat.a23 as f32;
        let a31 = mat.a31 as f32;
        let a32 = mat.a32 as f32;
        let a33 = mat.a33 as f32;

        let process_row_fun = |(y, row) : (usize, &mut [u8])| {
            let y = (y + min_y as usize) as f32;
            for ((r, g, b, a), x) in row.iter_mut().tuples().zip(min_x..) {
                let x = x as f32;
                let mut sx = a11 * x + a21 * y + a31;
                let mut sy = a12 * x + a22 * y + a32;
                let     z  = a13 * x + a23 * y + a33;
                let d_div = 1.0 / z;
                sx *= d_div;
                sy *= d_div;
                let sx = sx as i32;
                if sx < 0 || sx >= src_width {
                    *a = 0;
                    continue;
                }
                let sy = sy as i32;
                if sy < 0 || sy >= src_height {
                    *a = 0;
                    continue;
                }
                let src_ptr = (sx * src_pix_len + sy * src_rowstride) as usize;
                if let [src_r, src_g, src_b] = &src_bytes[src_ptr..src_ptr+3] {
                    *r = *src_r;
                    *g = *src_g;
                    *b = *src_b;
                    *a = 255;
                }
            }
        };

        if width * height > 100_000 {
            self.data
                .par_chunks_exact_mut(4 * width)
                .enumerate()
                .for_each(process_row_fun);
        } else {
            self.data
                .chunks_exact_mut(4 * width)
                .enumerate()
                .for_each(process_row_fun);
        }

        let pixbuf = gdk_pixbuf::Pixbuf::from_mut_slice(
            &mut self.data,
            gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            width as i32,
            height as i32,
            (width * 4) as i32
        );

        cairo.set_source_pixbuf(&pixbuf, min_x as f64, min_y as f64);
        cairo.paint()?;

        Ok(())
    }

    fn calc_matrix(
        x0: f64, y0: f64,
        x1: f64, y1: f64,
        x2: f64, y2: f64,
        x3: f64, y3: f64,
        width: f64,
        height: f64,
    ) -> Mat3 {
        let dx1 = x1 - x2;
        let dx2 = x3 - x2;
        let dx3 = x0 - x1 + x2 - x3;
        let dy1 = y1 - y2;
        let dy2 = y3 - y2;
        let dy3 = y0 - y1 + y2 - y3;

        let a13 = (dx3 * dy2 - dy3 * dx2) / (dx1 * dy2 - dy1 * dx2);
        let a23 = (dx1 * dy3 - dy1 * dx3) / (dx1 * dy2 - dy1 * dx2);
        let a11 = x1 - x0 + a13 * x1;
        let a21 = x3 - x0 + a23 * x3;
        let a31 = x0;
        let a12 = y1 - y0 + a13 * y1;
        let a22 = y3 - y0 + a23 * y3;
        let a32 = y0;

        let mat = Mat3::new(
            a11, a12, a13,
            a21, a22, a23,
            a31, a32, 1.0
        );

        let mut inv = mat.inv();

        inv.a11 *= width;
        inv.a21 *= width;
        inv.a31 *= width;

        inv.a12 *= height;
        inv.a22 *= height;
        inv.a32 *= height;

        inv
    }
}
