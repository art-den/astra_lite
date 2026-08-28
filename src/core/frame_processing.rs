use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, sync::{mpsc, Mutex}, path::*};

use chrono::{DateTime, Local};
use bitflags::bitflags;

use crate::{
    core::{
        engine::ModeKind,
        live_stacking::{LiveStackedImageInfo, LiveStacking},
        preview::{Preview, ResultImageInfo},
        raw_calibration::{CalibrParams, RawCalibration}
    },
    hal::{CameraShot, CameraShotType, FrameType},
    image::{
        histogram::*, info::*,
        preview::*, raw::*,
        stars::{Stars, StarsFinder}, stars_offset::*,
    },
    options::*,
    utils::log_utils::*,
};

#[derive(Clone)]
pub struct FrameQuality {
    pub ccd_temp_ok:   bool,
    pub offset_is_ok:  bool,
    pub fwhm_is_ok:    bool,
    pub ovality_is_ok: bool,
}

impl Default for FrameQuality {
    fn default() -> Self {
        Self {
            ccd_temp_ok:   true,
            offset_is_ok:  true,
            fwhm_is_ok:    true,
            ovality_is_ok: true,
        }
    }
}

impl FrameQuality {
    pub fn is_ok(&self) -> bool {
        self.ccd_temp_ok &&
        self.offset_is_ok &&
        self.fwhm_is_ok &&
        self.ovality_is_ok
    }

    pub fn stars_is_ok(&self) -> bool {
        self.fwhm_is_ok &&
        self.ovality_is_ok
    }
}

pub struct LightFrameResult {
    pub raw:     Option<RawImageInfo>,
    pub image:   Arc<LightFrameInfo>,
    pub stars:   Arc<Stars>,
    pub offset:  Option<Offset>,
    pub quality: FrameQuality,
}

pub struct LiveStackingCtx {
    pub data:    Arc<LiveStacking>,
    pub options: LiveStackingOptions,
}

bitflags! {
    pub struct FrameProcessCommandFlags: u32 {
        const HISTOGRAM_ONLY = 1;
    }
}

pub struct ProcessImageParams {
    pub mode_kind:       ModeKind,
    pub camera_id:       String,
    pub img_source:      Arc<dyn CameraShot + Send + Sync>,
    pub flags:           FrameProcessCommandFlags,
    pub preview:         Arc<Preview>,
    pub stop_flag:       Arc<AtomicBool>,
    pub ref_stars:       Option<Vec<Point>>,
    pub calibr_params:   Option<CalibrParams>,
    pub calibr_data:     Arc<Mutex<RawCalibration>>,
    pub view_options:    PreviewParams,
    pub frame_options:   FrameOptions,
    pub cam_ctrl_opts:   Option<CamCtrlOptions>,
    pub quality_options: Option<QualityOptions>,
    pub live_stacking:   Option<LiveStackingCtx>,
}

pub struct PreviewImage {
    pub rgb_data: PreviewRgbData,
    pub params:   PreviewParams,
}

#[derive(Clone)]
pub struct RawFrameResult {
    pub image:       Arc<RawImage>,
    pub ccd_temp_ok: bool,
    pub mean:        f32,
    pub median:      u16,
    pub std_dev:     f32,
}

impl RawFrameResult {
    pub fn quality_is_ok(&self) -> bool {
        self.ccd_temp_ok
    }
}

#[derive(Clone)]
pub enum FrameProcessEvent {
    ShotProcessingStarted,
    RawFrameReady(RawFrameResult),
    RawHistogramReady,
    ImageReady,
    PreviewOrigFrame(Arc<PreviewImage>),
    PreviewLiveStacking(Arc<PreviewImage>),
    LightFrameReady(Arc<LightFrameResult>),
    OrigFrameInfoReady,
    LiveStackingInfoReady,
    LiveStackingHistogramReady,
    MasterSaved {
        frame_type: FrameType,
        file_name: PathBuf
    },
    ShotProcessingFinished {
        frame_is_ok:     bool,
        camera_shot:     Arc<dyn CameraShot + Send + Sync>,
        raw_image_info:  Arc<RawImageInfo>,
        processing_time: f64,
    },
}

#[derive(Clone)]
pub struct FrameProcessNotification {
    pub camera_id: String,
    pub mode_kind: ModeKind,
    pub event:     FrameProcessEvent,
}

#[derive(Clone)]
pub enum FrameProcessingReply {
    Result(FrameProcessNotification),
    Error(String),
    QueueOverflow,
}

pub type ResultFun = Box<dyn Fn(FrameProcessingReply) + Send + 'static>;

pub enum FrameProcessCommand {
    ProcessImage(ProcessImageParams),
    Stop
}

pub struct FrameProcessing {
    sender:     mpsc::Sender<FrameProcessCommand>,
    result_fun: Mutex<Option<ResultFun>>,
}

impl FrameProcessing {
    pub fn new() -> Arc<Self> {
        let (sender, receiver) = mpsc::channel();
        let this = Arc::new(FrameProcessing{ sender, result_fun: Mutex::new(None) });
        let weak_this = Arc::downgrade(&this);
        std::thread::spawn(move || {
            log::info!("process_blob_thread_fun started");
            'outer:
            while let Ok(cmd) = receiver.recv() {
                if matches!(cmd, FrameProcessCommand::Stop) { break 'outer; }
                let mut commands = Vec::new();
                commands.push(cmd);
                loop {
                    let next_cmd = receiver.try_recv();
                    match next_cmd {
                        Ok(next_cmd) => {
                            if matches!(next_cmd, FrameProcessCommand::Stop) { break 'outer; }
                            commands.push(next_cmd);
                        },
                        Err(mpsc::TryRecvError::Disconnected) => {
                            break 'outer;
                        },
                        Err(mpsc::TryRecvError::Empty) => {
                            break;
                        },
                    }
                }

                if commands.len() > 1 {
                    log::info!("Commands queue size > 1 ({})", commands.len());
                }

                let queue_is_overflowed = commands.len() >= 3;
                if queue_is_overflowed {
                    let Some(self_) = weak_this.upgrade() else { break 'outer; };
                    self_.notify_cmd_result(FrameProcessingReply::QueueOverflow);
                }

                for cmd in commands {
                    if let FrameProcessCommand::ProcessImage(cmd) = cmd {
                        let Some(self_) = weak_this.upgrade() else { break 'outer; };
                        let process_cmd_res = self_.process_command(cmd);
                        if let Err(err) = process_cmd_res {
                            self_.notify_cmd_result(FrameProcessingReply::Error(err.to_string()));
                        }
                    }
                }
            }

            log::info!("process_blob_thread_fun finished");
        });

        this
    }

    pub fn connect_result_fun(&self, fun: impl Fn(FrameProcessingReply) + Send + 'static) {
        let mut result_fun = self.result_fun.lock().unwrap();
        *result_fun = Some(Box::new(fun));
    }

    pub fn add_to_queue(&self, cmd: FrameProcessCommand) -> eyre::Result<()> {
        self.sender.send(cmd)?;
        Ok(())
    }

    fn notify_cmd_result(&self, result: FrameProcessingReply) {
        let result_fun_mutex = self.result_fun.lock().unwrap();
        let result_fun = result_fun_mutex.as_ref().expect("FrameProcessing::result_fun");
        result_fun(result);
    }

    fn notify_frame_result(&self, result: FrameProcessEvent, command: &ProcessImageParams) {
        self.notify_cmd_result(FrameProcessingReply::Result(FrameProcessNotification {
            camera_id: command.camera_id.clone(),
            mode_kind: command.mode_kind,
            event:     result
        }));
    }

    fn process_command(&self, command: ProcessImageParams) -> eyre::Result<()> {
        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        let total_tmr = TimeLogger::start();

        self.notify_frame_result(
            FrameProcessEvent::ShotProcessingStarted,
            &command,
        );

        let mut frame_type = FrameType::Lights;
        let mut is_light_frame = true;
        let mut exposure = 0_f64;
        let mut raw_info = None;
        let mut raw_noise = None;

        let mut quality = FrameQuality::default();

        let mut image = match command.img_source.get_type() {
            crate::hal::CameraShotType::RawCcdData => {
                let mut raw_image = command.img_source.get_raw()?;
                let mut info = raw_image.info().clone();
                if info.offset == 0 {
                    info.offset = command.frame_options.offset;
                    raw_image.set_offset(info.offset);
                }

                frame_type = info.frame_type;
                exposure = info.exposure;
                is_light_frame = frame_type == FrameType::Lights;

                log::debug!("Raw type      = {:?}", frame_type);
                log::debug!("Raw width     = {}",   info.width);
                log::debug!("Raw height    = {}",   info.height);
                log::debug!("Raw zero      = {}",   info.offset);
                log::debug!("Raw max_value = {}",   info.max_value);
                log::debug!("Raw CFA       = {:?}", info.cfa);
                log::debug!("Raw bin       = {}",   info.bin);
                log::debug!("Raw exposure  = {}s",  info.exposure);
                log::debug!("Raw CCD temp  = {:?}", info.ccd_temp);

                if command.stop_flag.load(Ordering::Relaxed) {
                    log::debug!("Command stopped");
                    return Ok(());
                }

                // Check if frame CCD temperature is good

                if let Some(qo) = &command.quality_options
                && let Some(ctrl_o) = &command.cam_ctrl_opts
                && ctrl_o.enable_cooler && qo.check_ccd_temp
                && let Some(ccd_temp) = info.ccd_temp {
                    let diff = f64::abs(ccd_temp - ctrl_o.temperature);
                    quality.ccd_temp_ok = diff <= qo.max_ccd_temp_diff;
                }

                let is_monochrome_img =
                    matches!(frame_type, FrameType::Biases) ||
                    matches!(frame_type, FrameType::Darks);

                // Raw histogram (before applying calibration data)

                let mut raw_hist = command.preview.raw_hist.write().unwrap();
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

                let chan = if let Some(chan) = &raw_hist.l {
                    chan
                } else if let Some(chan) = &raw_hist.g {
                    chan
                } else {
                    unreachable!();
                };

                let raw_mean = chan.mean;
                let raw_median = chan.median();
                let raw_std_dev = chan.std_dev;

                drop(raw_hist);

                self.notify_frame_result(
                    FrameProcessEvent::RawHistogramReady,
                    &command,
                );

                if command.flags.contains(FrameProcessCommandFlags::HISTOGRAM_ONLY) {
                    return Ok(());
                }

                // Applying calibration data
                if is_light_frame
                || frame_type == FrameType::Flats {
                    let mut calibr = command.calibr_data.lock().unwrap();
                    calibr.apply_calibr_data_and_remove_hot_pixels(
                        &command.calibr_params,
                        &mut raw_image,
                    )?;
                    info = raw_image.info().clone()
                }

                let raw_image = Arc::new(raw_image);

                let raw_frame_info = RawFrameResult {
                    image:       Arc::clone(&raw_image),
                    ccd_temp_ok: quality.ccd_temp_ok,
                    mean:        raw_mean as f32,
                    median:      raw_median,
                    std_dev:     raw_std_dev as f32,
                };
                self.notify_frame_result(
                    FrameProcessEvent::RawFrameReady(raw_frame_info),
                    &command,
                );

                if command.stop_flag.load(Ordering::Relaxed) {
                    log::debug!("Command stopped");
                    return Ok(());
                }

                // Raw noise
                raw_noise = if is_light_frame {
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
                        let hist = command.preview.raw_hist.read().unwrap();
                        *command.preview.info.write().unwrap() = ResultImageInfo::FlatInfo(
                            FlatImageInfo::from_histogram(&hist)
                        );
                        self.notify_frame_result(
                            FrameProcessEvent::OrigFrameInfoReady,
                            &command,
                        );
                    },
                    FrameType::Darks | FrameType::Biases => {
                        let hist = command.preview.raw_hist.read().unwrap();
                        *command.preview.info.write().unwrap() = ResultImageInfo::RawInfo(
                            RawImageStat::from_histogram(&hist)
                        );
                        self.notify_frame_result(
                            FrameProcessEvent::OrigFrameInfoReady,
                            &command,
                        );
                    },

                    _ => {},
                }

                if command.stop_flag.load(Ordering::Relaxed) {
                    log::debug!("Command stopped");
                    return Ok(());
                }

                // Demosaic

                let mut image = command.preview.image.write().unwrap();

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
            }

            crate::hal::CameraShotType::ReadyImage => {
                let mut image = command.preview.image.write().unwrap();
                command.img_source.get_image(&mut image)?;
                image
            }
        };

        // Remove gradient from light frame

        if is_light_frame
        && (command.view_options.remove_gradient
        || command.mode_kind == ModeKind::LiveStacking) {
            let tmr = TimeLogger::start();
            image.remove_gradient();
            tmr.log("remove gradient from light frame");
        }

        drop(image);

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        self.notify_frame_result(
            FrameProcessEvent::ImageReady,
            &command,
        );

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        // Result image histogram

        let image = command.preview.image.read().unwrap();
        let mut hist = command.preview.img_hist.write().unwrap();
        let tmr = TimeLogger::start();
        hist.from_image(&image);
        tmr.log("histogram for result image");

        if command.img_source.get_type() == CameraShotType::ReadyImage {
            *command.preview.raw_hist.write().unwrap() = hist.clone();
            self.notify_frame_result(
                FrameProcessEvent::RawHistogramReady,
                &command,
            );
        }

        drop(hist);

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        // Stars

        let frame_stars = if is_light_frame {
            let stars_recgn_send = command.quality_options
                .as_ref().map(|qo| qo.star_recogn_sens)
                .unwrap_or_default();

            let mono_layer = if image.is_color() { &image.g } else { &image.l };
            let mut stars_finder = StarsFinder::new();
            let ignore_3px_stars = command.quality_options
                .as_ref()
                .map(|opts| opts.ignore_3px_stars)
                .unwrap_or(false);

            stars_finder.find_stars_and_get_info(
                mono_layer,
                &image.raw_info,
                stars_recgn_send,
                ignore_3px_stars,
                true
            )
        } else {
            Stars::default()
        };

        // Preview image RGB bytes

        let hist = command.preview.img_hist.read().unwrap();
        let tmr = TimeLogger::start();
        let rgb_data = get_preview_rgb_data(
            &image,
            &hist,
            &command.view_options,
            if is_light_frame { Some(&frame_stars.items)} else { None },
        );
        tmr.log("get_rgb_bytes_from_preview_image");

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        if let Some(rgb_data) = rgb_data {
            let preview_data = Arc::new(PreviewImage {
                rgb_data,
                params: command.view_options.clone(),
            });
            self.notify_frame_result(
                FrameProcessEvent::PreviewOrigFrame(preview_data),
                &command,
            );
        }

        if frame_type == FrameType::Lights {
            let stars = Arc::new(frame_stars);

            // Stars quality

            if let Some(qo) = &command.quality_options {
                if qo.use_max_fwhm && let Some(fwhm) = stars.info.fwhm {
                    quality.fwhm_is_ok = fwhm < qo.max_fwhm;
                }
                if qo.use_max_ovality && let Some(ovality) = stars.info.ovality {
                    quality.ovality_is_ok = ovality < qo.max_ovality;
                }
            }

            // Light frame information

            let tmr = TimeLogger::start();
            let mut info = LightFrameInfo::from_image(
                &image,
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

            // Offset by previous stars

            let stars_offset =
                if let (Some(stars_for_offset), true) = (&command.ref_stars, quality.stars_is_ok()) {
                    let tmr = TimeLogger::start();
                    let cur_stars_points: Vec<_> = stars.items.iter()
                        .map(|star| Point {x: star.x, y: star.y })
                        .collect();
                    let image_offset = Offset::calculate(
                        stars_for_offset,
                        &cur_stars_points,
                        image.width() as f64,
                        image.height() as f64
                    );
                    tmr.log("Offset::calculate");
                    quality.offset_is_ok = image_offset.is_some();
                    image_offset
                } else {
                    None
                };

            let info = Arc::new(LightFrameResult {
                raw: raw_info.clone(),
                image: Arc::new(info),
                stars: Arc::clone(&stars),
                offset: stars_offset,
                quality: quality.clone(),
            });

            // Send message about calculated light frame

            self.notify_frame_result(
                FrameProcessEvent::LightFrameReady(Arc::clone(&info)),
                &command,
            );

            // Send message about light frame info stored

            *command.preview.info.write().unwrap() = ResultImageInfo::LightInfo(Arc::clone(&info));
            self.notify_frame_result(
                FrameProcessEvent::OrigFrameInfoReady,
                &command,
            );

            // Live stacking

            if let (Some(live_stacking), true) = (&command.live_stacking, quality.is_ok()) {
                // Translate/rotate image to reference image and add
                let offset = info.offset.clone().unwrap_or_default();
                let mut stacker = live_stacking.data.stacker.write().unwrap();
                let tmr = TimeLogger::start();
                stacker.add(
                    &image,
                    &hist,
                    -offset.x,
                    -offset.y,
                    -offset.angle,
                    exposure,
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
                self.notify_frame_result(
                    FrameProcessEvent::LiveStackingHistogramReady,
                    &command,
                );

                // Stars on live stacking image

                let ls_mono_layer = if res_image.is_color() {
                    &res_image.g
                } else {
                    &res_image.l
                };

                let ignore_3px_stars = command.quality_options
                    .as_ref()
                    .map(|opts| opts.ignore_3px_stars)
                    .unwrap_or(false);

                let stars_recgn_send = command.quality_options
                    .as_ref().map(|qo| qo.star_recogn_sens)
                    .unwrap_or_default();

                let mut stars_finder = StarsFinder::new();
                let ls_stars = stars_finder.find_stars_and_get_info(
                    ls_mono_layer,
                    &raw_info,
                    stars_recgn_send,
                    ignore_3px_stars,
                    true
                );

                // Live stacking image info

                let tmr = TimeLogger::start();
                let mut live_stacking_info = LightFrameInfo::from_image(&res_image, true);
                live_stacking_info.exposure = stacker.total_exposure();
                tmr.log("LightImageInfo::from_image for live stacking");

                if command.stop_flag.load(Ordering::Relaxed) {
                    log::debug!("Command stopped");
                    return Ok(());
                }

                let ls_light_frame_info = LiveStackedImageInfo {
                    image: Arc::new(live_stacking_info),
                    stars: Arc::new(ls_stars),
                };

                //let ls_light_frame_info = Arc::new(ls_light_frame_info);

                *live_stacking.data.info.write().unwrap() = Some(ls_light_frame_info.clone());
                self.notify_frame_result(
                    FrameProcessEvent::LiveStackingInfoReady,
                    &command,
                );

                // Convert to RGB bytes for preview

                if !command.view_options.orig_frame_in_ls {
                    let tmr = TimeLogger::start();
                    let rgb_data = get_preview_rgb_data(
                        &res_image,
                        &hist,
                        &command.view_options,
                        Some(&ls_light_frame_info.stars.items),
                    );
                    tmr.log("get_rgb_bytes_from_preview_image");

                    if command.stop_flag.load(Ordering::Relaxed) {
                        log::debug!("Command stopped");
                        return Ok(());
                    }

                    if let Some(rgb_data) = rgb_data {
                        let preview_data = Arc::new(PreviewImage {
                            rgb_data,
                            params: command.view_options.clone(),
                        });

                        self.notify_frame_result(
                            FrameProcessEvent::PreviewLiveStacking(preview_data),
                            &command,
                        );
                    }
                }

                // Save result image

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
                                .map_err(|e|eyre::eyre!(
                                    "Error '{}'\nwhen trying to create directory '{}' for saving live stacking result image",
                                    e, file_path.to_str().unwrap_or_default(),
                                ))?;
                        }
                        let file_path = file_path.join(format!("Live_{}.tif", now_time_str));
                        let tmr = TimeLogger::start();
                        stacker.save_to_tiff(&file_path)?;
                        tmr.log("save live stacking result image");
                    }
                }
            }
        };

        if command.stop_flag.load(Ordering::Relaxed) {
            log::debug!("Command stopped");
            return Ok(());
        }

        let process_time = total_tmr.log("TOTAL PREVIEW");

        if let Some(raw_info) = raw_info {
            let result = FrameProcessEvent::ShotProcessingFinished{
                raw_image_info:  Arc::new(raw_info),
                frame_is_ok:     quality.is_ok(),
                camera_shot:     Arc::clone(&command.img_source),
                processing_time: process_time,
            };
            self.notify_frame_result(result, &command);
        }

        Ok(())
    }
}
