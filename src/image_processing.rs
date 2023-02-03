use std::{sync::Arc, sync::{mpsc, RwLock, Mutex}, thread::JoinHandle, path::*};

use chrono::{DateTime, Local};

use crate::{
    indi_api,
    image_raw::*,
    image::*,
    image_info::*,
    log_utils::*,
    stars_offset::*
};

pub enum ResultImageInfo {
    None,
    LightInfo(LightImageInfo),
    FlatInfo(FlatImageInfo),
    RawInfo(RawImageStat),
}

#[derive(Default)]
struct CalibrImages {
    master_dark:    Option<RawImage>,
    master_dark_fn: Option<PathBuf>,
    master_flat:    Option<RawImage>,
    master_flat_fn: Option<PathBuf>,
    bad_pixels:     Vec<BadPixel>,
}

pub struct ResultImage {
    pub image: RwLock<Image>,
    pub hist:  RwLock<Histogram>,
    pub info:  RwLock<ResultImageInfo>,
    calibr:    Mutex<CalibrImages>,
}

impl ResultImage {
    pub fn new() -> Self {
        Self {
            image:  RwLock::new(Image::new_empty()),
            hist:   RwLock::new(Histogram::new()),
            info:   RwLock::new(ResultImageInfo::None),
            calibr: Mutex::new(CalibrImages::default()),
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
    pub dark: Option<PathBuf>,
    pub flat: Option<PathBuf>,
}

pub struct PreviewImageCommand {
    pub camera:     String,
    pub blob:       Arc<indi_api::BlobPropValue>,
    pub frame:      Arc<ResultImage>,
    pub calibr:     CalibrParams,
    pub fn_gen:     Arc<Mutex<SeqFileNameGen>>,
    pub options:    PreviewParams,
    pub save_path:  Option<PathBuf>,
    pub live_view:  bool,
    pub raw_adder:  Arc<Mutex<Option<RawAdder>>>,
    pub result_fun: Box<dyn Fn(FrameProcessingResult) + Send + 'static>,
}

impl PreviewImageCommand {
    fn send_result(&self, data: ProcessingResultData) {
        (self.result_fun)(FrameProcessingResult {
            camera: self.camera.clone(),
            data
        });
    }
}

pub struct PreviewImgData {
    pub rgb_bytes:    RgbU8Data,
    pub image_width:  usize,
    pub image_height: usize,
    pub params:       PreviewParams,
}

#[derive(Default, Debug)]
pub struct LightFileShortInfo {
    pub stars_fwhm:    Option<f32>,
    pub stars_ovality: Option<f32>,
    pub stars_count:   usize,
    pub noise:         f32, // %
    pub background:    f32, // %
    pub offset_x:      Option<f32>,
    pub offset_y:      Option<f32>,
    pub angle:         Option<f32>,
}

pub struct LiveStackingData {
    pub adder:      RwLock<ImageAdder>,
    pub ref_stars:  RwLock<Option<Vec<Point>>>,
    pub result:     ResultImage,
    pub write_time: Mutex<Option<std::time::Instant>>,
}

impl LiveStackingData {
    pub fn new() -> Self {
        Self {
            adder:      RwLock::new(ImageAdder::new()),
            ref_stars:  RwLock::new(None),
            result:     ResultImage::new(),
            write_time: Mutex::new(None),
        }
    }

    pub fn clear(&mut self) {
        self.adder.write().unwrap().clear();
    }
}

pub struct LiveStackingCommand {
    pub camera:           String,
    pub blob:             Arc<indi_api::BlobPropValue>,
    pub frame:            Arc<ResultImage>,
    pub calibr:           CalibrParams,
    pub data:             Arc<LiveStackingData>,
    pub fn_gen:           Arc<Mutex<SeqFileNameGen>>,
    pub preview_params:   PreviewParams,
    pub max_fwhm:         Option<f32>,
    pub max_ovality:      Option<f32>,
    pub min_stars:        Option<usize>,
    pub save_path:        PathBuf,
    pub save_orig_frames: bool,
    pub save_res_interv:  Option<usize>,
    pub result_fun:       Box<dyn Fn(FrameProcessingResult) + Send + 'static>,
}


impl LiveStackingCommand {
    fn send_result(&self, data: ProcessingResultData) {
        (self.result_fun)(FrameProcessingResult {
            camera: self.camera.clone(),
            data
        });
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum ResultMode {
    OneShot,
    LiveView,
    RawFrame,
    LiveFrame,
    LiveResult,
}

pub enum ProcessingResultData {
    Error(String),
    SingleShotFinished,

    LightShortInfo(LightFileShortInfo), // for history

    Preview(PreviewImgData, ResultMode),
    FrameInfo(ResultMode),
    Histogram(ResultMode),
}

pub struct FrameProcessingResult {
    pub camera: String,
    pub data:   ProcessingResultData,
}

pub enum Command {
    PreviewImage(PreviewImageCommand),
    LiveStacking(LiveStackingCommand),
    Exit
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::PreviewImage(_) => "PreviewImage",
            Command::LiveStacking(_) => "LiveStacking",
            Command::Exit            => "Exit",
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
            Command::PreviewImage(command) =>
                make_preview_image(command),
            Command::LiveStacking(command) =>
                append_image_for_live_stacking(command),
        };
    }
}

fn create_raw_image_from_blob(
    blob_prop_value: &Arc<indi_api::BlobPropValue>
) -> anyhow::Result<RawImage> {
    if blob_prop_value.format == ".fits" {
        use fitsio::{sys, FileOpenMode, FitsFile};
        let mut ptr = blob_prop_value.data.as_ptr();
        let mut ptr_size = blob_prop_value.data.len() as sys::size_t;
        let mut fptr = std::ptr::null_mut();
        let mut status = 0;
        let c_filename = std::ffi::CString::new("filename.fits").unwrap();
        unsafe {
            sys::ffomem(
                &mut fptr as *mut *mut _,
                c_filename.as_ptr(),
                sys::READONLY as _,
                &mut ptr as *const _ as *mut *mut libc::c_void,
                &mut ptr_size as *mut sys::size_t,
                0,
                None,
                &mut status,
            );
        }
        if status != 0 {
            unsafe { sys::ffrprt(sys::stderr, status) };
            panic!("bad status: {}", status); // TODO: return error
        }
        let mut f = unsafe { FitsFile::from_raw(
            fptr,
            FileOpenMode::READONLY
        )}.unwrap();
        let raw_image = RawImage::new_from_fits(&mut f);
        return raw_image
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

fn apply_calibr_data(
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
            calibr.bad_pixels = master_dark.find_hot_pixels_in_master_dark();
            tmr.log("searching hot pixels");
            log::debug!("hot pixels count = {}", calibr.bad_pixels.len());
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
        raw_image.remove_bad_pixels(&calibr.bad_pixels);
        tmr.log("removing bad pixels from light frame");
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
            raw_image.remove_bad_pixels(&calibr.bad_pixels);
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

fn make_preview_image(command: PreviewImageCommand) {
    let res = make_preview_image_impl(&command);
    match res {
        Ok(Some(light_info)) => {
            command.send_result(ProcessingResultData::LightShortInfo(
                light_info
            ));
        },
        Err(err) => {
            command.send_result(ProcessingResultData::Error(
                err.to_string()
            ));
        },
        _ => {},
    }
}

fn add_calibr_image(
    raw_image:  &mut RawImage,
    adder:      &Arc<Mutex<Option<RawAdder>>>,
    frame_type: FrameType
) -> anyhow::Result<()> {
    let mut adder = adder.lock().unwrap();
    let Some(adder) = &mut *adder else { return Ok(()); };

    if frame_type == FrameType::Flat {
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
    command: &PreviewImageCommand
) -> anyhow::Result<Option<LightFileShortInfo>> {
    let total_tmr = TimeLogger::start();

    let tmr = TimeLogger::start();
    let mut raw_image = create_raw_image_from_blob(&command.blob)?;
    tmr.log("create_raw_image_from_blob");
    let exposure = raw_image.info().exposure;

    let frame_type = raw_image.info().frame_type;

    let is_monochrome_img =
        matches!(frame_type, FrameType::Bias) ||
        matches!(frame_type, FrameType::Dark);

    // Applying calibration data
    if frame_type == FrameType::Light {
        let mut calibr = command.frame.calibr.lock().unwrap();
        apply_calibr_data(&command.calibr, &mut raw_image, &mut calibr, true)?;
    }

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

    let mode = if command.save_path.is_some() {
        ResultMode::RawFrame
    } else if command.live_view {
        ResultMode::LiveView
    } else  {
        ResultMode::OneShot
    };

    command.send_result(ProcessingResultData::Histogram(
        mode
    ));

    match frame_type {
        FrameType::Flat => {
            let hist = command.frame.hist.read().unwrap();
            *command.frame.info.write().unwrap() = ResultImageInfo::FlatInfo(
                FlatImageInfo::from_histogram(&hist)
            );
            command.send_result(ProcessingResultData::FrameInfo(
                mode
            ));
        },
        FrameType::Dark | FrameType::Bias => {
            let hist = command.frame.hist.read().unwrap();
            *command.frame.info.write().unwrap() = ResultImageInfo::RawInfo(
                RawImageStat::from_histogram(&hist)
            );
            command.send_result(ProcessingResultData::FrameInfo(
                mode
            ));
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

    if let FrameType::Flat| FrameType::Dark | FrameType::Bias = frame_type {
        add_calibr_image(&mut raw_image, &command.raw_adder, frame_type)?;
    }

    drop(raw_image);
    drop(image);

    // Preview image RGB bytes

    let image = command.frame.image.read().unwrap();
    let hist = command.frame.hist.read().unwrap();
    let tmr = TimeLogger::start();
    let rgb_bytes =
        get_rgb_bytes_from_preview_image(&image, &hist, &command.options);
    tmr.log("get_rgb_bytes_from_preview_image");

    let preview_data = PreviewImgData {
        rgb_bytes,
        image_width: image.width(),
        image_height: image.height(),
        params: command.options.clone(),
    };
    command.send_result(ProcessingResultData::Preview(
        preview_data,
        mode
    ));

    let mut light_info_result: Option<LightFileShortInfo> = None;
    if frame_type == FrameType::Light {
        let tmr = TimeLogger::start();
        let info = LightImageInfo::from_image(&image, exposure, true);
        let mut light_info = LightFileShortInfo::default();
        light_info.noise = 100.0 * info.noise / image.max_value() as f32;
        light_info.background = 100.0 * info.background as f32 / image.max_value() as f32;
        light_info.stars_fwhm = info.stars_fwhm;
        light_info.stars_ovality = info.stars_ovality;
        light_info.stars_count = info.stars.len();
        light_info_result = Some(light_info);
        *command.frame.info.write().unwrap() = ResultImageInfo::LightInfo(info);
        tmr.log("TOTAL LightImageInfo::from_image");
        command.send_result(ProcessingResultData::FrameInfo(
            mode
        ));
    }

    if let Some(save_path) = command.save_path.as_ref() {
        let sub_path = match frame_type {
            FrameType::Light => "Light",
            FrameType::Flat => "Flat",
            FrameType::Dark => "Dark",
            FrameType::Bias => "Bias",
        };
        let full_path = save_path.join(sub_path);
        if !full_path.is_dir() {
            std::fs::create_dir_all(&full_path)
                .map_err(|e|anyhow::anyhow!(
                    "Error '{}'\nwhen trying to create directory '{}'",
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

    total_tmr.log("TOTAL PREVIEW");

    command.send_result(ProcessingResultData::SingleShotFinished);

    Ok(light_info_result)
}

fn append_image_for_live_stacking(command: LiveStackingCommand) {
    let res = append_image_for_live_stacking_impl(&command);
    match res {
        Ok(Some(light_info)) => {
            command.send_result(ProcessingResultData::LightShortInfo(
                light_info
            ));
        },
        Err(err) => {
            command.send_result(ProcessingResultData::Error(
                err.to_string()
            ));
        },
        _ => {},
    }
}

fn append_image_for_live_stacking_impl(
    command: &LiveStackingCommand
) -> anyhow::Result<Option<LightFileShortInfo>> {
    let total_tmr = TimeLogger::start();

    // FITS blob -> raw image

    let tmr = TimeLogger::start();
    let mut raw_image = create_raw_image_from_blob(&command.blob)
        .map_err(|e| anyhow::anyhow!(
            "Error: {} when extracting image from indi server blob\nPossible camera is not in RAW mode",
            e.to_string()
        ))?;
    tmr.log("create_raw_image_from_blob");
    let exposure = raw_image.info().exposure;

    // Histogram

    let mut hist = command.frame.hist.write().unwrap();
    let tmr = TimeLogger::start();
    hist.from_raw_image(&raw_image, false, true);
    tmr.log("histogram from raw image");
    drop(hist);
    let hist = command.frame.hist.read().unwrap();
    command.send_result(
        ProcessingResultData::Histogram(ResultMode::LiveFrame)
    );

    // Applying calibration data

    let mut calibr = command.frame.calibr.lock().unwrap();
    apply_calibr_data(&command.calibr, &mut raw_image, &mut calibr, true)?;
    drop(calibr);

    // Demosaic

    let mut image = command.frame.image.write().unwrap();
    let tmr = TimeLogger::start();
    raw_image.demosaic_into(&mut image, true);
    tmr.log("demosaic");
    drop(raw_image);
    drop(image);

    let image = command.frame.image.read().unwrap();

    // Preview current frame

    if command.preview_params.show_orig_frame {
        let tmr = TimeLogger::start();
        let rgb_bytes = get_rgb_bytes_from_preview_image(
            &image,
            &hist,
            &command.preview_params
        );
        tmr.log("get_rgb_bytes_from_preview_image");

        let preview_data = PreviewImgData {
            rgb_bytes,
            image_width: image.width(),
            image_height: image.height(),
            params: command.preview_params.clone(),
        };

        command.send_result(ProcessingResultData::Preview(
            preview_data,
            ResultMode::LiveFrame
        ));
    }

    // Image info and stars

    let mut light_info = LightFileShortInfo::default();

    let tmr = TimeLogger::start();
    let light_frame_info = LightImageInfo::from_image(&image, exposure, true);
    tmr.log("LightImageInfo::from_image");

    light_info.noise = 100.0 * light_frame_info.noise / image.max_value() as f32;
    light_info.background = 100.0 * light_frame_info.background as f32 / image.max_value() as f32;
    light_info.stars_fwhm = light_frame_info.stars_fwhm;
    light_info.stars_ovality = light_frame_info.stars_ovality;
    light_info.stars_count = light_frame_info.stars.len();

    let cur_stars_points: Vec<_> = light_frame_info.stars.iter()
        .map(|star| Point {x: star.x, y: star.y })
        .collect();

    // Check taken frame is good or bad

    let bad_stars_fwhm = match (light_frame_info.stars_fwhm, command.max_fwhm) {
        (None, _)               => true,
        (Some(fwhm), Some(max)) => fwhm > max,
        _                       => false,
    };
    let bad_stars_ovality = match (light_frame_info.stars_ovality, command.max_ovality) {
        (None, _)                  => true,
        (Some(ovality), Some(max)) => ovality > max,
        _                          => false,
    };
    let bad_stars_cnt = match command.min_stars {
        Some(min) => light_frame_info.stars.len() < min,
        _         => false,
    };
    let is_bad_frame = bad_stars_fwhm || bad_stars_ovality || bad_stars_cnt;

    // Inform about frame information

    *command.frame.info.write().unwrap() = ResultImageInfo::LightInfo(
        light_frame_info
    );
    command.send_result(ProcessingResultData::FrameInfo(
        ResultMode::LiveFrame
    ));

    if is_bad_frame {
        return Ok(Some(light_info));
    }

    // Compare reference stars and new stars
    // and calculate offset and angle

    let ref_stars = command.data.ref_stars.read().unwrap();
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
            (image_offset.angle, image_offset.x, image_offset.y)
        } else {
            return Ok(Some(light_info));
        }
    } else {
        drop(ref_stars);
        let mut ref_stars = command.data.ref_stars.write().unwrap();
        *ref_stars = Some(cur_stars_points);
        (0.0, 0.0, 0.0)
    };

    light_info.offset_x = Some(offset_x as f32);
    light_info.offset_y = Some(offset_y as f32);
    light_info.angle = Some(angle as f32);

    // Translate/rotate image to reference image and add

    let mut image_adder = command.data.adder.write().unwrap();
    let tmr = TimeLogger::start();
    image_adder.add(&image, -offset_x, -offset_y, -angle, exposure, true);
    tmr.log("ImageAdder::add");
    drop(image_adder);

    let image_adder = command.data.adder.read().unwrap();

    let mut res_image = command.data.result.image.write().unwrap();
    let tmr = TimeLogger::start();
    image_adder.copy_to_image(&mut res_image, true);
    tmr.log("ImageAdder::copy_to_image");
    drop(res_image);

    let res_image = command.data.result.image.read().unwrap();

    // Histogram for live stacking image

    let mut hist = command.data.result.hist.write().unwrap();
    let tmr = TimeLogger::start();
    hist.from_image(&res_image, true);
    tmr.log("histogram from live view");
    drop(hist);
    let hist = command.data.result.hist.read().unwrap();
    command.send_result(ProcessingResultData::Histogram(
        ResultMode::LiveResult
    ));

    // Live stacking image info

    let tmr = TimeLogger::start();
    let live_stacking_info = LightImageInfo::from_image(
        &res_image,
        image_adder.total_exposure(),
        true
    );
    tmr.log("LightImageInfo::from_image for livestacking");

    log::debug!("live_stacking_info.stars.len()={}", live_stacking_info.stars.len());

    *command.data.result.info.write().unwrap() = ResultImageInfo::LightInfo(
        live_stacking_info
    );
    command.send_result(ProcessingResultData::FrameInfo(
        ResultMode::LiveResult
    ));

    // Convert into preview RGB bytes

    if !command.preview_params.show_orig_frame {
        let tmr = TimeLogger::start();
        let rgb_bytes = get_rgb_bytes_from_preview_image(
            &res_image,
            &hist,
            &command.preview_params
        );
        tmr.log("get_rgb_bytes_from_preview_image");
        let preview_data = PreviewImgData {
            rgb_bytes,
            image_width: image.width(),
            image_height: image.height(),
            params: command.preview_params.clone(),
        };
        command.send_result(ProcessingResultData::Preview(
            preview_data,
            ResultMode::LiveResult
        ));
    }

    // Save original image

    if command.save_orig_frames {
        let mut fn_gen = command.fn_gen.lock().unwrap();
        let mut file_ext = command.blob.format.as_str().trim();
        while file_ext.starts_with('.') { file_ext = &file_ext[1..]; }
        let fn_mask = format!("Light_${{num}}.{}", file_ext);
        let orig_path = command.save_path.join("Original");
        if !orig_path.is_dir() {
            std::fs::create_dir_all(&orig_path)
                .map_err(|e|anyhow::anyhow!(
                    "Error '{}'\nwhen trying to create directory '{}'",
                    e.to_string(),
                    orig_path.to_str().unwrap_or_default()
                ))?;

        }
        let file_name = fn_gen.generate(&orig_path, &fn_mask);
        let tmr = TimeLogger::start();
        std::fs::write(&file_name, command.blob.data.as_slice())
            .map_err(|e|anyhow::anyhow!(
                "Error '{}'\nwhen saving file '{}'",
                e.to_string(),
                file_name.to_str().unwrap_or_default()
            ))?;
        tmr.log("save original raw image");
    }

    // save result image

    if let Some(save_res_interv) = command.save_res_interv {
        let mut last_save = command.data.write_time.lock().unwrap();
        let have_to_save = if let Some(last_save) = &*last_save {
            last_save.elapsed().as_secs() >= save_res_interv as u64
        } else {
            true
        };
        if have_to_save {
            let now_time: DateTime<Local> = Local::now();
            let now_time_str = now_time.format("%Y%m%d-%H%M%S").to_string();
            let file_path = command.save_path.join(format!("Live_{}.tif", now_time_str));
            let tmr = TimeLogger::start();
            image_adder.save_to_tiff(&file_path)?;
            tmr.log("save live stacking result image");
            *last_save = Some(std::time::Instant::now());
        }
    }

    total_tmr.log("TOTAL LIVE STACKING");
    Ok(Some(light_info))
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
