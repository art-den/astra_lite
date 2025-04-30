use itertools::izip;

use crate::utils::math::median5;

use super::raw::*;

pub struct RawStacker {
    data:       Vec<u32>,
    images:     Vec<RawImage>,
    info:       Option<RawImageInfo>,
    counter:    u32,
    zero_sum:   i32,
    integr_exp: f64,
}

impl RawStacker {
    pub fn new() -> Self {
        Self {
            data:       Vec::new(),
            images:     Vec::new(),
            info:       None,
            counter:    0,
            zero_sum:   0,
            integr_exp: 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.data.shrink_to_fit();
        self.images.clear();
        self.images.shrink_to_fit();
        self.info = None;
        self.counter = 0;
        self.zero_sum = 0;
        self.integr_exp = 0.0;
    }

    pub fn add(
        &mut self,
        raw:        &RawImage,
        use_median: bool
    ) -> anyhow::Result<()> {
        let raw_info = raw.info();
        if let Some(info) = &self.info {
            if info.width != raw_info.width
            || info.height != raw_info.height {
                anyhow::bail!(
                    "Size of images differ: stacker {}x{}, raw {}x{}",
                    info.width, info.height,
                    raw_info.width, raw_info.height,
                );
            }
            if info.cfa != raw_info.cfa {
                anyhow::bail!("CFA of images differ");
            }
            if info.frame_type != raw_info.frame_type {
                anyhow::bail!("Frame type of images differ");
            }
            if self.data.len() != raw.as_slice().len() {
                anyhow::bail!("Internal error: self.data.len() != raw.data.len()");
            }
        } else {
            self.info = Some(raw_info.clone());
            self.counter = 0;
            self.zero_sum = 0;
            self.data.resize(raw.as_slice().len(), 0);
        }
        if use_median {
            if self.images.len() == 4 {
                let raw1 = self.images[0].as_slice();
                let raw2 = self.images[1].as_slice();
                let raw3 = self.images[2].as_slice();
                let raw4 = self.images[3].as_slice();

                for (s1, s2, s3, s4, s5, d)
                in izip!(raw1, raw2, raw3, raw4, raw.as_slice(), &mut self.data) {
                    *d += median5(*s1, *s2, *s3, *s4, *s5) as u32;
                }
                self.counter += 1;
                self.zero_sum += raw_info.offset;
                self.images.clear();
                self.images.shrink_to_fit();
            } else  {
                self.images.push(raw.clone());
            }
        } else {
            for (s, d) in izip!(raw.as_slice(), &mut self.data) {
                *d += *s as u32;
            }
            self.counter += 1;
            self.zero_sum += raw_info.offset;
        }
        self.integr_exp += raw_info.exposure;
        Ok(())
    }

    pub fn get(&mut self) -> anyhow::Result<RawImage> {
        let Some(info) = &self.info else {
            anyhow::bail!("Raw added is empty");
        };

        let cfa_arr = info.cfa.get_array();
        let mut info = info.clone();
        let counter2 = self.counter/2;
        info.offset = (self.zero_sum + counter2 as i32) / self.counter as i32;
        info.integr_time = Some(self.integr_exp);

        if self.counter == 0 && !self.images.is_empty() {
            // Median is used but less then 3 images are added.
            // Just use mean
            let mut iterators: Vec<_> = self.images
                .iter()
                .map(|image| image.as_slice().iter())
                .collect();
            let mut data = Vec::new();
            let count = self.images.len() as u32;
            let count2 = count / 2;
            loop {
                let mut sum = 0u32;
                let mut ok = false;
                for iter in &mut iterators {
                    if let Some(v) = iter.next() {
                        sum += *v as u32;
                        ok = true;
                    } else {
                        break;
                    }
                }
                if ok {
                    data.push(((sum + count2) / count) as u16);
                } else {
                    break;
                }
            }
            Ok(RawImage::new(info, data, cfa_arr))
        } else {
            let data: Vec<_> = self.data
                .iter()
                .map(|v| ((*v + counter2) / self.counter) as u16)
                .collect();
            Ok(RawImage::new(info, data, cfa_arr))
        }
    }
}
