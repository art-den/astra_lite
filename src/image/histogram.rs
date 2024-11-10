use std::sync::Mutex;
use itertools::*;
use super::{raw::*, image::*};

#[derive(Clone)]
pub struct HistogramChan {
    pub mean:    f32,
    pub std_dev: f32,
    pub count:   usize,
    pub freq:    Vec<u32>,
}

impl HistogramChan {
    pub fn new() -> Self {
        Self {
            mean: 0.0,
            std_dev: 0.0,
            count: 0,
            freq: Vec::new(),
        }
    }

    fn take_from_freq(&mut self, freq: Vec<u32>, max_value: u16) {
        self.freq = freq;
        self.freq.resize(max_value as usize + 1, 0);
        let total_cnt = self.freq.iter().sum::<u32>() as usize;
        let total_sum = self.freq.iter().enumerate().map(|(i, v)| { i * *v as usize }).sum::<usize>();
        self.count = total_cnt as usize;
        let mean = total_sum as f64 / total_cnt as f64;
        self.mean = mean as f32;
        let mut sum = 0_f64;
        for (value, cnt) in self.freq.iter().enumerate() {
            let diff = mean - value as f64;
            sum += *cnt as f64 * diff * diff;
        }
        self.std_dev = f64::sqrt(sum / total_cnt as f64) as f32;
    }

    pub fn get_nth_element(&self, mut n: usize) -> u16 {
        for (idx, v) in self.freq.iter().enumerate() {
            if n < *v as usize {
                return idx as u16;
            }
            n -= *v as usize;
        }
        u16::MAX
    }

    pub fn median(&self) -> u16 {
        self.get_nth_element(self.count/2)
    }

    pub fn get_percentile(&self, n: usize) -> u16 {
        self.get_nth_element((n * self.count + 50) / 100)
    }
}

#[derive(Clone)]
pub struct Histogram {
    pub max: u16,
    pub r:   Option<HistogramChan>,
    pub g:   Option<HistogramChan>,
    pub b:   Option<HistogramChan>,
    pub l:   Option<HistogramChan>,
}

impl Histogram {
    pub fn new() -> Self {
        Self { max: 0, r: None, g: None, b: None, l: None }
    }

    pub fn from_raw_image(
        &mut self,
        img:        &RawImage,
        monochrome: bool,
    ) {
        let img_max_value = img.info().max_value;
        self.max = img_max_value;
        if img.info().cfa == CfaType::None || monochrome {
            let tmp = Self::tmp_from_slice(img.as_slice(), 1);
            let mut l = self.l.take().unwrap_or(HistogramChan::new());
            l.take_from_freq(tmp.0, img_max_value);
            self.l = Some(l);
            self.r = None;
            self.g = None;
            self.b = None;
        } else {
            let tmp = Mutex::new(Vec::<(TmpFreqValues, TmpFreqValues, TmpFreqValues)>::new());
            let img_height = img.info().height;
            let process_range = |y1, y2| {
                let mut r = [0u32; u16::MAX as usize + 1];
                let mut g = [0u32; u16::MAX as usize + 1];
                let mut b = [0u32; u16::MAX as usize + 1];
                for y in y1..y2 {
                    let cfa = img.cfa_row(y);
                    let row_data = img.row(y);
                    for (v, c) in izip!(row_data, cfa.iter().cycle()) {
                        match *c {
                            CfaColor::R => r[*v as usize] += 1,
                            CfaColor::G => g[*v as usize] += 1,
                            CfaColor::B => b[*v as usize] += 1,
                            _           => {},
                        }
                    }
                }
                tmp.lock().unwrap().push((
                    TmpFreqValues::new_from_slice(&r),
                    TmpFreqValues::new_from_slice(&g),
                    TmpFreqValues::new_from_slice(&b)
                ));
            };

            // map
            let max_threads = rayon::current_num_threads();
            let tasks_cnt = if max_threads != 1 { 2 * max_threads  } else { 1 };
            rayon::scope(|s| {
                for t in 0..tasks_cnt {
                    let y1 = t * img_height / tasks_cnt;
                    let y2 = (t+1) * img_height / tasks_cnt;
                    s.spawn(move |_| process_range(y1, y2));
                }
            });

            // reduce
            let mut r_res = TmpFreqValues::new();
            let mut g_res = TmpFreqValues::new();
            let mut b_res = TmpFreqValues::new();
            for (r, g, b) in tmp.lock().unwrap().iter() {
                r_res.append(r);
                g_res.append(g);
                b_res.append(b);
            }

            let mut r = self.r.take().unwrap_or(HistogramChan::new());
            let mut g = self.g.take().unwrap_or(HistogramChan::new());
            let mut b = self.b.take().unwrap_or(HistogramChan::new());

            r.take_from_freq(r_res.0, img_max_value);
            g.take_from_freq(g_res.0, img_max_value);
            b.take_from_freq(b_res.0, img_max_value);

            self.l = None;
            self.r = Some(r);
            self.g = Some(g);
            self.b = Some(b);
        }
    }

    fn tmp_from_slice(data: &[u16], step: usize) -> TmpFreqValues {
        let tmp = Mutex::new(Vec::<TmpFreqValues>::new());
        let process_sub_slice = |from: usize, to: usize| {
            let sub_slice = &data[from..to];
            let mut res = [0u32; u16::MAX as usize + 1];
            if step == 1 {
                for v in sub_slice.iter() {
                    res[*v as usize] += 1;
                }
            } else {
                for v in sub_slice.iter().step_by(step) {
                    res[*v as usize] += 1;
                }
            }
            tmp.lock().unwrap().push(
                TmpFreqValues::new_from_slice(&res)
            );
        };

        // map
        let size = data.len();
        let max_threads = rayon::current_num_threads();
        let tasks_cnt = if max_threads != 1 { 2 * max_threads  } else { 1 };
        rayon::scope(|s| {
            for t in 0..tasks_cnt {
                let from = t * size / tasks_cnt;
                let to = (t+1) * size / tasks_cnt;
                s.spawn(move |_| process_sub_slice(from, to));
            }
        });

        // reduce
        let mut res = TmpFreqValues::new();
        for t in tmp.lock().unwrap().iter() {
            res.append(t);
        }
        res
    }

    pub fn from_image(&mut self, img: &Image) {
        let from_image_layer = |
            chan:  Option<HistogramChan>,
            layer: &ImageLayer<u16>,
        | -> Option<HistogramChan> {
            if layer.is_empty() { return None; }
            let mut chan = chan.unwrap_or(HistogramChan::new());
            let slice = layer.as_slice();
            let step = (slice.len() / 3_000_000) | 1;
            let tmp = Self::tmp_from_slice(slice, step);
            chan.take_from_freq(tmp.0, img.max_value());
            Some(chan)
        };
        self.max = img.max_value();
        self.l = from_image_layer(self.l.take(), &img.l);
        self.r = from_image_layer(self.r.take(), &img.r);
        self.g = from_image_layer(self.g.take(), &img.g);
        self.b = from_image_layer(self.b.take(), &img.b);
    }

    pub fn clear(&mut self) {
        self.l = None;
        self.r = None;
        self.g = None;
        self.b = None;
    }
}

struct TmpFreqValues(Vec<u32>);

impl TmpFreqValues {
    fn new_from_slice(freq_data: &[u32]) -> Self {
        let mut freq = Vec::new();
        freq.extend_from_slice(freq_data);
        Self(freq)
    }

    fn new() -> Self {
        let mut freq = Vec::new();
        freq.resize(u16::MAX as usize + 1, 0);
        Self(freq)
    }

    fn append(&mut self, other: &TmpFreqValues) {
        for (s, d) in izip!(&other.0, &mut self.0) { *d += *s; }
    }
}
