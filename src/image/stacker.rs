use std::{fs::File, io::BufWriter, path::Path};

use itertools::*;
use rayon::prelude::*;

use crate::utils::math::*;

use super::{histogram::*, image::*, raw::RawImageInfo};

/// Channel of rotated image
#[derive(Default)]
struct StackerTempChan {
    data: Vec<u16>,
    background: u16,
}

#[derive(Default)]
struct StackerChan {
    data: Vec<i32>,
    tmp: Vec<StackerTempChan>,
}

impl StackerChan {
    fn clear(&mut self) {
        self.data.clear();
        self.data.shrink_to_fit();
        self.tmp.clear();
        self.tmp.shrink_to_fit();
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty() && self.tmp.is_empty()
    }

    fn get(&self, dest: &mut [u16], from: usize, to: usize, cnt: &[u16]) {
        if self.tmp.len() == 1 {
            dest.copy_from_slice(&self.tmp[0].data[from..to]);
        } else if self.tmp.len() == 2 {
            let src1 = &self.tmp[0].data[from..to];
            let src2 = &self.tmp[1].data[from..to];
            for (d, s1, s2) in izip!(dest, src1, src2) {
                let mut div = 0;
                if *s1 != 0 { div += 1; }
                if *s2 != 0 { div += 1; }
                if div == 0 { div = 1; }
                *d = ((*s1 as u32 + *s2 as u32) / div) as u16;
            }
        } else if self.tmp.len() == 3 {
            let src1 = &self.tmp[0].data[from..to];
            let src2 = &self.tmp[1].data[from..to];
            let src3 = &self.tmp[2].data[from..to];
            for (d, s1, s2, s3) in izip!(dest, src1, src2, src3) {
                let mut div = 0;
                if *s1 != 0 { div += 1; }
                if *s2 != 0 { div += 1; }
                if *s3 != 0 { div += 1; }
                if div == 0 { div = 1; }
                *d = ((*s1 as u32 + *s2 as u32 + *s3 as u32) / div) as u16;
            }
        } else if self.tmp.len() == 4 {
            let src1 = &self.tmp[0].data[from..to];
            let src2 = &self.tmp[1].data[from..to];
            let src3 = &self.tmp[2].data[from..to];
            let src4 = &self.tmp[3].data[from..to];
            for (d, s1, s2, s3, s4) in izip!(dest, src1, src2, src3, src4) {
                let mut div = 0;
                if *s1 != 0 { div += 1; }
                if *s2 != 0 { div += 1; }
                if *s3 != 0 { div += 1; }
                if *s4 != 0 { div += 1; }
                if div == 0 { div = 1; }
                *d = ((*s1 as u32 + *s2 as u32 + *s3 as u32 + *s4 as u32) / div) as u16;
            }
        } else {
            let data = &self.data[from..to];
            let cnt = &cnt[from..to];
            for (d, s, c) in izip!(dest, data, cnt) {
                let mut value = if *c != 0 { *s / *c as i32 } else { 0 };
                if value < 0 { value = 0; }
                if value > u16::MAX as i32 { value = u16::MAX as i32; }
                *d = value as u16;
            }
        }
    }
}

pub struct Stacker {
    r: StackerChan,
    g: StackerChan,
    b: StackerChan,
    l: StackerChan,
    cnt: Vec<u16>,
    tmp_idx: usize,
    width: usize,
    height: usize,
    max_value: u16,
    total_exp: f64,
    frames_cnt: u32,
    no_tracks: bool,
    raw_info: Option<RawImageInfo>,
}

impl Stacker {
    pub fn new() -> Self {
        Self {
            r: StackerChan::default(),
            g: StackerChan::default(),
            b: StackerChan::default(),
            l: StackerChan::default(),
            cnt: Vec::new(),
            tmp_idx: 0,
            width: 0,
            height: 0,
            max_value: 0,
            total_exp: 0.0,
            frames_cnt: 0,
            no_tracks: false,
            raw_info: None,
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
        self.l.clear();
        self.r.clear();
        self.g.clear();
        self.b.clear();
        self.cnt.clear();
        self.cnt.shrink_to_fit();
        self.tmp_idx = 0;
        self.width = 0;
        self.height = 0;
        self.max_value = 0;
        self.total_exp = 0.0;
        self.frames_cnt = 0;
    }

    pub fn add(
        &mut self,
        image:     &Image,
        hist:      &Histogram,
        transl_x:  f64,
        transl_y:  f64,
        angle:     f64,
        exposure:  f64,
        no_tracks: bool,
    ) {
        debug_assert!(!image.is_empty());
        if self.is_empty() {
            let data_len = image.width() * image.height();
            if image.is_color() {
                self.r.data.resize(data_len, 0);
                self.g.data.resize(data_len, 0);
                self.b.data.resize(data_len, 0);
            } else {
                self.l.data.resize(data_len, 0);
            }
            self.cnt.resize(data_len, 0);
            self.width = image.width();
            self.height = image.height();
            self.max_value = image.max_value();
            self.no_tracks = no_tracks;
            self.raw_info = image.raw_info.clone();
        }
        if !self.no_tracks {
            self.add_simple(image, transl_x, transl_y, angle);
        } else {
            self.add_no_tracks(image, hist, transl_x, transl_y, angle);
        }
        self.total_exp += exposure;
        self.frames_cnt += 1;
    }

    fn add_simple(
        &mut self,
        image:    &Image,
        transl_x: f64,
        transl_y: f64,
        angle:    f64,
    ) {
        Self::add_layer(&mut self.r, &mut self.cnt, &image.r, transl_x, transl_y, angle, false);
        Self::add_layer(&mut self.g, &mut self.cnt, &image.g, transl_x, transl_y, angle, false);
        Self::add_layer(&mut self.b, &mut self.cnt, &image.b, transl_x, transl_y, angle, true);
        Self::add_layer(&mut self.l, &mut self.cnt, &image.l, transl_x, transl_y, angle, true);
    }

    fn add_layer(
        dst:        &mut StackerChan,
        cnt:        &mut [u16],
        src:        &ImageLayer<u16>,
        transl_x:   f64,
        transl_y:   f64,
        angle:      f64,
        update_cnt: bool
    ) {
        if src.is_empty() {
            return;
        }

        const K: i64 = 65536;

        fn f64_to_i64(value: f64) -> i64 {
            f64::round(value * K as f64) as i64
        }

        let cos_a = f64_to_i64(f64::cos(-angle));
        let sin_a = f64_to_i64(f64::sin(-angle));

        fn crd_to_i64(value: f64) -> i64 {
            f64::round(value * CRD_DIV as f64) as i64
        }

        let center_x = crd_to_i64((src.width() as f64 - 1.0) / 2.0);
        let center_y = crd_to_i64((src.height() as f64 - 1.0) / 2.0);
        let transl_x = crd_to_i64(transl_x);
        let transl_y = crd_to_i64(transl_y);

        dst.data.par_chunks_exact_mut(src.width())
            .zip(cnt.par_chunks_exact_mut(src.width()))
            .enumerate()
            .for_each(|(y, (dst_row, cnt_row))| {
                let y = y as i64 * CRD_DIV - transl_y;
                let dy = y - center_y;
                let mut x = -transl_x;
                for (dst_v, cnt_v) in dst_row.iter_mut().zip(cnt_row) {
                    let dx = x - center_x;
                    let rot_x = center_x + (dx * cos_a - dy * sin_a) / K;
                    let rot_y = center_y + (dy * cos_a + dx * sin_a) / K;
                    let src_v = src.get_crd_i64(rot_x, rot_y);
                    if let Some(v) = src_v {
                        *dst_v += v as i32;
                        if update_cnt { *cnt_v += 1; }
                    }
                    x += CRD_DIV;
                }
            });
    }

    pub fn add_no_tracks(
        &mut self,
        image:    &Image,
        hist:     &Histogram,
        transl_x: f64,
        transl_y: f64,
        angle:    f64,
    ) {
        let background_r = hist.r.as_ref().map(|chan| chan.median()).unwrap_or(0);
        let background_g = hist.g.as_ref().map(|chan| chan.median()).unwrap_or(0);
        let background_b = hist.b.as_ref().map(|chan| chan.median()).unwrap_or(0);
        let background_l = hist.l.as_ref().map(|chan| chan.median()).unwrap_or(0);

        let idx = self.tmp_idx % 5;

        Self::rotate_image(idx, &mut self.r, &image.r, background_r, transl_x, transl_y, angle);
        Self::rotate_image(idx, &mut self.g, &image.g, background_g, transl_x, transl_y, angle);
        Self::rotate_image(idx, &mut self.b, &image.b, background_b, transl_x, transl_y, angle);
        Self::rotate_image(idx, &mut self.l, &image.l, background_l, transl_x, transl_y, angle);

        self.tmp_idx += 1;

        if self.tmp_idx >= 5 {
            Self::add_median(&mut self.r, &mut self.cnt, false);
            Self::add_median(&mut self.g, &mut self.cnt, false);
            Self::add_median(&mut self.b, &mut self.cnt, true);
            Self::add_median(&mut self.l, &mut self.cnt, true);
        }
    }

    fn rotate_image(
        idx:        usize,
        dst_chan:   &mut StackerChan,
        src:        &ImageLayer<u16>,
        background: u16,
        transl_x:   f64,
        transl_y:   f64,
        angle:      f64,
    ) {
        if src.is_empty() { return; }
        while dst_chan.tmp.len() <= idx {
            dst_chan.tmp.push(StackerTempChan::default());
        }
        let tmp = &mut dst_chan.tmp[idx];
        tmp.background = background;
        if tmp.data.is_empty() {
            tmp.data.resize(src.width() * src.height(), 0);
        }

        const K: i64 = 65536;

        fn f64_to_i64(value: f64) -> i64 {
            f64::round(value * K as f64) as i64
        }

        let cos_a = f64_to_i64(f64::cos(-angle));
        let sin_a = f64_to_i64(f64::sin(-angle));

        fn crd_to_i64(value: f64) -> i64 {
            f64::round(value * CRD_DIV as f64) as i64
        }

        let center_x = crd_to_i64((src.width() as f64 - 1.0) / 2.0);
        let center_y = crd_to_i64((src.height() as f64 - 1.0) / 2.0);
        let transl_x = crd_to_i64(transl_x);
        let transl_y = crd_to_i64(transl_y);

        tmp.data.par_chunks_exact_mut(src.width())
            .enumerate()
            .for_each(|(y, dst_row)| {
                let y = y as i64 * CRD_DIV - transl_y;
                let dy = y - center_y;
                let mut x = -transl_x;
                for dst_v in dst_row {
                    let dx = x - center_x;
                    let rot_x = center_x + (dx * cos_a - dy * sin_a) / K;
                    let rot_y = center_y + (dy * cos_a + dx * sin_a) / K;
                    let src_v = src.get_crd_i64(rot_x, rot_y);
                    *dst_v = if let Some(mut v) = src_v {
                        if v == 0 { v = 1; }
                        v
                    } else {
                        0
                    };
                    x += CRD_DIV;
                }
            });
    }

    fn add_median(
        chan: &mut StackerChan,
        cnt: &mut Vec<u16>,
        update_cnt: bool,
    ) {
        if chan.tmp.is_empty() { return; }
        let src1 = &chan.tmp[0];
        let src2 = &chan.tmp[1];
        let src3 = &chan.tmp[2];
        let src4 = &chan.tmp[3];
        let src5 = &chan.tmp[4];

        if chan.data.is_empty() {
            chan.data.resize(src1.data.len(), 0);
        }

        let common_background = median5(
            src1.background,
            src2.background,
            src3.background,
            src4.background,
            src5.background,
        ) as i32;

        src1.data.par_iter()
            .zip(src2.data.par_iter())
            .zip(src3.data.par_iter())
            .zip(src4.data.par_iter())
            .zip(src5.data.par_iter())
            .zip(chan.data.par_iter_mut())
            .zip(cnt.par_iter_mut())
            .for_each(|((((((r1, r2), r3), r4), r5), d), c)| {
                let mut result =
                    if *r1 != 0 && *r2 != 0 && *r3 != 0 && *r4 != 0 && *r5 != 0 {
                        median5(
                            *r1 as i32 - src1.background as i32,
                            *r2 as i32 - src2.background as i32,
                            *r3 as i32 - src3.background as i32,
                            *r4 as i32 - src4.background as i32,
                            *r5 as i32 - src5.background as i32,
                        )
                    } else {
                        let mut tmp = [0_i32; 4];
                        let mut idx = 0;
                        if *r1 != 0 { tmp[idx] = *r1 as i32 - src1.background as i32; idx += 1; }
                        if *r2 != 0 { tmp[idx] = *r2 as i32 - src2.background as i32; idx += 1; }
                        if *r3 != 0 { tmp[idx] = *r3 as i32 - src3.background as i32; idx += 1; }
                        if *r4 != 0 { tmp[idx] = *r4 as i32 - src4.background as i32; idx += 1; }
                        if *r5 != 0 { tmp[idx] = *r5 as i32 - src5.background as i32; idx += 1; }
                        match idx {
                            0 => 0,
                            1 => tmp[0],
                            2 => (tmp[0] + tmp[1]) / 2,
                            3 => median3(tmp[0], tmp[1], tmp[2]),
                            4 => median4(tmp[0], tmp[1], tmp[2], tmp[3]),
                            _ => unreachable!(),
                        }
                    };
                if result != i32::MIN {
                    result += common_background;
                    if result < 0 { result = 0; }
                    *d += result;
                    if update_cnt { *c += 1; }
                }
            });

    }

    pub fn save_to_tiff(&self, file_name: &Path) -> anyhow::Result<()> {
        use tiff::encoder::*;
        let mut file = BufWriter::new(File::create(file_name)?);
        let mut decoder = TiffEncoder::new(&mut file)?;
        if !self.l.data.is_empty() {
            let mut tiff = decoder.new_image::<colortype::Gray16>(
                self.width as u32,
                self.height as u32
            )?;
            tiff.rows_per_strip(64)?;
            let mut values = Vec::new();
            let mut pos = 0_usize;

            loop {
                let samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }

                let from = pos;
                let to = pos + samples_count;
                values.resize(samples_count, 0);
                self.l.get(&mut values, from, to, &self.cnt);

                tiff.write_strip(&values)?;
                pos += samples_count;
            }
            tiff.finish()?;
        } else {
            let mut tiff = decoder.new_image::<colortype::RGB16>(
                self.width as u32,
                self.height as u32
            )?;
            let mut strip_data = Vec::new();
            let mut r_values = Vec::new();
            let mut g_values = Vec::new();
            let mut b_values = Vec::new();
            tiff.rows_per_strip(64)?;
            let mut pos = 0_usize;
            loop {
                let mut samples_count = tiff.next_strip_sample_count() as usize;
                if samples_count == 0 { break; }
                samples_count /= 3;

                let from = pos;
                let to = pos + samples_count;
                r_values.resize(samples_count, 0);
                self.r.get(&mut r_values, from, to, &self.cnt);
                g_values.resize(samples_count, 0);
                self.g.get(&mut g_values, from, to, &self.cnt);
                b_values.resize(samples_count, 0);
                self.b.get(&mut b_values, from, to, &self.cnt);

                strip_data.clear();
                for (r, g, b) in izip!(&r_values, &g_values, &b_values) {
                    strip_data.push(*r);
                    strip_data.push(*g);
                    strip_data.push(*b);
                }
                tiff.write_strip(&strip_data)?;
                pos += samples_count;
            }
            tiff.finish()?;
        }
        Ok(())
    }

    pub fn total_exposure(&self) -> f64 {
        self.total_exp
    }

    pub fn copy_to_image(&self, image: &mut Image) {
        let copy_layer = |chan: &StackerChan, dst: &mut ImageLayer<u16>| {
            if chan.is_empty() { return; }
            dst.resize(self.width, self.height);
            dst.as_slice_mut()
                .par_chunks_exact_mut(self.width)
                .enumerate()
                .for_each(|(y, row)| {
                    let from = y * self.width;
                    let to = (y + 1) * self.width;
                    chan.get(row, from, to, &self.cnt);
                });
        };

        copy_layer(&self.r, &mut image.r);
        copy_layer(&self.g, &mut image.g);
        copy_layer(&self.b, &mut image.b);
        copy_layer(&self.l, &mut image.l);

        image.set_max_value(self.max_value);
        image.raw_info = self.raw_info.clone();
    }
}
