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
    // Sanitized `Camera::name()`: folder key in the dark library.
    // Must match `RawImageInfo::camera` written by the HAL into FITS `INSTRUME`
    camera_name: String,
    sensor_width: usize,
    sensor_height: usize,
    cooler_supported: bool,
    // Unsupported settings are omitted from master file names
    gain_supported: bool,
    offset_supported: bool,
}

impl FileNameUtils {
    pub fn init(&mut self, camera: &Arc<dyn Camera>) {
        self.camera_name = Self::sanitize_file_name(camera.name());

        let (sensor_width, sensor_height) = camera.ccd_size().unwrap_or((0, 0));
        self.cooler_supported = camera.is_cooler_supported().unwrap_or(false);
        self.gain_supported = camera.is_gain_supported().unwrap_or(false);
        self.offset_supported = camera.is_offset_supported().unwrap_or(false);

        self.sensor_width = sensor_width;
        self.sensor_height = sensor_height;
    }

    /// Sanitized folder name of the camera initialized by `init()`
    pub fn camera_dir_name(&self) -> &str {
        &self.camera_name
    }

    /// Single gate for the temperature suffix in file/folder names:
    /// the setpoint applies only when the camera supports cooling and it is enabled
    pub fn setpoint_for_naming(cooler_supported: bool, ctrl: &CamCtrlOptions) -> Option<f64> {
        if cooler_supported && ctrl.enable_cooler {
            Some(ctrl.temperature)
        } else {
            None
        }
    }

    /// Camera folder key in the dark library / output tree
    fn camera_folder_key(&self, args: &FileNameArg) -> String {
        match args {
            FileNameArg::RawInfo{info, ..} => Self::sanitize_file_name(&info.camera),
            FileNameArg::Options(_) => self.camera_name.clone(),
        }
    }

    /// Camera names may contain arbitrary text (INDI/ASCOM/Alpaca), so make
    /// it safe to use as a path component.
    /// Windows reserved names (CON, COM1, ...) are deliberately not handled:
    /// real camera names never match them
    pub fn sanitize_file_name(name: &str) -> String {
        let mut result: String = name.chars().map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || (c as u32) < 0x20 {
                    '_'
                } else {
                    c
                }
        }).collect();
        // Windows forbids trailing dots and spaces in file/directory names
        while result.ends_with('.') || result.ends_with(' ') {
            result.pop();
        }
        if result.is_empty() {
            result = "_".to_string();
        }
        result
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
                let temperature = Self::setpoint_for_naming(
                    self.cooler_supported,
                    &opts.ctrl
                );
                Self::master_file_name_impl(
                    date,
                    master_frame_type,
                    opts.frame.exposure(),
                    self.gain_supported.then_some(opts.frame.gain as i32),
                    self.offset_supported.then_some(opts.frame.offset),
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
                    // Only the gated setpoint may add the suffix; measured `info.ccd_temp`
                    // must not leak into the lookup name
                    *ccd_temp
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
        let cam_name = self.camera_folder_key(to_calibrate);
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
        let camera = self.camera_folder_key(args);
        let defect_pixels_file_name = match args {
            FileNameArg::Options(opts) => {
                let (img_width, img_height) = opts.frame.active_sensor_size(
                    self.sensor_width,
                    self.sensor_height,
                );
                Self::defect_pixels_file_name_impl(
                    img_width, img_height,
                    opts.frame.binning.get_ratio() as i32,
                )
            }
            FileNameArg::RawInfo{info, ..} => Self::defect_pixels_file_name_impl(
                info.width, info.height,
                info.bin as i32,
            ),
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
        let temperature = Self::setpoint_for_naming(
            self.cooler_supported,
            &cam_options.ctrl
        );
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

    /// Master file to subtract: flat under 1 s exposure -> master bias, otherwise master dark
    pub fn get_subtract_master_fname(
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
        gain:        Option<i32>,   // None = gain unsupported: omit from the name
        offset:      Option<i32>,   // None = offset unsupported: omit from the name
        img_width:   usize,
        img_height:  usize,
        bin:         i32,
        temperature: Option<f64>,
    ) -> String {
        let mut result = Self::type_part_of_file_name(frame_type).to_string();
        if frame_type != FrameType::Biases {
            result += &format!("_{}", Self::exp_to_str(exposure));
        }
        if let Some(gain) = gain {
            result += &format!("_g{}", gain);
        }
        if let Some(offset) = offset {
            result += &format!("_offs{}", offset);
        }
        result += &format!("_{}x{}", img_width, img_height);

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
        eyre::bail!("Telescope position is too far from desired one");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fn_utils() -> FileNameUtils {
        FileNameUtils {
            camera_name: "Cam A".to_string(),
            sensor_width: 1000,
            sensor_height: 800,
            cooler_supported: true,
            gain_supported: true,
            offset_supported: true,
        }
    }

    fn dark_opts() -> CamOptions {
        let mut opts = CamOptions::default();
        opts.frame.frame_type = FrameType::Lights;
        opts.frame.exp_main = 30.0;
        opts.frame.gain = 100.0;
        opts.frame.offset = 10;
        opts.frame.binning = Binning::Bin2;
        opts.ctrl.enable_cooler = true;
        opts.ctrl.temperature = -10.0;
        opts
    }

    #[test]
    fn test_master_file_name_from_options() {
        let mut utils = fn_utils();
        let opts = dark_opts();
        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Darks
        );
        assert_eq!(name, "dark_30s_g100_offs10_500x400_bin2x2_-10C.fit");

        utils.cooler_supported = false;
        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Darks
        );
        assert_eq!(name, "dark_30s_g100_offs10_500x400_bin2x2.fit");
    }

    #[test]
    fn test_master_file_name_bias_no_exposure() {
        let utils = fn_utils();
        let mut opts = dark_opts();
        opts.ctrl.enable_cooler = false;
        opts.frame.binning = Binning::Orig;
        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Biases
        );
        assert_eq!(name, "bias_g100_offs10_1000x800.fit");

        opts.ctrl.enable_cooler = true;
        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Biases
        );
        assert_eq!(name, "bias_g100_offs10_1000x800_-10C.fit");
    }

    #[test]
    fn test_master_file_name_unsupported_parts_omitted() {
        let mut utils = fn_utils();
        let mut opts = dark_opts();
        opts.ctrl.enable_cooler = false;

        // Gain unsupported: no `_g...` segment
        utils.gain_supported = false;
        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Darks
        );
        assert_eq!(name, "dark_30s_offs10_500x400_bin2x2.fit");

        // Both unsupported:
        // unsupported settings cannot split creation and lookup anymore
        utils.offset_supported = false;
        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Darks
        );
        assert_eq!(name, "dark_30s_500x400_bin2x2.fit");

        let name = utils.master_only_file_name(
            None,
            &FileNameArg::Options(&opts),
            FrameType::Biases
        );
        assert_eq!(name, "bias_500x400_bin2x2.fit");
    }

    #[test]
    fn test_master_file_name_exposure_formats() {
        let utils = fn_utils();
        let mut opts = dark_opts();
        opts.ctrl.enable_cooler = false;
        opts.frame.binning = Binning::Orig;

        for (exp, exp_str) in [(1.0, "1s"), (0.5, "500ms"), (0.0001, "100us")] {
            opts.frame.exp_main = exp;
            let name = utils.master_only_file_name(
                None,
                &FileNameArg::Options(&opts),
                FrameType::Darks
            );
            assert_eq!(name, format!("dark_{}_g100_offs10_1000x800.fit", exp_str));
        }
    }

    #[test]
    fn test_get_subtract_master_fname() {
        let utils = fn_utils();

        // Light frame -> master dark
        let opts = dark_opts();
        let (fname, method) = utils.get_subtract_master_fname(
            &FileNameArg::Options(&opts),
            Path::new("/lib")
        );
        assert!(method.contains(CalibrMethods::BY_DARK));
        let mut expected = PathBuf::from("/lib");
        expected.push("Cam A");
        expected.push("dark_30s_g100_offs10_500x400_bin2x2_-10C.fit");
        assert_eq!(fname, expected);

        // Flat with exposure < 1 s -> master bias
        let mut flat_opts = dark_opts();
        flat_opts.frame.frame_type = FrameType::Flats;
        flat_opts.frame.exp_flat = 0.5;
        let (fname, method) = utils.get_subtract_master_fname(
            &FileNameArg::Options(&flat_opts),
            Path::new("/lib")
        );
        assert!(method.contains(CalibrMethods::BY_BIAS));
        assert_eq!(fname.file_name().unwrap().to_str().unwrap(), "bias_g100_offs10_500x400_bin2x2_-10C.fit");

        // Flat with exposure >= 1 s -> master dark
        flat_opts.frame.exp_flat = 2.0;
        let (_fname, method) = utils.get_subtract_master_fname(
            &FileNameArg::Options(&flat_opts),
            Path::new("/lib")
        );
        assert!(method.contains(CalibrMethods::BY_DARK));
    }

    #[test]
    fn test_defect_pixels_file_name() {
        let utils = fn_utils();
        let opts = dark_opts();
        let fname = utils.defect_pixels_file_name(
            &FileNameArg::Options(&opts),
            Path::new("/lib")
        );
        assert_eq!(
            fname.to_str().unwrap().replace('\\', "/"),
            "/lib/Cam A/defect_pixels_500x400_bin2x2.txt"
        );

        // RawInfo branch: folder from sanitized `info.camera`
        let info = RawImageInfo {
            width: 500,
            height: 400,
            bin: 2,
            camera: "Bad:Name/x".to_string(),
            ..Default::default()
        };
        let raw_info_arg = || FileNameArg::RawInfo{
            info: &info,
            ccd_temp: None,
        };
        let fname = utils.defect_pixels_file_name(
            &raw_info_arg(),
            Path::new("/lib")
        );
        assert_eq!(
            fname.to_str().unwrap().replace('\\', "/"),
            "/lib/Bad_Name_x/defect_pixels_500x400_bin2x2.txt"
        );

        // master_file_name must sanitize `info.camera` the same way
        let master = utils.master_file_name(
            &raw_info_arg(),
            Path::new("/lib"),
            FrameType::Darks
        );
        assert!(
            master.to_str().unwrap().replace('\\', "/").contains("/lib/Bad_Name_x/"),
            "unexpected master path: {:?}", master
        );
    }

    #[test]
    fn test_sanitize_file_name() {
        assert_eq!(FileNameUtils::sanitize_file_name("ZWO ASI120MM"), "ZWO ASI120MM");
        assert_eq!(
            FileNameUtils::sanitize_file_name("Cam:1/2\\3*4?5|6<7>8\"9"),
            "Cam_1_2_3_4_5_6_7_8_9"
        );
        assert_eq!(FileNameUtils::sanitize_file_name("name..  "), "name");
        assert_eq!(FileNameUtils::sanitize_file_name("a\u{1}b"), "a_b");
        assert_eq!(FileNameUtils::sanitize_file_name(""), "_");
        assert_eq!(FileNameUtils::sanitize_file_name("..."), "_");
    }
}
