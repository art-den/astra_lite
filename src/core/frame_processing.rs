use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, sync::{mpsc, RwLock, Mutex}, thread::JoinHandle, path::*, io::Cursor};

use bitflags::bitflags;
use chrono::{DateTime, Local, Utc};

use crate::{
    indi,
    image::raw::*,
    image::histogram::*,
    image::image::*,
    image::info::*,
    utils::log_utils::*,
    image::stars_offset::*,
    options::*,
    core::core::ModeType,
    utils::math::linear_interpolate
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
    pub dark_fname:       Option<PathBuf>,
    pub flat_fname:       Option<PathBuf>,
    pub def_pixels_fname: Option<PathBuf>,
    pub hot_pixels:       bool,
}

#[derive(Default)]
pub struct CalibrData {
    master_dark:         Option<RawImage>,
    master_dark_fname:   Option<PathBuf>,
    master_flat:         Option<RawImage>,
    master_flat_fname:   Option<PathBuf>,
    defect_pixels:       Option<BadPixels>,
    defect_pixels_fname: Option<PathBuf>,
}

impl CalibrData {
    pub fn clear(&mut self) {
        self.master_dark = None;
        self.master_dark_fname = None;
        self.master_flat = None;
        self.master_flat_fname = None;
        self.defect_pixels = None;
        self.defect_pixels_fname = None;
    }
}

pub struct LiveStackingData {
    pub adder:    RwLock<ImageAdder>,
    pub image:    RwLock<Image>,
    pub hist:     RwLock<Histogram>,
    pub info:     RwLock<ResultImageInfo>,
    pub time_cnt: Mutex<f64>,
}

impl LiveStackingData {
    pub fn new() -> Self {
        Self {
            adder:    RwLock::new(ImageAdder::new()),
            image:    RwLock::new(Image::new_empty()),
            hist:     RwLock::new(Histogram::new()),
            info:     RwLock::new(ResultImageInfo::None),
            time_cnt: Mutex::new(0.0),
        }
    }
}

pub struct LiveStackingParams {
    pub data:    Arc<LiveStackingData>,
    pub options: LiveStackingOptions,
}

bitflags! { pub struct ProcessImageFlags: u32 {
    const CALC_STARS_OFFSET = 1;
}}

pub struct FrameProcessCommandData {
    pub mode_type:       ModeType,
    pub camera:          DeviceAndProp,
    pub flags:           ProcessImageFlags,
    pub blob:            Arc<indi::BlobPropValue>,
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
    pub rgb_data:     Mutex<RgbU8Data>,
    pub image_width:  usize,
    pub image_height: usize,
    pub params:       PreviewParams,
}

#[derive(Clone)]
pub struct HistogramResult {
    pub frame_type: FrameType,
    pub time:       Option<DateTime<Utc>>,
    pub histogram:  Arc<RwLock<Histogram>>,
}

#[derive(Clone)]
pub enum FrameProcessResultData {
    Error(String),
    ShotProcessingStarted,
    RawHistogram(HistogramResult),
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

fn create_raw_image_from_blob(
    blob_prop_value: &Arc<indi::BlobPropValue>
) -> anyhow::Result<RawImage> {
    if blob_prop_value.format == ".fits" {
        let mem_stream = Cursor::new(blob_prop_value.data.as_slice());
        let raw_image = RawImage::new_from_fits_stream(mem_stream)?;
        return Ok(raw_image);
    }

    anyhow::bail!("Unsupported blob format: {}", blob_prop_value.format);
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

    Some(image.to_grb_bytes(
        &l_levels,
        &r_levels,
        &g_levels,
        &b_levels,
        params.gamma,
        reduct_ratio,
        color_mode,
    ))
}


bitflags! {
    #[derive(Clone)]
    pub struct CalibrMethods: u32 {
        const BY_DARK           = 1;
        const BY_FLAT           = 2;
        const DEFECTIVE_PIXELS  = 4;
        const HOT_PIXELS_SEARCH = 8;
    }
}

fn apply_calibr_data_and_remove_hot_pixels(
    params:    &Option<CalibrParams>,
    raw_image: &mut RawImage,
    calibr:    &mut CalibrData,
) -> anyhow::Result<CalibrMethods> {
    let mut result = CalibrMethods::empty();

    log::debug!("apply_calibr_data_and_remove_hot_pixels params={:?}", params);

    let Some(params) = params else { return Ok(result); };

    log::debug!("calibr.defect_pixels_fname={:?}", calibr.defect_pixels_fname);
    log::debug!("calibr.master_dark_fname={:?}", calibr.master_dark_fname);
    log::debug!("calibr.master_flat_fname{:?}", calibr.master_flat_fname);

    // Load defect pixels file

    if calibr.defect_pixels_fname != params.def_pixels_fname {
        let mut loaded = false;
        if let Some(file_name) = &params.def_pixels_fname { if file_name.is_file() {
            let mut defect_pixels = BadPixels::default();
            log::debug!(
                "Loading defect pixels file {} ...",
                file_name.to_str().unwrap_or_default()
            );
            defect_pixels.load_from_file(&file_name)?;
            loaded = true;
            calibr.defect_pixels = Some(defect_pixels);
        }}
        if !loaded {
            calibr.defect_pixels = None;
        }
        calibr.defect_pixels_fname = params.def_pixels_fname.clone();
    }

    // Load master dark file

    if calibr.master_dark_fname != params.dark_fname {
        let mut loaded = false;
        if let Some(file_name) = &params.dark_fname { if file_name.is_file() {
            log::debug!(
                "Loading master dark file {} ...",
                file_name.to_str().unwrap_or_default()
            );
            let tmr = TimeLogger::start();
            let master_dark = RawImage::new_from_fits_file(file_name)
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nwhen reading master dark '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("loading master dark from file");
            if calibr.defect_pixels.is_none() {
                let tmr = TimeLogger::start();
                let defect_pixels = master_dark.find_hot_pixels_in_master_dark();
                tmr.log("searching hot pixels in dark image");
                log::debug!("hot pixels count = {}", defect_pixels.items.len());
                calibr.defect_pixels = Some(defect_pixels);
            }
            loaded = true;
            calibr.master_dark = Some(master_dark);
        }}
        if !loaded {
            calibr.master_dark = None;
        }
        calibr.master_dark_fname = params.dark_fname.clone();
    }

    // Load master flat file

    if calibr.master_flat_fname != params.flat_fname {
        if let Some(file_name) = &params.flat_fname {
            let tmr = TimeLogger::start();
            let mut master_flat = RawImage::new_from_fits_file(file_name)
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nreading master flat '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("loading master flat from file");
            if let Some(defect_pixels) = &calibr.defect_pixels {
                let tmr = TimeLogger::start();
                raw_image.remove_bad_pixels(&defect_pixels.items);
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
        } else {
            calibr.master_flat = None;
        }
        calibr.master_flat_fname = params.flat_fname.clone();
    }

    // Apply master dark image

    if let (Some(file_name), Some(dark_image)) = (&params.dark_fname, &calibr.master_dark) {
        let tmr = TimeLogger::start();
        raw_image.subtract_dark(dark_image)
            .map_err(|err| anyhow::anyhow!(
                "Error {}\nwhen trying to subtract dark image {}",
                err.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;
        tmr.log("subtracting master dark");
        result.set(CalibrMethods::BY_DARK, true);
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
        result.set(CalibrMethods::BY_FLAT, true);
    }

    // remove defect pixels

    if let Some(defect_pixels) = &calibr.defect_pixels {
        let tmr = TimeLogger::start();
        raw_image.remove_bad_pixels(&defect_pixels.items);
        tmr.log("removing hot pixels from light frame");
        result.set(CalibrMethods::DEFECTIVE_PIXELS, true);
    }

    // Find and remove hot pixels if there is no calibration data

    if params.hot_pixels && calibr.defect_pixels.is_none() {
        let tmr = TimeLogger::start();
        let hot_pixels = raw_image.find_hot_pixels_in_light();
        tmr.log("searching hot pixels in light image");
        log::debug!("hot pixels count = {}", hot_pixels.len());
        raw_image.remove_bad_pixels(&hot_pixels);
        result.set(CalibrMethods::HOT_PIXELS_SEARCH, true);
    }

    Ok(result)
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

fn make_preview_image_impl(
    command:    &FrameProcessCommandData,
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

    log::debug!("Starting BLOB processing... Blob len = {}", command.blob.data.len());

    let tmr = TimeLogger::start();
    let mut raw_image = create_raw_image_from_blob(&command.blob)?;
    tmr.log("create_raw_image_from_blob");

    let mut raw_info = raw_image.info().clone();
    if raw_info.offset == 0 {
        raw_info.offset = command.frame_options.offset;
        raw_image.set_offset(raw_info.offset);
    }

    log::debug!("Raw type      = {:?}", raw_info.frame_type);
    log::debug!("Raw width     = {}",   raw_info.width);
    log::debug!("Raw height    = {}",   raw_info.height);
    log::debug!("Raw zero      = {}",   raw_info.offset);
    log::debug!("Raw max_value = {}",   raw_info.max_value);
    log::debug!("Raw CFA       = {:?}", raw_info.cfa);
    log::debug!("Raw bin       = {}",   raw_info.bin);
    log::debug!("Raw exposure  = {}s",  raw_info.exposure);

    if command.stop_flag.load(Ordering::Relaxed) {
        log::debug!("Command stopped");
        return Ok(());
    }

    let is_monochrome_img =
        matches!(raw_info.frame_type, FrameType::Biases) ||
        matches!(raw_info.frame_type, FrameType::Darks);

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

    let histogram_result = HistogramResult {
        frame_type: command.frame_options.frame_type,
        time:       raw_image.info().time.clone(),
        histogram:  Arc::clone(&command.frame.raw_hist),
    };

    send_result(
        FrameProcessResultData::RawHistogram(histogram_result),
        &command.camera,
        command.mode_type,
        &command.stop_flag,
        result_fun
    );

    // Applying calibration data
    let mut calibr_methods = CalibrMethods::empty();
    if raw_info.frame_type == FrameType::Lights {
        let mut calibr = command.calibr_data.lock().unwrap();
        calibr_methods = apply_calibr_data_and_remove_hot_pixels(
            &command.calibr_params,
            &mut raw_image,
            &mut calibr
        )?;
    }

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
    let raw_noise = if raw_info.frame_type == FrameType::Lights {
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

    match raw_info.frame_type {
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

    // Remove gradient from light frame

    if raw_info.frame_type == FrameType::Lights
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

    let raw_image_info = Arc::new(raw_image.info().clone());

    drop(raw_image);

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
            rgb_data: Mutex::new(rgb_data),
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

    let is_bad_frame = if raw_info.frame_type == FrameType::Lights {
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
        info.exposure = raw_info.exposure;
        info.raw_noise = raw_noise;
        info.calibr_methods = calibr_methods;
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

        if let (Some(live_stacking), false) = (command.live_stacking.as_ref(), bad_frame) {
            // Translate/rotate image to reference image and add
            if let Some(offset) = &info.stars_offset {
                let mut image_adder = live_stacking.data.adder.write().unwrap();
                let tmr = TimeLogger::start();
                image_adder.add(
                    &image,
                    hist.r.as_ref().map(|chan| chan.median()),
                    hist.g.as_ref().map(|chan| chan.median()),
                    hist.b.as_ref().map(|chan| chan.median()),
                    hist.l.as_ref().map(|chan| chan.median()),
                    -offset.x,
                    -offset.y,
                    -offset.angle,
                    raw_info.exposure
                );
                tmr.log("ImageAdder::add");
                drop(image_adder);

                if command.stop_flag.load(Ordering::Relaxed) {
                    log::debug!("Command stopped");
                    return Ok(());
                }

                let image_adder = live_stacking.data.adder.read().unwrap();

                let mut res_image = live_stacking.data.image.write().unwrap();
                let tmr = TimeLogger::start();
                image_adder.copy_to_image(&mut res_image, true);
                tmr.log("ImageAdder::copy_to_image");

                if command.view_options.remove_gradient { // TODO: do gradient removal in image_adder.copy_to_image!
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
                live_stacking_info.exposure = image_adder.total_exposure();
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
                            rgb_data: Mutex::new(rgb_data),
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
                    *save_cnt += raw_info.exposure;
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
                        image_adder.save_to_tiff(&file_path)?;
                        tmr.log("save live stacking result image");
                    }
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

    let result = FrameProcessResultData::ShotProcessingFinished{
        raw_image_info,
        frame_is_ok:     !is_bad_frame,
        blob:            Arc::clone(&command.blob),
        processing_time: process_time,
        blob_dl_time:    command.blob.dl_time,
    };
    send_result(
        result,
        &command.camera,
        command.mode_type,
        &command.stop_flag,
        result_fun
    );

    Ok(())
}
