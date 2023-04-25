use std::{sync::Arc, sync::{mpsc, RwLock, Mutex}, thread::JoinHandle, path::*, io::Cursor};

use bitflags::bitflags;
use chrono::{DateTime, Local, Utc};

use crate::{
    indi_api,
    image_raw::*,
    image::*,
    image_info::*,
    log_utils::*,
    stars_offset::*,
    options::*,
    state::ModeType,
};

pub enum ResultImageInfo {
    None,
    LightInfo(LightImageInfo),
    FlatInfo(FlatImageInfo),
    RawInfo(RawImageStat),
    SaveMaster(SaveMasterResult),
}

pub struct SaveMasterResult {
    path:      PathBuf,
    frametype: FrameType,
}

pub struct ResultImage {
    pub image: RwLock<Image>,
    pub hist:  RwLock<Histogram>,
    pub info:  RwLock<ResultImageInfo>,
}

impl ResultImage {
    pub fn new() -> Self {
        Self {
            image: RwLock::new(Image::new_empty()),
            hist:  RwLock::new(Histogram::new()),
            info:  RwLock::new(ResultImageInfo::None),
        }
    }
}

#[derive(PartialEq, Clone)]
pub struct PreviewParams {
    pub auto_min:        bool,
    pub gamma:           f64,
    pub max_img_width:   Option<usize>,
    pub max_img_height:  Option<usize>,
    pub show_orig_frame: bool,
}

#[derive(Default)]
pub struct CalibrParams {
    pub dark:       Option<PathBuf>,
    pub flat:       Option<PathBuf>,
    pub hot_pixels: bool,
}

#[derive(Default)]
pub struct CalibrImages {
    master_dark:     Option<RawImage>,
    master_dark_fn:  Option<PathBuf>,
    master_flat:     Option<RawImage>,
    master_flat_fn:  Option<PathBuf>,
    dark_hot_pixels: Vec<BadPixel>,
}

pub struct RawAdderParams {
    pub adder: Arc<Mutex<RawAdder>>,
    pub save:  bool,
}

pub struct LiveStackingData {
    pub adder:    RwLock<ImageAdder>,
    pub result:   ResultImage,
    pub time_cnt: Mutex<f64>,
}

impl LiveStackingData {
    pub fn new() -> Self {
        Self {
            adder:    RwLock::new(ImageAdder::new()),
            result:   ResultImage::new(),
            time_cnt: Mutex::new(0.0),
        }
    }
}

pub struct LiveStackingParams {
    pub data:    Arc<LiveStackingData>,
    pub options: LiveStackingOptions,
}

bitflags! {
    pub struct ProcessImageFlags: u32 {
        const CALC_STARS_OFFSET = 1;
        const SAVE_RAW          = 2;
    }
}

pub struct ProcessImageCommand {
    pub mode_type:       ModeType,
    pub camera:          String,
    pub flags:           ProcessImageFlags,
    pub blob:            Arc<indi_api::BlobPropValue>,
    pub frame:           Arc<ResultImage>,
    pub ref_stars:       Arc<RwLock<Option<Vec<Point>>>>,
    pub calibr_params:   CalibrParams,
    pub calibr_images:   Arc<Mutex<CalibrImages>>,
    pub fn_gen:          Arc<Mutex<SeqFileNameGen>>,
    pub view_options:    PreviewParams,
    pub frame_options:   FrameOptions,
    pub quality_options: Option<QualityOptions>,
    pub save_path:       Option<PathBuf>,
    pub raw_adder:       Option<RawAdderParams>,
    pub live_stacking:   Option<LiveStackingParams>,
}

pub struct PreviewImgData {
    pub rgb_bytes:    RgbU8Data,
    pub image_width:  usize,
    pub image_height: usize,
    pub params:       PreviewParams,
}

bitflags! {
    #[derive(Default)]
    pub struct LightFrameShortInfoFlags: u32 {
        const BAD_STARS_FWHM = 1;
        const BAD_STARS_OVAL = 2;
        const BAD_OFFSET     = 4;
    }
}

#[derive(Default, Debug, Clone)]
pub struct LightFrameShortInfo {
    pub width:          usize,
    pub height:         usize,
    pub time:           DateTime<Utc>,
    pub exposure:       f64,
    pub stars_fwhm:     Option<f32>,
    pub stars_ovality:  Option<f32>,
    pub stars_count:    usize,
    pub noise:          f32, // %
    pub background:     f32, // %
    pub offset_x:       Option<f64>,
    pub offset_y:       Option<f64>,
    pub angle:          Option<f64>,
    pub flags:          LightFrameShortInfoFlags,
}

pub enum ProcessingResultData {
    Error(String),
    ShotProcessingStarted(ModeType),
    ShotProcessingFinished {
        mode_type:   ModeType,
        frame_is_ok: bool
    },
    LightShortInfo(LightFrameShortInfo, ModeType),
    PreviewFrame(PreviewImgData, ModeType),
    PreviewLiveRes(PreviewImgData, ModeType),
    FrameInfo(ModeType),
    FrameInfoLiveRes(ModeType),
    Histogram(ModeType),
    HistogramLiveRes(ModeType),
    MasterSaved {
        frame_type: FrameType,
        file_name: PathBuf
    }
}

pub struct FrameProcessingResult {
    pub camera: String,
    pub data:   ProcessingResultData,
}

pub type ResultFun = Box<dyn Fn(FrameProcessingResult) + Send + 'static>;

pub enum Command {
    ProcessImage {
        command:    ProcessImageCommand,
        result_fun: ResultFun,
    },
    Exit
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::ProcessImage{..} => "PreviewImage",
            Command::Exit             => "Exit",
        }
    }
}

pub fn start_process_blob_thread() -> (mpsc::Sender<Command>, JoinHandle<()>) {
    let (bg_comands_sender, bg_comands_receiver) = mpsc::channel();
    let thread = std::thread::spawn(|| {
        process_blob_thread_fun(bg_comands_receiver);
        log::info!("process_blob_thread_fun finished");
    });
    (bg_comands_sender, thread)
}

fn process_blob_thread_fun(receiver: mpsc::Receiver<Command>) {
    'outer:
    while let Ok(mut cmd) = receiver.recv() {
        loop {
            if matches!(cmd, Command::Exit) { break; }
            let next_cmd = receiver.try_recv();
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
            Command::Exit =>
                break,
            Command::ProcessImage{command, result_fun} =>
                make_preview_image(command, result_fun),
        };
    }
}

fn create_raw_image_from_blob(
    blob_prop_value: &Arc<indi_api::BlobPropValue>
) -> anyhow::Result<RawImage> {
    if blob_prop_value.format == ".fits" {
        let mem_stream = Cursor::new(blob_prop_value.data.as_slice());
        let raw_image = RawImage::new_from_fits_stream(mem_stream)?;
        return Ok(raw_image);
    }

    anyhow::bail!("Unsupported blob format: {}", blob_prop_value.format);
}

fn calc_reduct_ratio(options:  &PreviewParams, img_width: usize, img_height: usize) -> usize {
    let max_img_width = options.max_img_width.unwrap_or(usize::MAX);
    let max_img_height = options.max_img_height.unwrap_or(usize::MAX);
    if img_width / 4  > max_img_width
    && img_height / 4 > max_img_height {
        4
    } else if img_width / 3  > max_img_width
           && img_height / 3 > max_img_height {
        3
    } else if img_width / 2  > max_img_width
           && img_height / 2 > max_img_height {
        2
    } else {
        1
    }
}

pub fn get_rgb_bytes_from_preview_image(
    image:    &Image,
    hist:     &Histogram,
    options:  &PreviewParams,
) -> RgbU8Data {
    let reduct_ratio = calc_reduct_ratio(
        options,
        image.width(),
        image.height()
    );
    log::debug!("reduct_ratio = {}", reduct_ratio);
    const BLACK_PERCENTILE: usize = 5;
    let l_blk_lvl = hist.l.as_ref().map(|h| h.get_percentile(BLACK_PERCENTILE)).unwrap_or(0);
    let r_blk_lvl = hist.r.as_ref().map(|h| h.get_percentile(BLACK_PERCENTILE)).unwrap_or(0);
    let g_blk_lvl = hist.g.as_ref().map(|h| h.get_percentile(BLACK_PERCENTILE)).unwrap_or(0);
    let b_blk_lvl = hist.b.as_ref().map(|h| h.get_percentile(BLACK_PERCENTILE)).unwrap_or(0);
    image.to_grb_bytes(
        if options.auto_min { Some(l_blk_lvl as i32) } else { None },
        if options.auto_min { Some(r_blk_lvl as i32) } else { None },
        if options.auto_min { Some(g_blk_lvl as i32) } else { None },
        if options.auto_min { Some(b_blk_lvl as i32) } else { None },
        options.gamma,
        reduct_ratio
    )
}

fn apply_calibr_data_and_remove_hot_pixels(
    params:    &CalibrParams,
    raw_image: &mut RawImage,
    calibr:    &mut CalibrImages,
    mt:        bool,
) -> anyhow::Result<()> {
    if let Some(file_name) = &params.dark {
        if calibr.master_dark.is_none()
        || params.dark != calibr.master_dark_fn {
            let tmr = TimeLogger::start();
            let master_dark = RawImage::new_from_fits_file(file_name)
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nwhen reading master dark '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("loading master dark from file");
            let tmr = TimeLogger::start();
            calibr.dark_hot_pixels = master_dark.find_hot_pixels_in_master_dark();
            tmr.log("searching hot pixels in dark image");
            log::debug!("hot pixels count = {}", calibr.dark_hot_pixels.len());
            calibr.master_dark = Some(master_dark);
            calibr.master_dark_fn = Some(file_name.clone());
        }
    }

    if let (Some(file_name), Some(dark_image)) = (&params.dark, &calibr.master_dark) {
        let tmr = TimeLogger::start();
        raw_image.subtract_dark(dark_image)
            .map_err(|err| anyhow::anyhow!(
                "Error {}\nwhen trying to subtract dark image {}",
                err.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;
        tmr.log("subtracting master dark");
        let tmr = TimeLogger::start();
        raw_image.remove_bad_pixels(&calibr.dark_hot_pixels);
        tmr.log("removing hot pixels from light frame");
    }

    if params.hot_pixels && calibr.master_dark.is_none() {
        let tmr = TimeLogger::start();
        let hot_pixels = raw_image.find_hot_pixels_in_light();
        tmr.log("searching hot pixels in light image");
        log::debug!("hot pixels count = {}", hot_pixels.len());
        raw_image.remove_bad_pixels(&hot_pixels);
    }

    if let Some(file_name) = &params.flat {
        if calibr.master_flat.is_none()
        || params.flat != calibr.master_flat_fn {
            let tmr = TimeLogger::start();
            let mut master_flat = RawImage::new_from_fits_file(file_name)
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nreading master flat '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("loading master flat from file");
            let tmr = TimeLogger::start();
            raw_image.remove_bad_pixels(&calibr.dark_hot_pixels);
            tmr.log("removing bad pixels from master flat");
            let tmr = TimeLogger::start();
            master_flat.filter_flat();
            tmr.log("filter master flat");
            calibr.master_flat = Some(master_flat);
            calibr.master_flat_fn = Some(file_name.clone());
        }
    }

    if let (Some(file_name), Some(flat_image)) = (&params.flat, &calibr.master_flat) {
        let tmr = TimeLogger::start();
        raw_image.apply_flat(flat_image, mt)
            .map_err(|err| anyhow::anyhow!(
                "Error {}\nwher trying to apply flat image {}",
                err.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;

        tmr.log("applying master flat");
    }

    Ok(())
}

fn send_result(
    data:       ProcessingResultData,
    camera:     &str,
    result_fun: &ResultFun
) {
    let result = FrameProcessingResult {
        data,
        camera: camera.to_string(),
    };
    result_fun(result);
}

fn make_preview_image(
    command:    ProcessImageCommand,
    result_fun: ResultFun
) {
    let res = make_preview_image_impl(&command, &result_fun);
    if let Err(err) = res {
        send_result(
            ProcessingResultData::Error(err.to_string()),
            &command.camera,
            &result_fun
        );
    }
}

fn add_calibr_image(
    raw_image: &mut RawImage,
    raw_adder: &Option<RawAdderParams>,
    frame_type: FrameType
) -> anyhow::Result<()> {
    let Some(adder) = raw_adder else { return Ok(()); };
    let mut adder = adder.adder.lock().unwrap();
    if frame_type == FrameType::Flats {
        let tmr = TimeLogger::start();
        raw_image.normalize_flat();
        tmr.log("Normalizing flat");
    }
    let tmr = TimeLogger::start();
    adder.add(raw_image)?;
    tmr.log("Adding raw calibration frame");
    Ok(())
}

fn make_preview_image_impl(
    command:    &ProcessImageCommand,
    result_fun: &ResultFun
) -> anyhow::Result<()> {
    let total_tmr = TimeLogger::start();

    send_result(
        ProcessingResultData::ShotProcessingStarted(command.mode_type),
        &command.camera,
        result_fun
    );

    let tmr = TimeLogger::start();
    let mut raw_image = create_raw_image_from_blob(&command.blob)?;
    tmr.log("create_raw_image_from_blob");
    let exposure = raw_image.info().exposure;

    let frame_type = raw_image.info().frame_type;

    let is_monochrome_img =
        matches!(frame_type, FrameType::Biases) ||
        matches!(frame_type, FrameType::Darks);

    // Histogram

    let mut hist = command.frame.hist.write().unwrap();
    let tmr = TimeLogger::start();
    hist.from_raw_image(
        &raw_image,
        is_monochrome_img,
        true
    );
    tmr.log("histogram from raw image");
    drop(hist);

    // Applying calibration data
    if frame_type == FrameType::Lights {
        let mut calibr = command.calibr_images.lock().unwrap();
        apply_calibr_data_and_remove_hot_pixels(&command.calibr_params, &mut raw_image, &mut calibr, true)?;
    }

    send_result(
        ProcessingResultData::Histogram(command.mode_type),
        &command.camera,
        result_fun
    );

    match frame_type {
        FrameType::Flats => {
            let hist = command.frame.hist.read().unwrap();
            *command.frame.info.write().unwrap() = ResultImageInfo::FlatInfo(
                FlatImageInfo::from_histogram(&hist)
            );
            send_result(
                ProcessingResultData::FrameInfo(command.mode_type),
                &command.camera,
                result_fun
            );
        },
        FrameType::Darks | FrameType::Biases => {
            let hist = command.frame.hist.read().unwrap();
            *command.frame.info.write().unwrap() = ResultImageInfo::RawInfo(
                RawImageStat::from_histogram(&hist)
            );
            send_result(
                ProcessingResultData::FrameInfo(command.mode_type),
                &command.camera,
                result_fun
            );
        },

        _ => {},
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

    let frame_for_raw_adder = matches!(
        frame_type,
        FrameType::Flats| FrameType::Darks | FrameType::Biases
    );

    if frame_for_raw_adder {
        add_calibr_image(&mut raw_image, &command.raw_adder, frame_type)?;
    }

    drop(image);
    drop(raw_image);

    // Preview image RGB bytes

    let image = command.frame.image.read().unwrap();
    let hist = command.frame.hist.read().unwrap();
    let tmr = TimeLogger::start();
    let rgb_bytes = get_rgb_bytes_from_preview_image(
        &image,
        &hist,
        &command.view_options
    );
    tmr.log("get_rgb_bytes_from_preview_image");

    let preview_data = PreviewImgData {
        rgb_bytes,
        image_width: image.width(),
        image_height: image.height(),
        params: command.view_options.clone(),
    };
    send_result(
        ProcessingResultData::PreviewFrame(preview_data, command.mode_type),
        &command.camera,
        result_fun
    );

    let max_stars_fwhm = command.quality_options
        .as_ref()
        .and_then(|qo| if qo.use_max_fwhm { Some(qo.max_fwhm) } else { None });

    let max_stars_ovality = command.quality_options
        .as_ref()
        .and_then(|qo| if qo.use_max_ovality { Some(qo.max_ovality) } else { None });

    let is_bad_frame = if frame_type == FrameType::Lights {
        let tmr = TimeLogger::start();
        let info = LightImageInfo::from_image(
            &image,
            exposure,
            max_stars_fwhm,
            max_stars_ovality,
            true
        );
        tmr.log("TOTAL LightImageInfo::from_image");

        let mut light_info = LightFrameShortInfo::default();
        light_info.time = Utc::now();
        light_info.exposure = exposure;
        light_info.noise = 100.0 * info.noise / image.max_value() as f32;
        light_info.background = 100.0 * info.background as f32 / image.max_value() as f32;
        light_info.stars_fwhm = info.stars_fwhm;
        light_info.stars_ovality = info.stars_ovality;
        light_info.stars_count = info.stars.len();
        light_info.width = info.width;
        light_info.height = info.height;

        // Check taken frame is good or bad

        if !info.stars_fwhm_good {
            light_info.flags |= LightFrameShortInfoFlags::BAD_STARS_FWHM;
        }
        if !info.stars_ovality_good {
            light_info.flags |= LightFrameShortInfoFlags::BAD_STARS_OVAL;
        }

        let bad_frame = !info.stars_fwhm_good || !info.stars_ovality_good;

        // Stars offset
        if command.flags.contains(ProcessImageFlags::CALC_STARS_OFFSET) && !bad_frame {
             // Compare reference stars and new stars
            // and calculate offset and angle
            let cur_stars_points: Vec<_> = info.stars.iter()
                .map(|star| Point {x: star.x, y: star.y })
                .collect();

            let ref_stars = command.ref_stars.read().unwrap();
            let (angle, offset_x, offset_y) = if let Some(ref_stars) = &*ref_stars {
                let tmr = TimeLogger::start();
                let image_offset = Offset::calculate(
                    ref_stars,
                    &cur_stars_points,
                    image.width() as f64,
                    image.height() as f64
                );
                tmr.log("Offset::calculate");
                if let Some(image_offset) = image_offset {
                    (Some(image_offset.angle), Some(image_offset.x), Some(image_offset.y))
                } else {
                    light_info.flags |= LightFrameShortInfoFlags::BAD_OFFSET;
                    (None, None, None)
                }
            } else {
                drop(ref_stars);
                let mut ref_stars = command.ref_stars.write().unwrap();
                *ref_stars = Some(cur_stars_points);
                (Some(0.0), Some(0.0), Some(0.0))
            };

            light_info.offset_x = offset_x;
            light_info.offset_y = offset_y;
            light_info.angle = angle;
        }

        // Live stacking

        if let (Some(live_stacking), false) = (command.live_stacking.as_ref(), bad_frame) {
            // Translate/rotate image to reference image and add
            if let (Some(offset_x), Some(offset_y), Some(angle)) = (light_info.offset_x, light_info.offset_y, light_info.angle) {
                let mut image_adder = live_stacking.data.adder.write().unwrap();
                let tmr = TimeLogger::start();
                image_adder.add(&image, -offset_x, -offset_y, -angle, exposure, true);
                tmr.log("ImageAdder::add");
                drop(image_adder);

                let image_adder = live_stacking.data.adder.read().unwrap();

                let mut res_image = live_stacking.data.result.image.write().unwrap();
                let tmr = TimeLogger::start();
                image_adder.copy_to_image(&mut res_image, true);
                tmr.log("ImageAdder::copy_to_image");
                drop(res_image);

                let res_image = live_stacking.data.result.image.read().unwrap();

                // Histogram for live stacking image

                let mut hist = live_stacking.data.result.hist.write().unwrap();
                let tmr = TimeLogger::start();
                hist.from_image(&res_image, true);
                tmr.log("histogram from live view");
                drop(hist);
                let hist = live_stacking.data.result.hist.read().unwrap();
                send_result(
                    ProcessingResultData::HistogramLiveRes(command.mode_type),
                    &command.camera,
                    result_fun
                );

                // Live stacking image info

                let tmr = TimeLogger::start();
                let live_stacking_info = LightImageInfo::from_image(
                    &res_image,
                    image_adder.total_exposure(),
                    max_stars_fwhm,
                    max_stars_ovality,
                    true
                );
                tmr.log("LightImageInfo::from_image for livestacking");

                *live_stacking.data.result.info.write().unwrap() = ResultImageInfo::LightInfo(
                    live_stacking_info
                );
                send_result(
                    ProcessingResultData::FrameInfoLiveRes(command.mode_type),
                    &command.camera,
                    result_fun
                );

                // Convert into preview RGB bytes

                if !command.view_options.show_orig_frame {
                    let tmr = TimeLogger::start();
                    let rgb_bytes = get_rgb_bytes_from_preview_image(
                        &res_image,
                        &hist,
                        &command.view_options
                    );
                    tmr.log("get_rgb_bytes_from_preview_image");
                    let preview_data = PreviewImgData {
                        rgb_bytes,
                        image_width: image.width(),
                        image_height: image.height(),
                        params: command.view_options.clone(),
                    };
                    send_result(
                        ProcessingResultData::PreviewLiveRes(preview_data, command.mode_type),
                        &command.camera,
                        result_fun
                    );
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
                        image_adder.save_to_tiff(&file_path)?;
                        tmr.log("save live stacking result image");
                    }
                }
            }
        }

        // Send message with short light frame info

        send_result(
            ProcessingResultData::LightShortInfo(light_info, command.mode_type),
            &command.camera,
            result_fun
        );

        // Send message about light frame info stored

        *command.frame.info.write().unwrap() = ResultImageInfo::LightInfo(info);
        send_result(
            ProcessingResultData::FrameInfo(command.mode_type),
            &command.camera,
            result_fun
        );

        bad_frame
    } else {
        false
    };

    // Save original raw image
    if !is_bad_frame && command.flags.contains(ProcessImageFlags::SAVE_RAW) {
        if let Some(save_path) = command.save_path.as_ref() {
            let sub_path = match frame_type {
                FrameType::Lights => "Light",
                FrameType::Flats => "Flat",
                FrameType::Darks => "Dark",
                FrameType::Biases => "Bias",
                FrameType::Undef => unreachable!(),
            };
            let full_path = save_path.join(sub_path);
            if !full_path.is_dir() {
                std::fs::create_dir_all(&full_path)
                    .map_err(|e|anyhow::anyhow!(
                        "Error '{}'\nwhen trying to create directory '{}' for saving RAW frame",
                        e.to_string(),
                        full_path.to_str().unwrap_or_default()
                    ))?;
            }
            let mut fs_gen = command.fn_gen.lock().unwrap();
            let mut file_ext = command.blob.format.as_str().trim();
            while file_ext.starts_with('.') { file_ext = &file_ext[1..]; }
            let fn_mask = format!("{}_${{num}}.{}", sub_path, file_ext);
            let file_name = fs_gen.generate(&full_path, &fn_mask);
            let tmr = TimeLogger::start();
            std::fs::write(&file_name, command.blob.data.as_slice())
                .map_err(|e| anyhow::anyhow!(
                    "Error '{}'\nwhen saving file '{}'",
                    e.to_string(),
                    file_name.to_str().unwrap_or_default()
                ))?;
            tmr.log("Saving raw image");
        }
    }

    total_tmr.log("TOTAL PREVIEW");

    // Save master file

    if let (Some(raw_adder), Some(save_path)) = (command.raw_adder.as_ref(), command.save_path.as_ref()) {
        if raw_adder.save && frame_for_raw_adder {
            log::debug!("Saving master frame...");
            let mut adder = raw_adder.adder.lock().unwrap();
            let raw_image = adder.get()?;
            adder.clear();
            let (prefix, file_name_suff) = match frame_type {
                FrameType::Flats => ("flat", command.frame_options.create_master_flat_file_name_suff()),
                FrameType::Darks => ("dark", command.frame_options.create_master_dark_file_name_suff()),
                _ => unreachable!(),
            };
            let file_name = format!("{}_{}x{}-{}.fits", prefix, adder.width(), adder.height(), file_name_suff);
            let full_file_name = save_path.join(file_name);
            raw_image.save_to_fits_file(&full_file_name)?;
            send_result(
                ProcessingResultData::MasterSaved {
                    frame_type,
                    file_name: full_file_name
                },
                &command.camera,
                result_fun
            );
        }
        log::debug!("Master frame saved!");
    }

    send_result(
        ProcessingResultData::ShotProcessingFinished{
            mode_type:        command.mode_type,
            frame_is_ok: !is_bad_frame
        },
        &command.camera,
        result_fun
    );

    Ok(())
}

pub struct SeqFileNameGen {
    last_num: u32,
}

impl SeqFileNameGen {
    pub fn new() -> Self {
        Self {
            last_num: 1,
        }
    }

    pub fn clear(&mut self) {
        self.last_num = 1;
    }

    pub fn generate(&mut self, parent_path: &Path, file_mask: &str) -> PathBuf {
        loop {
            let num_str = format!("{:04}", self.last_num);
            let file_name = file_mask.replace("${num}", &num_str);
            let result = parent_path.join(file_name);
            self.last_num += 1;
            if !result.is_file() && !result.is_dir() {
                return result;
            }
        }
    }
}
