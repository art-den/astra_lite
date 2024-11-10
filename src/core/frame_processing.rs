use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, sync::{mpsc, RwLock, Mutex}, thread::JoinHandle, path::*, io::Cursor};

use bitflags::bitflags;
use chrono::{DateTime, Local, Utc};

use crate::{
    core::{core::ModeType, utils::{FileNameArg, FileNameUtils}}, image::{histogram::*, image::*, info::*, io::*, raw::*, simple_fits::{FitsReader, SeekNRead}, stacker::Stacker, stars_offset::*}, indi, options::*, utils::{log_utils::*, math::linear_interpolate}
};

pub enum ResultImageInfo {
    None,
    LightInfo(Arc<LightFrameInfo>),
    FlatInfo(FlatImageInfo),
    RawInfo(RawImageStat),
}

pub struct ResultImage {
    pub image:    Arc<RwLock<Image>>,
    pub raw_hist: Arc<RwLock<Histogram>>,
    pub img_hist: RwLock<Histogram>,
    pub info:     RwLock<ResultImageInfo>,
}

impl ResultImage {
    pub fn new() -> Self {
        Self {
            image:    Arc::new(RwLock::new(Image::new_empty())),
            raw_hist: Arc::new(RwLock::new(Histogram::new())),
            img_hist: RwLock::new(Histogram::new()),
            info:     RwLock::new(ResultImageInfo::None),
        }
    }
}

#[derive(PartialEq, Clone)]
pub enum PreviewImgSize {
    Fit{ width: usize, height: usize },
    Scale(PreviewScale),
}

impl PreviewImgSize {
    pub fn get_preview_img_size(&self, orig_width: usize, orig_height: usize) -> (usize, usize) {
        match self {
            &PreviewImgSize::Fit { width, height } => {
                let img_ratio = orig_width as f64 / orig_height as f64;
                let gui_ratio = width as f64 / height as f64;
                if img_ratio > gui_ratio {
                    (width, (width as f64 / img_ratio) as usize)
                } else {
                    ((height as f64 * img_ratio) as usize, height)
                }
            },
            PreviewImgSize::Scale(PreviewScale::Original) => (orig_width, orig_height),
            PreviewImgSize::Scale(PreviewScale::P75) => (3*orig_width/4, 3*orig_height/4),
            PreviewImgSize::Scale(PreviewScale::P50) => (orig_width/2, orig_height/2),
            PreviewImgSize::Scale(PreviewScale::P33) => (orig_width/3, orig_height/3),
            PreviewImgSize::Scale(PreviewScale::P25) => (orig_width/4, orig_height/4),
            PreviewImgSize::Scale(PreviewScale::FitWindow) => unreachable!(),
        }
    }
}

#[derive(PartialEq, Clone)]
pub struct PreviewParams {
    pub dark_lvl:         f64,
    pub light_lvl:        f64,
    pub gamma:            f64,
    pub img_size:         PreviewImgSize,
    pub orig_frame_in_ls: bool,
    pub remove_gradient:  bool,
    pub color:            PreviewColor,
}

#[derive(Default, Debug)]
pub struct CalibrParams {
    pub extract_dark:  bool,
    pub dark_lib_path: PathBuf,
    pub flat_fname:    Option<PathBuf>,

    /// search and remove hot pixles
    pub sar_hot_pixs:  bool,
}

#[derive(Default)]
pub struct CalibrData {
    /// Defect pixles found in current master dark
    dark_defect_pixels:  Option<BadPixels>,
    subtract_image:      Option<RawImage>,
    subtract_fname:      Option<PathBuf>,
    master_flat:         Option<RawImage>,
    master_flat_fname:   Option<PathBuf>,
    defect_pixels:       Option<BadPixels>,
    defect_pixels_fname: Option<PathBuf>,
}

impl CalibrData {
    pub fn clear(&mut self) {
        self.dark_defect_pixels = None;
        self.subtract_image = None;
        self.subtract_fname = None;
        self.master_flat = None;
        self.master_flat_fname = None;
        self.defect_pixels = None;
        self.defect_pixels_fname = None;
    }
}

pub struct LiveStackingData {
    pub stacker:  RwLock<Stacker>,
    pub image:    RwLock<Image>,
    pub hist:     RwLock<Histogram>,
    pub info:     RwLock<ResultImageInfo>,
    pub time_cnt: Mutex<f64>,
}

impl LiveStackingData {
    pub fn new() -> Self {
        Self {
            stacker:  RwLock::new(Stacker::new()),
            image:    RwLock::new(Image::new_empty()),
            hist:     RwLock::new(Histogram::new()),
            info:     RwLock::new(ResultImageInfo::None),
            time_cnt: Mutex::new(0.0),
        }
    }

    pub fn clear(&self) {
        self.stacker.write().unwrap().clear();
        self.image.write().unwrap().clear();
        self.hist.write().unwrap().clear();
        *self.info.write().unwrap() = ResultImageInfo::None;
        *self.time_cnt.lock().unwrap() = 0.0;
    }
}

pub struct LiveStackingParams {
    pub data:    Arc<LiveStackingData>,
    pub options: LiveStackingOptions,
}

bitflags! { pub struct ProcessImageFlags: u32 {
    const CALC_STARS_OFFSET = 1;
}}

pub enum ImageSource {
    Blob(Arc<indi::BlobPropValue>),
    FileName(PathBuf),
}

impl ImageSource {
    fn type_hint(&self) -> &str {
        match self {
            Self::Blob(blob) =>
                &blob.format,
            Self::FileName(fname) =>
                fname.extension().unwrap_or_default().to_str().unwrap_or_default(),
        }
    }
}

pub struct FrameProcessCommandData {
    pub mode_type:       ModeType,
    pub camera:          DeviceAndProp,
    pub flags:           ProcessImageFlags,
    pub img_source:      ImageSource,
    pub frame:           Arc<ResultImage>,
    pub stop_flag:       Arc<AtomicBool>,
    pub ref_stars:       Arc<Mutex<Option<Vec<Point>>>>,
    pub calibr_params:   Option<CalibrParams>,
    pub calibr_data:     Arc<Mutex<CalibrData>>,
    pub view_options:    PreviewParams,
    pub frame_options:   FrameOptions,
    pub quality_options: Option<QualityOptions>,
    pub live_stacking:   Option<LiveStackingParams>,
}

pub struct Preview8BitImgData {
    pub rgb_data:     RgbU8Data,
    pub image_width:  usize,
    pub image_height: usize,
    pub params:       PreviewParams,
}

#[derive(Clone)]
pub struct RawFrameInfo {
    pub frame_type:     FrameType,
    pub time:           Option<DateTime<Utc>>,
    pub histogram:      Arc<RwLock<Histogram>>,
    pub calubr_methods: CalibrMethods,
}

#[derive(Clone)]
pub enum FrameProcessResultData {
    Error(String),
    ShotProcessingStarted,
    RawFrameInfo(RawFrameInfo),
    RawFrame(Arc<RawImage>),
    Image(Arc<RwLock<Image>>),
    PreviewFrame(Arc<Preview8BitImgData>),
    PreviewLiveRes(Arc<Preview8BitImgData>),
    LightFrameInfo(Arc<LightFrameInfo>),
    FrameInfo,
    FrameInfoLiveRes,
    HistogramLiveRes,
    MasterSaved {
        frame_type: FrameType,
        file_name: PathBuf
    },
    ShotProcessingFinished {
        frame_is_ok:     bool,
        blob:            Arc<indi::BlobPropValue>,
        raw_image_info:  Arc<RawImageInfo>,
        processing_time: f64,
        blob_dl_time:    f64,
    },
}

#[derive(Clone)]
pub struct FrameProcessResult {
    pub camera:        DeviceAndProp,
    pub cmd_stop_flag: Arc<AtomicBool>,
    pub mode_type:     ModeType,
    pub data:          FrameProcessResultData,
}

pub type ResultFun = Box<dyn Fn(FrameProcessResult) + Send + 'static>;

pub enum FrameProcessCommand {
    ProcessImage {
        command:    FrameProcessCommandData,
        result_fun: ResultFun,
    },
    Exit
}

impl FrameProcessCommand {
    fn name(&self) -> &'static str {
        match self {
            FrameProcessCommand::ProcessImage{..} => "PreviewImage",
            FrameProcessCommand::Exit             => "Exit",
        }
    }
}

pub fn start_frame_processing_thread() -> (mpsc::Sender<FrameProcessCommand>, JoinHandle<()>) {
    let (bg_comands_sender, bg_comands_receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        'outer:
        while let Ok(mut cmd) = bg_comands_receiver.recv() {
            loop {
                if matches!(cmd, FrameProcessCommand::Exit) { break; }
                let next_cmd = bg_comands_receiver.try_recv();
                match next_cmd {
                    Ok(next_cmd) => {
                        log::error!("command {} skipped", cmd.name());
                        cmd = next_cmd;
                    },
                    Err(mpsc::TryRecvError::Disconnected) => {
                        break 'outer;
                    },
                    Err(mpsc::TryRecvError::Empty) => {
                        break;
                    },
                }
            }

            match cmd {
                FrameProcessCommand::Exit =>
                    break,
                FrameProcessCommand::ProcessImage{command, result_fun} =>
                    make_preview_image(command, result_fun),
            };
        }

        log::info!("process_blob_thread_fun finished");
    });
    (bg_comands_sender, thread)
}

fn calc_reduct_ratio(options: &PreviewParams, img_width: usize, img_height: usize) -> usize {
    match &options.img_size {
        &PreviewImgSize::Fit{width, height} => {
            if img_width/4 > width && img_height/4 > height {
                4
            } else if img_width/3 > width && img_height/3 > height {
                3
            } else if img_width/2 > width && img_height/2 > height {
                2
            } else {
                1
            }
        },
        PreviewImgSize::Scale(PreviewScale::Original) => 1,
        PreviewImgSize::Scale(PreviewScale::P75) => 1,
        PreviewImgSize::Scale(PreviewScale::P50) => 2,
        PreviewImgSize::Scale(PreviewScale::P33) => 3,
        PreviewImgSize::Scale(PreviewScale::P25) => 4,
        PreviewImgSize::Scale(PreviewScale::FitWindow) => unreachable!(),
    }
}

pub fn get_rgb_data_from_preview_image(
    image:  &Image,
    hist:   &Histogram,
    params: &PreviewParams,
) -> Option<RgbU8Data> {
    if hist.l.is_none() && hist.b.is_none() {
        return None;
    }

    let reduct_ratio = calc_reduct_ratio(
        params,
        image.width(),
        image.height()
    );
    log::debug!("reduct_ratio = {}", reduct_ratio);

    const WB_PERCENTILE:        usize = 45;
    const DARK_MIN_PERCENTILE:  usize = 1;
    const DARK_MAX_PERCENTILE:  usize = 60;
    const LIGHT_MIN_PERCENTILE: usize = 95;

    let light_max = image.max_value() as f64;
    let light_lvl = params.light_lvl.powf(0.05);

    let l_levels = if let Some(hist) = &hist.l {
        let dark_min = hist.get_percentile(DARK_MIN_PERCENTILE) as f64;
        let dark_max = hist.get_percentile(DARK_MAX_PERCENTILE) as f64;
        let light_min = hist.get_percentile(LIGHT_MIN_PERCENTILE) as f64;
        let mut dark = linear_interpolate(params.dark_lvl, 1.0, 0.0, dark_min, dark_max);
        let mut light = linear_interpolate(light_lvl, 1.0, 0.0, light_min, light_max);
        if (light - dark) < 2.0 { light += 1.0; dark -= 1.0; }
        DarkLightLevels { dark, light }
    } else {
        DarkLightLevels::default()
    };

    let (g_levels, g_wb) = if let Some(hist) = &hist.g {
        let dark_min = hist.get_percentile(DARK_MIN_PERCENTILE) as f64;
        let dark_max = hist.get_percentile(DARK_MAX_PERCENTILE) as f64;
        let light_min = hist.get_percentile(LIGHT_MIN_PERCENTILE) as f64;
        let mut dark = linear_interpolate(params.dark_lvl, 1.0, 0.0, dark_min, dark_max);
        let mut light = linear_interpolate(light_lvl, 1.0, 0.0, light_min, light_max);
        if (light - dark) < 2.0 { light += 1.0; dark -= 1.0; }
        let wb = hist.get_percentile(WB_PERCENTILE) as f64;
        (DarkLightLevels { dark, light }, wb)
    } else {
        (DarkLightLevels::default(), 0.0)
    };

    let g_range = g_levels.light - g_levels.dark;

    let r_levels = if let Some(hist) = &hist.r {
        let wb = hist.get_percentile(WB_PERCENTILE) as f64;
        let dark = g_levels.dark + (wb - g_wb);
        DarkLightLevels { dark, light: dark + g_range }
    } else {
        DarkLightLevels::default()
    };

    let b_levels = if let Some(hist) = &hist.b {
        let wb = hist.get_percentile(WB_PERCENTILE) as f64;
        let dark = g_levels.dark + (wb - g_wb);
        DarkLightLevels { dark, light: dark + g_range }
    } else {
        DarkLightLevels::default()
    };

    let color_mode = match params.color {
        PreviewColor::Rgb   => ToBytesColorMode::Rgb,
        PreviewColor::Red   => ToBytesColorMode::Red,
        PreviewColor::Green => ToBytesColorMode::Green,
        PreviewColor::Blue  => ToBytesColorMode::Blue,
    };

    image.to_grb_bytes(
        &l_levels,
        &r_levels,
        &g_levels,
        &b_levels,
        params.gamma,
        reduct_ratio,
        color_mode,
    )
}

fn apply_calibr_data_and_remove_hot_pixels(
    params:    &Option<CalibrParams>,
    raw_image: &mut RawImage,
    calibr:    &mut CalibrData,
) -> anyhow::Result<()> {
    let Some(params) = params else { return Ok(()); };

    let image_info = raw_image.info();
    let is_flat_file = image_info.frame_type == FrameType::Flats;
    let mut calibr_methods = CalibrMethods::empty();

    let fn_utils = FileNameUtils::default();
    let (defect_pixel_file, subtrack_fname, subtrack_method) =
        if params.extract_dark {
            let to_calibrate = FileNameArg::RawInfo(image_info);
            let defect_pixel_file = fn_utils.defect_pixels_file_name(&to_calibrate, &params.dark_lib_path);
            let (subtrack_fname, subtrack_method) = fn_utils.get_subtrack_master_fname(
                &to_calibrate,
                &params.dark_lib_path
            );
            (Some(defect_pixel_file), Some(subtrack_fname), subtrack_method)
        } else {
            (None, None, CalibrMethods::empty())
        };

    log::debug!("apply_calibr_data_and_remove_hot_pixels params={:?}", params);
    log::debug!("calibr.defect_pixels_fname={:?}", calibr.defect_pixels_fname);
    log::debug!("calibr.subtract_fname={:?}", calibr.subtract_fname);
    log::debug!("calibr.master_flat_fname={:?}", calibr.master_flat_fname);

    let mut reload_flat = false;

    // Load defect pixels file

    if calibr.defect_pixels_fname != defect_pixel_file {
        calibr.defect_pixels = None;
        if let Some(file_name) = &defect_pixel_file { if file_name.is_file() {
            let mut defect_pixels = BadPixels::default();
            log::debug!(
                "Loading defect pixels file {} ...",
                file_name.to_str().unwrap_or_default()
            );
            defect_pixels.load_from_file(&file_name)?;
            calibr.defect_pixels = Some(defect_pixels);
            reload_flat = true;
        }}
        calibr.defect_pixels_fname = defect_pixel_file.clone();
    }

    // Load master dark or bias file

    if calibr.subtract_fname != subtrack_fname {
        calibr.subtract_image = None;
        calibr.dark_defect_pixels = None;
        if let Some(file_name) = &subtrack_fname { if file_name.is_file() {
            log::debug!(
                "Loading master dark file {} ...",
                file_name.to_str().unwrap_or_default()
            );
            let tmr = TimeLogger::start();
            let subtract_image = load_raw_image_from_fits_file(file_name)
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nwhen reading master dark '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("loading master dark from file");

            if subtrack_method.contains(CalibrMethods::BY_DARK) {
                let tmr = TimeLogger::start();
                let defect_pixels = subtract_image.find_hot_pixels_in_master_dark();
                tmr.log("searching hot pixels in dark image");
                log::debug!("hot pixels count = {}", defect_pixels.items.len());
                calibr.dark_defect_pixels = Some(defect_pixels);
                reload_flat = true;
            }

            calibr.subtract_image = Some(subtract_image);
        }}
        calibr.subtract_fname = subtrack_fname.clone();
    }

    // Load master flat file

    if !is_flat_file && (calibr.master_flat_fname != params.flat_fname || reload_flat) {
        calibr.master_flat = None;
        if let Some(file_name) = &params.flat_fname {
            let tmr = TimeLogger::start();
            let mut master_flat = load_raw_image_from_fits_file(file_name)
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nreading master flat '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("loading master flat from file");
            let defect_pixels = calibr
                .defect_pixels.as_ref()
                .or(calibr.dark_defect_pixels.as_ref());
            if let Some(defect_pixels) = defect_pixels {
                let tmr = TimeLogger::start();
                master_flat.remove_bad_pixels(&defect_pixels.items);
                tmr.log("removing bad pixels from master flat");
            }
            let tmr = TimeLogger::start();
            master_flat.filter_flat();
            tmr.log("filter master flat");
            log::debug!(
                "Loaded master flat file {}",
                file_name.to_str().unwrap_or_default()
            );
            calibr.master_flat = Some(master_flat);
        }
        calibr.master_flat_fname = params.flat_fname.clone();
    }

    // Apply master dark or bias image

    if let (Some(file_name), Some(dark_image)) = (&subtrack_fname, &calibr.subtract_image) {
        let tmr = TimeLogger::start();
        raw_image.subtract_dark_or_bias(dark_image)
            .map_err(|err| anyhow::anyhow!(
                "Error {}\nwhen trying to subtract image {}",
                err.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;
        tmr.log("subtracting master dark");
        calibr_methods.set(subtrack_method, true);
    }

    // Apply master flat image

    if let (Some(file_name), Some(flat_image)) = (&params.flat_fname, &calibr.master_flat) {
        let tmr = TimeLogger::start();
        raw_image.apply_flat(flat_image)
            .map_err(|err| anyhow::anyhow!(
                "Error {}\nwher trying to apply flat image {}",
                err.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;

        tmr.log("applying master flat");
        calibr_methods.set(CalibrMethods::BY_FLAT, true);
    }

    // remove defect pixels

    let defect_pixels = calibr
        .defect_pixels.as_ref()
        .or(calibr.dark_defect_pixels.as_ref());
    if let Some(defect_pixels) = defect_pixels {
        if !defect_pixels.items.is_empty() {
            let tmr = TimeLogger::start();
            raw_image.remove_bad_pixels(&defect_pixels.items);
            tmr.log("removing hot pixels from light frame");
        }
        calibr_methods.set(CalibrMethods::DEFECTIVE_PIXELS, true);
    }

    // Search and remove hot pixels if there is no calibration data

    if !is_flat_file
    && params.sar_hot_pixs
    && calibr.defect_pixels.is_none()
    && calibr.dark_defect_pixels.is_none() {
        let tmr = TimeLogger::start();
        let hot_pixels = raw_image.find_hot_pixels_in_light();
        tmr.log("searching hot pixels in light image");
        log::debug!("hot pixels count = {}", hot_pixels.len());
        if !hot_pixels.is_empty() {
            let tmr = TimeLogger::start();
            raw_image.remove_bad_pixels(&hot_pixels);
            tmr.log("removing hot pixels");
        }
        calibr_methods.set(CalibrMethods::HOT_PIXELS_SEARCH, true);
    }

    raw_image.set_calibr_methods(calibr_methods);

    Ok(())
}

fn send_result(
    data:          FrameProcessResultData,
    camera:        &DeviceAndProp,
    mode_type:     ModeType,
    cmd_stop_flag: &Arc<AtomicBool>,
    result_fun:    &ResultFun
) {
    let result = FrameProcessResult {
        camera:        camera.clone(),
        mode_type,
        cmd_stop_flag: Arc::clone(cmd_stop_flag),
        data,
    };
    result_fun(result);
}

fn make_preview_image(
    command:    FrameProcessCommandData,
    result_fun: ResultFun
) {
    let res = make_preview_image_impl(&command, &result_fun);
    if let Err(err) = res {
        send_result(
            FrameProcessResultData::Error(err.to_string()),
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            &result_fun
        );
    }
}

enum ImageLoader<'a> {
    Fits(FitsReader, Box<dyn SeekNRead + 'a>),
    Tif(PathBuf),
}

impl<'a> ImageLoader<'a> {
    fn is_raw_image(&self) -> bool {
        match self {
            Self::Fits(reader, _) =>
                find_mono_image_hdu_in_fits(reader).is_some(),
            Self::Tif(_) =>
                false,
        }
    }

    fn is_ready_image(&self) -> bool {
        match self {
            Self::Fits(reader, _) =>
                find_color_image_hdu_in_fits(reader).is_some(),
            Self::Tif(_) =>
                true,
        }
    }

    fn load_raw_image(&mut self) -> anyhow::Result<RawImage> {
        match self {
            Self::Fits(reader, stream) =>
                load_raw_image_from_fits_reader(reader, stream),
            Self::Tif(_) =>
                anyhow::bail!("Format not support row images"),
        }
    }

    fn load_image(&mut self, image: &mut Image) -> anyhow::Result<()> {
        match self {
            Self::Fits(reader, stream) =>
                load_image_from_fits_reader(image, reader, stream)?,
            Self::Tif(file_name) =>
                load_image_from_tif_file(image, file_name)?,
        }
        Ok(())
    }
}

fn make_preview_image_impl<'a>(
    command:    &'a FrameProcessCommandData,
    result_fun: &ResultFun
) -> anyhow::Result<()> {
    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    let total_tmr = TimeLogger::start();

    send_result(
        FrameProcessResultData::ShotProcessingStarted,
        &command.camera,
        command.mode_type,
        &command.stop_flag,
        result_fun
    );

    let type_hint = command.img_source.type_hint();

    let is_fits_file =
        type_hint.eq_ignore_ascii_case("fit") ||
        type_hint.eq_ignore_ascii_case("fits") ||
        type_hint.eq_ignore_ascii_case(".fit") ||
        type_hint.eq_ignore_ascii_case(".fits");

    let is_tif_file =
        type_hint.eq_ignore_ascii_case("tif");

    let mut loader = if is_fits_file {
        let mut stream: Box<dyn SeekNRead + 'a> = match &command.img_source {
            ImageSource::Blob(blob) =>
                Box::new(Cursor::new(blob.data.as_slice())),
            ImageSource::FileName(file_name) => {
                let file = std::fs::File::open(file_name)?;
                Box::new(file)
            },
        };

        let reader = FitsReader::new(&mut stream)?;
        ImageLoader::Fits(reader, stream)
    } else if is_tif_file {
        if let ImageSource::FileName(file_name) = &command.img_source {
            ImageLoader::Tif(file_name.clone())
        } else {
            unreachable!();
        }
    } else {
        anyhow::bail!("Image format {} is not supported", type_hint);
    };

    let is_raw_image = loader.is_raw_image();
    let is_ready_mage = loader.is_ready_image();

    if !is_raw_image && !is_ready_mage {
        anyhow::bail!("No supported image found in {}", type_hint);
    }

    let mut frame_type = FrameType::Lights;
    let mut exposure = 0_f64;
    let mut raw_info = None;
    let mut raw_noise = None;

    let mut image = if is_raw_image {
        let mut raw_image = loader.load_raw_image()?;
        drop(loader);

        let mut info = raw_image.info().clone();
        if info.offset == 0 {
            info.offset = command.frame_options.offset;
            raw_image.set_offset(info.offset);
        }

        frame_type = info.frame_type;
        exposure = info.exposure;

        log::debug!("Raw type      = {:?}", frame_type);
        log::debug!("Raw width     = {}",   info.width);
        log::debug!("Raw height    = {}",   info.height);
        log::debug!("Raw zero      = {}",   info.offset);
        log::debug!("Raw max_value = {}",   info.max_value);
        log::debug!("Raw CFA       = {:?}", info.cfa);
        log::debug!("Raw bin       = {}",   info.bin);
        log::debug!("Raw exposure  = {}s",  info.exposure);

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        let is_monochrome_img =
            matches!(frame_type, FrameType::Biases) ||
            matches!(frame_type, FrameType::Darks);

        // Raw histogram (before applying calibration data)

        let mut raw_hist = command.frame.raw_hist.write().unwrap();
        let tmr = TimeLogger::start();
        raw_hist.from_raw_image(
            &raw_image,
            is_monochrome_img
        );
        tmr.log("histogram from raw image");
        let debug_log_hist_chan = |name, chan: &Option<HistogramChan>| {
            if let Some(chan) = chan {
                log::debug!("Raw {} median = {}", name, chan.median());
                log::debug!("Raw {} mean   = {}", name, chan.mean);
            }
        };
        debug_log_hist_chan("L", &raw_hist.l);
        debug_log_hist_chan("R", &raw_hist.r);
        debug_log_hist_chan("G", &raw_hist.g);
        debug_log_hist_chan("B", &raw_hist.b);

        drop(raw_hist);

        // Applying calibration data
        if frame_type == FrameType::Lights
        || frame_type == FrameType::Flats {
            let mut calibr = command.calibr_data.lock().unwrap();
            apply_calibr_data_and_remove_hot_pixels(
                &command.calibr_params,
                &mut raw_image,
                &mut calibr
            )?;
            info = raw_image.info().clone()
        }

        let raw_frame_info = RawFrameInfo {
            frame_type,
            time:       info.time,
            histogram:  Arc::clone(&command.frame.raw_hist),
            calubr_methods: info.calibr_methods.clone(),
        };
        send_result(
            FrameProcessResultData::RawFrameInfo(raw_frame_info),
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            result_fun
        );

        let raw_image = Arc::new(raw_image);

        send_result(
            FrameProcessResultData::RawFrame(Arc::clone(&raw_image)),
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            result_fun
        );

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        // Raw noise
        raw_noise = if frame_type == FrameType::Lights {
            let tmr = TimeLogger::start();
            let noise = raw_image.calc_noise();
            tmr.log("light frame raw noise calculation");
            noise
        } else {
            None
        };

        log::debug!("Raw noise = {:?}", raw_noise);

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        match frame_type {
            FrameType::Flats => {
                let hist = command.frame.raw_hist.read().unwrap();
                *command.frame.info.write().unwrap() = ResultImageInfo::FlatInfo(
                    FlatImageInfo::from_histogram(&hist)
                );
                send_result(
                    FrameProcessResultData::FrameInfo,
                    &command.camera,
                    command.mode_type,
                    &command.stop_flag,
                    result_fun
                );
            },
            FrameType::Darks | FrameType::Biases => {
                let hist = command.frame.raw_hist.read().unwrap();
                *command.frame.info.write().unwrap() = ResultImageInfo::RawInfo(
                    RawImageStat::from_histogram(&hist)
                );
                send_result(
                    FrameProcessResultData::FrameInfo,
                    &command.camera,
                    command.mode_type,
                    &command.stop_flag,
                    result_fun
                );
            },

            _ => {},
        }

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        // Demosaic

        let mut image = command.frame.image.write().unwrap();

        let tmr = TimeLogger::start();
        if !is_monochrome_img {
            raw_image.demosaic_into(&mut image, true);
        } else {
            raw_image.copy_into_monochrome(&mut image);
        }
        tmr.log("demosaic");

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        raw_info = Some(info);

        image
    } else if is_ready_mage {
        let mut image = command.frame.image.write().unwrap();
        loader.load_image(&mut image)?;
        drop(loader);
        image
    } else {
        unreachable!();
    };

    // Remove gradient from light frame

    if frame_type == FrameType::Lights
    && (command.view_options.remove_gradient
    || command.mode_type == ModeType::LiveStacking) {
        let tmr = TimeLogger::start();
        image.remove_gradient();
        tmr.log("remove gradient from light frame");
    }

    drop(image);

    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    send_result(
        FrameProcessResultData::Image(Arc::clone(&command.frame.image)),
        &command.camera,
        command.mode_type,
        &command.stop_flag,
        result_fun
    );

    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    // Result image histogram

    let image = command.frame.image.read().unwrap();
    let mut hist = command.frame.img_hist.write().unwrap();
    let tmr = TimeLogger::start();
    hist.from_image(&image);
    tmr.log("histogram for result image");
    drop(hist);

    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    // Preview image RGB bytes

    let hist = command.frame.img_hist.read().unwrap();
    let tmr = TimeLogger::start();
    let rgb_data = get_rgb_data_from_preview_image(
        &image,
        &hist,
        &command.view_options
    );
    tmr.log("get_rgb_bytes_from_preview_image");

    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    if let Some(rgb_data) = rgb_data {
        let preview_data = Arc::new(Preview8BitImgData {
            rgb_data,
            image_width: image.width(),
            image_height: image.height(),
            params: command.view_options.clone(),
        });
        send_result(
            FrameProcessResultData::PreviewFrame(preview_data),
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            result_fun
        );
    }

    let max_stars_fwhm = command.quality_options
        .as_ref()
        .and_then(|qo| if qo.use_max_fwhm { Some(qo.max_fwhm) } else { None });

    let max_stars_ovality = command.quality_options
        .as_ref()
        .and_then(|qo| if qo.use_max_ovality { Some(qo.max_ovality) } else { None });

    let is_bad_frame = if frame_type == FrameType::Lights {
        let mut ref_stars_lock = command.ref_stars.lock().unwrap();

        let ref_stars = if command.flags.contains(ProcessImageFlags::CALC_STARS_OFFSET) {
            ref_stars_lock.as_ref()
        } else {
            None
        };

        // Light frame information

        let tmr = TimeLogger::start();
        let mut info = LightFrameInfo::from_image(
            &image,
            max_stars_fwhm,
            max_stars_ovality,
            ref_stars,
            true,
        );
        info.exposure = exposure;
        info.raw_noise = raw_noise;
        info.calibr_methods = raw_info.as_ref()
            .map(|i| i.calibr_methods)
            .unwrap_or(CalibrMethods::empty());
        tmr.log("TOTAL LightImageInfo::from_image");

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        // Store reference stars for first good light frame

        if command.flags.contains(ProcessImageFlags::CALC_STARS_OFFSET)
        && info.stars.is_ok()
        && ref_stars_lock.is_none() {
            *ref_stars_lock = Some(info.stars.items.iter()
                .map(|star| Point {x: star.x, y: star.y })
                .collect::<Vec<_>>());
        }

        let info = Arc::new(info);

        // Send message about light frame calculated

        send_result(
            FrameProcessResultData::LightFrameInfo(Arc::clone(&info)),
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            result_fun
        );

        // Send message about light frame info stored

        *command.frame.info.write().unwrap() = ResultImageInfo::LightInfo(Arc::clone(&info));
        send_result(
            FrameProcessResultData::FrameInfo,
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            result_fun
        );

        let bad_frame = !info.stars.fwhm_is_ok || !info.stars.ovality_is_ok;

        // Live stacking

        if let (Some(live_stacking), false) = (&command.live_stacking, bad_frame) {
            // Translate/rotate image to reference image and add
            let offset = info.stars_offset.clone().unwrap_or_default();
            let mut stacker = live_stacking.data.stacker.write().unwrap();
            let tmr = TimeLogger::start();
            stacker.add(
                &image,
                &hist,
                -offset.x,
                -offset.y,
                -offset.angle,
                exposure,
                live_stacking.options.remove_tracks
            );
            tmr.log("ImageStacker::add");
            drop(stacker);

            if command.stop_flag.load(Ordering::Relaxed) {
                log::debug!("Command stopped");
                return Ok(());
            }

            let stacker = live_stacking.data.stacker.read().unwrap();

            let mut res_image = live_stacking.data.image.write().unwrap();
            let tmr = TimeLogger::start();
            stacker.copy_to_image(&mut res_image);
            tmr.log("ImageStacker::copy_to_image");

            if command.view_options.remove_gradient {
                let tmr = TimeLogger::start();
                res_image.remove_gradient();
                tmr.log("remove gradient from live stacking result");
            }

            drop(res_image);

            let res_image = live_stacking.data.image.read().unwrap();

            // Histogram for live stacking image

            let mut hist = live_stacking.data.hist.write().unwrap();
            let tmr = TimeLogger::start();
            hist.from_image(&res_image);
            tmr.log("histogram from live view image");
            drop(hist);

            if command.stop_flag.load(Ordering::Relaxed) {
                log::debug!("Command stopped");
                return Ok(());
            }

            let hist = live_stacking.data.hist.read().unwrap();
            send_result(
                FrameProcessResultData::HistogramLiveRes,
                &command.camera,
                command.mode_type,
                &command.stop_flag,
                result_fun
            );

            // Live stacking image info

            let tmr = TimeLogger::start();
            let mut live_stacking_info = LightFrameInfo::from_image(
                &res_image,
                max_stars_fwhm,
                max_stars_ovality,
                None,
                true,
            );
            live_stacking_info.exposure = stacker.total_exposure();
            tmr.log("LightImageInfo::from_image for livestacking");

            if command.stop_flag.load(Ordering::Relaxed) {
                log::debug!("Command stopped");
                return Ok(());
            }

            *live_stacking.data.info.write().unwrap() = ResultImageInfo::LightInfo(
                Arc::new(live_stacking_info)
            );
            send_result(
                FrameProcessResultData::FrameInfoLiveRes,
                &command.camera,
                command.mode_type,
                &command.stop_flag,
                result_fun
            );

            // Convert into preview RGB bytes

            if !command.view_options.orig_frame_in_ls {
                let tmr = TimeLogger::start();
                let rgb_data = get_rgb_data_from_preview_image(
                    &res_image,
                    &hist,
                    &command.view_options
                );
                tmr.log("get_rgb_bytes_from_preview_image");

                if command.stop_flag.load(Ordering::Relaxed) {
                    log::debug!("Command stopped");
                    return Ok(());
                }

                if let Some(rgb_data) = rgb_data {
                    let preview_data = Arc::new(Preview8BitImgData {
                        rgb_data,
                        image_width: image.width(),
                        image_height: image.height(),
                        params: command.view_options.clone(),
                    });

                    send_result(
                        FrameProcessResultData::PreviewLiveRes(preview_data),
                        &command.camera,
                        command.mode_type,
                        &command.stop_flag,
                        result_fun
                    );
                }
            }

            // save result image

            if live_stacking.options.save_enabled {
                let save_res_interv = live_stacking.options.save_minutes as f64 * 60.0;
                let mut save_cnt = live_stacking.data.time_cnt.lock().unwrap();
                *save_cnt += exposure;
                if *save_cnt >= save_res_interv {
                    *save_cnt = 0.0;
                    drop(save_cnt);
                    let now_time: DateTime<Local> = Local::now();
                    let now_time_str = now_time.format("%Y%m%d-%H%M%S").to_string();
                    let file_path = live_stacking.options.out_dir
                        .join("Result");
                    if !file_path.exists() {
                        std::fs::create_dir_all(&file_path)
                            .map_err(|e|anyhow::anyhow!(
                                "Error '{}'\nwhen trying to create directory '{}' for saving result live stack image",
                                e.to_string(),
                                file_path.to_str().unwrap_or_default()
                            ))?;
                    }
                    let file_path = file_path.join(format!("Live_{}.tif", now_time_str));
                    let tmr = TimeLogger::start();
                    stacker.save_to_tiff(&file_path)?;
                    tmr.log("save live stacking result image");
                }
            }
        }

        bad_frame
    } else {
        false
    };

    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    let process_time = total_tmr.log("TOTAL PREVIEW");

    if let (ImageSource::Blob(blob), Some(raw_info)) = (&command.img_source, raw_info) {
        let result = FrameProcessResultData::ShotProcessingFinished{
            raw_image_info:  Arc::new(raw_info),
            frame_is_ok:     !is_bad_frame,
            blob:            Arc::clone(&blob),
            processing_time: process_time,
            blob_dl_time:    blob.dl_time,
        };
        send_result(
            result,
            &command.camera,
            command.mode_type,
            &command.stop_flag,
            result_fun
        );
    }

    Ok(())
}
