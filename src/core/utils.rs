use std::{ops::RangeInclusive, path::{Path, PathBuf}, sync::Arc};

use chrono::{DateTime, Utc};

use crate::{
    hal::{Camera, FrameType}, image::raw::*, options::*, sky_math::math::*
};

pub enum FileNameArg<'a> {
    Options(&'a CamOptions),
    RawInfo {
        info:     &'a RawImageInfo,
        ccd_temp: Option<f64>,
    },
}

impl FileNameArg<'_> {
    pub fn exposure(&self) -> f64 {
        match self {
            Self::Options(opts) => opts.frame.exposure(),
            Self::RawInfo{info, ..} => info.exposure,
        }
    }

    pub fn frame_type(&self) -> FrameType {
        match self {
            Self::Options(opts) => opts.frame.frame_type,
            Self::RawInfo{info, ..} => info.frame_type,
        }
    }
}

#[derive(Default)]
pub struct FileNameUtils {
    camera_id: String,
    sensor_width: usize,
    sensor_height: usize,
    cooler_supported: bool,
}

impl FileNameUtils {
    pub fn init(&mut self, camera: &Arc<dyn Camera>) {
        self.camera_id = camera.id().to_string();

        let (sensor_width, sensor_height) = camera.ccd_size().unwrap_or((0, 0));
        self.cooler_supported = camera.is_cooler_supported().unwrap_or(false);

        self.sensor_width = sensor_width;
        self.sensor_height = sensor_height;
    }

    pub fn master_only_file_name(
        &self,
        date:              Option<DateTime<Utc>>, // used for flat frames
        to_calibrate:      &FileNameArg,
        master_frame_type: FrameType,
    ) -> String {
        match to_calibrate {
            FileNameArg::Options(opts) => {
                let (img_width, img_height) = opts.frame.active_sensor_size(
                    self.sensor_width,
                    self.sensor_height,
                );
                let temperature = if self.cooler_supported && opts.ctrl.enable_cooler {
                    Some(opts.ctrl.temperature)
                } else {
                    None
                };
                Self::master_file_name_impl(
                    date,
                    master_frame_type,
                    opts.frame.exposure(),
                    opts.frame.gain as i32,
                    opts.frame.offset,
                    img_width,
                    img_height,
                    opts.frame.binning.get_ratio() as i32,
                    temperature
                )
            }
            FileNameArg::RawInfo{info, ccd_temp} => {
                Self::master_file_name_impl(
                    info.time,
                    master_frame_type,
                    info.exposure,
                    info.gain,
                    info.offset,
                    info.width,
                    info.height,
                    info.bin as i32,
                    ccd_temp.or(info.ccd_temp)
                )
            },
        }
    }

    pub fn master_file_name(
        &self,
        to_calibrate:      &FileNameArg,
        dark_library_path: &Path,
        master_frame_type: FrameType
    ) -> PathBuf {
        let mut path = PathBuf::new();
        let cam_name = if let FileNameArg::RawInfo{info, ..} = to_calibrate {
            info.camera.clone()
        } else {
            self.camera_id.clone()
        };
        path.push(dark_library_path);
        path.push(&cam_name);
        path.push(self.master_only_file_name(None, to_calibrate, master_frame_type));
        path
    }

    pub fn defect_pixels_file_name(
        &self,
        args:              &FileNameArg,
        dark_library_path: &Path
    ) -> PathBuf {
        let (defect_pixels_file_name, camera) = match args {
            FileNameArg::Options(opts) => {
                let (img_width, img_height) = opts.frame.active_sensor_size(
                    self.sensor_width,
                    self.sensor_height,
                );
                let file_name = Self::defect_pixels_file_name_impl(
                    img_width, img_height,
                    opts.frame.binning.get_ratio() as i32,
                );

                (file_name, &self.camera_id)
            }
            FileNameArg::RawInfo{info, ..} => {
                let file_name = Self::defect_pixels_file_name_impl(
                    info.width, info.height,
                    info.bin as i32,
                );
                (file_name, &info.camera)
            }
        };

        let mut path = PathBuf::new();
        path.push(dark_library_path);
        path.push(camera);
        path.push(&defect_pixels_file_name);
        path
    }

    pub fn raw_file_dest_dir(
        &self,
        date:        DateTime<Utc>, // used for flat frames
        cam_options: &CamOptions,
    ) -> String {
        let (img_width, img_height) = cam_options.frame.active_sensor_size(
            self.sensor_width,
            self.sensor_height,
        );
        let temperature = if self.cooler_supported && cam_options.ctrl.enable_cooler {
            Some(cam_options.ctrl.temperature)
        } else {
            None
        };
        Self::raw_directory_name_impl(
            date,
            cam_options.frame.frame_type,
            cam_options.frame.exposure(),
            cam_options.frame.gain as i32,
            cam_options.frame.offset,
            img_width,
            img_height,
            cam_options.frame.binning.get_ratio() as i32,
            temperature,
        )
    }

    pub fn get_subtrack_master_fname(
        self:          &FileNameUtils,
        to_calibrate:  &FileNameArg,
        dark_lib_path: &Path
    ) -> (PathBuf, CalibrMethods) {
        let is_flat_file = to_calibrate.frame_type() == FrameType::Flats;
        let (frame_type, master_calibr_method)  =
            if is_flat_file && to_calibrate.exposure() < 1.0 {
                (FrameType::Biases, CalibrMethods::BY_BIAS)
            } else {
                (FrameType::Darks, CalibrMethods::BY_DARK)
            };
        let master_fname = self.master_file_name(
            to_calibrate,
            dark_lib_path,
            frame_type
        );
        (master_fname, master_calibr_method)
    }

    fn master_file_name_impl(
        date:        Option<DateTime<Utc>>,
        frame_type:  FrameType,
        exposure:    f64,
        gain:        i32,
        offset:      i32,
        img_width:   usize,
        img_height:  usize,
        bin:         i32,
        temperature: Option<f64>,
    ) -> String {
        let mut result = match frame_type {
            FrameType::Biases =>
                format!(
                    "{}_g{}_offs{}_{}x{}",
                    Self::type_part_of_file_name(frame_type),
                    gain, offset, img_width, img_height,
                ),
            _ =>
                format!(
                    "{}_{}_g{}_offs{}_{}x{}",
                    Self::type_part_of_file_name(frame_type),
                    Self::exp_to_str(exposure),
                    gain, offset, img_width, img_height,
                ),
        };

        if bin != 1 {
            result += "_";
            result += &Self::bin_to_str(bin);
        }
        if let Some(temperature) = temperature {
            result += "_";
            result += &Self::temperature_to_str(temperature);
        }
        if frame_type == FrameType::Flats {
            let date = date.expect("Date must be defined for master flat file");
            result += "_";
            result += &Self::date_to_str(date);
        }
        result += ".fit";
        result
    }

    fn defect_pixels_file_name_impl(
        img_width:  usize,
        img_height: usize,
        bin:        i32,
    ) -> String {
        let mut result = format!(
            "defect_pixels_{}x{}",
            img_width, img_height
        );
        if bin != 1 {
            result += "_";
            result += &Self::bin_to_str(bin);
        }
        result += ".txt";
        result
    }

    fn raw_directory_name_impl(
        date:        DateTime<Utc>,
        frame_type:  FrameType,
        exposure:    f64,
        gain:        i32,
        offset:      i32,
        img_width:   usize,
        img_height:  usize,
        bin:         i32,
        temperature: Option<f64>,
    ) -> String {
        let mut result = format!(
            "{}_{}__{}_g{}_offs{}_{}x{}",
            Self::type_part_of_file_name(frame_type),
            Self::date_to_str(date),
            Self::exp_to_str(exposure),
            gain,
            offset,
            img_width,
            img_height,
        );
        if bin != 1 {
            result += "_";
            result += &Self::bin_to_str(bin);
        }
        if let Some(temperature) = temperature {
            result += "_";
            result += &Self::temperature_to_str(temperature);
        }
        result
    }

    fn date_to_str(date: DateTime<Utc>) -> String {
        date.format("%Y-%m-%d").to_string()
    }

    fn type_part_of_file_name(frame_type:  FrameType) -> &'static str {
        match frame_type {
            FrameType::Lights => "light",
            FrameType::Flats => "flat",
            FrameType::Darks => "dark",
            FrameType::Biases => "bias",
        }
    }

    fn exp_to_str(exp: f64) -> String {
        if exp >= 1.0 {
            format!("{:.0}s", exp)
        } else if exp >= 0.001 {
            format!("{:.0}ms", 1_000.8 * exp)
        } else {
            format!("{:.0}us", 1_000_000.8 * exp)
        }
    }

    fn temperature_to_str(temperature: f64) -> String {
        format!("{:+.0}C", temperature)
    }

    fn bin_to_str(bin: i32) -> String {
        format!("bin{0}x{0}", bin)
    }
}

pub fn gain_to_value(
    gain:       Gain,
    cur_gain:   f64,
    gain_range: RangeInclusive<f64>,
) -> f64 {
    let calc_gain = |part| -> f64 {
        part * (gain_range.end() - gain_range.start()) + gain_range.start()
    };

    match gain {
        Gain::Same => cur_gain,
        Gain::Min => calc_gain(0.0),
        Gain::P25 => calc_gain(0.25),
        Gain::P50 => calc_gain(0.50),
        Gain::P75 => calc_gain(0.75),
        Gain::Max => calc_gain(1.0),
    }
}

pub fn check_telescope_is_at_desired_position(
    telescope_ra:  f64,
    telescope_dec: f64,
    desired_pos:   &EqCoord,
    tolerance_in_degree: f64,
) -> eyre::Result<()> {
    let cur_pos = EqCoord {
        ra: hour_to_radian(telescope_ra),
        dec: degree_to_radian(telescope_dec)
    };
    let diff = EqCoord::angle_between(&cur_pos, desired_pos);
    if radian_to_degree(diff) > tolerance_in_degree {
        eyre::bail!("Tepescope position is too far from desired one");
    }
    Ok(())
}
