use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use astra_lite::{
    core::{
        core::{Core, ModeType},
        events::Event,
        frame_processing::{FrameProcessResult, FrameProcessResultData},
    },
    hal::FrameType,
    image::{
        io::save_raw_image_to_fits_file,
        raw::{CfaType, RawImage, RawImageInfo},
    },
};

/// Image dimensions for the synthetic FITS file.
const IMG_WIDTH: usize = 512;
const IMG_HEIGHT: usize = 512;

/// Maximum number of seconds to wait for processing to complete.
const TIMEOUT_SECS: i64 = 15;

/// Background pixel level in the synthetic RAW image.
const BACKGROUND_LEVEL: u16 = 100;

/// Exposure time set in the synthetic FITS header (seconds).
const EXPECTED_EXPOSURE: f64 = 10.0;

/// Allowed deviation of the RAW median from the expected background level.
/// Star peaks introduce minor shifts.
const BACKGROUND_TOLERANCE: f32 = 5.0;

/// Expected star positions (used to verify star-detection coordinates).
/// Spread across the image to exercise the full field of view.
type StarCoords = [(usize, usize); 25];

const EXPECTED_STARS: StarCoords = [
    // Row 1
    (64, 64), (160, 64), (256, 64), (352, 64), (448, 64),
    // Row 2
    (64, 144), (160, 144), (256, 144), (352, 144), (448, 144),
    // Row 3
    (64, 224), (160, 224), (256, 224), (352, 224), (448, 224),
    // Row 4
    (64, 304), (160, 304), (256, 304), (352, 304), (448, 304),
    // Row 5
    (64, 384), (160, 384), (256, 384), (352, 384), (448, 384),
];

/// Creates a synthetic RAW image with many bright "stars" on a dark background.
fn create_synthetic_raw_image(cfa: CfaType) -> RawImage {
    let mut data = vec![BACKGROUND_LEVEL; IMG_WIDTH * IMG_HEIGHT];

    for (cx, cy) in EXPECTED_STARS {
        let peak = (4500 + (cx.wrapping_mul(cy)) % 2000) as f32;
        // sigma=1.0 keeps stars sharp enough after Bayer demosaic interpolation
        // so they aren't flagged as overexposed by the plateau detector.
        let radius = 3;
        for dy in 0..=radius {
            for dx in 0..=radius {
                let dist_sq = (dx * dx + dy * dy) as f32;
                let sigma = 1.0_f32;
                let value = peak * (-dist_sq / (2.0 * sigma * sigma)).exp();
                let v = value as u16;

                let y = cy - dy;
                if y < IMG_HEIGHT {
                    let base = y * IMG_WIDTH;
                    if cx >= dx && cx - dx < IMG_WIDTH {
                        let idx = base + cx - dx;
                        data[idx] = data[idx].saturating_add(v);
                    }
                    if cx + dx < IMG_WIDTH {
                        let idx = base + cx + dx;
                        data[idx] = data[idx].saturating_add(v);
                    }
                }
                let y2 = cy + dy;
                if y2 < IMG_HEIGHT {
                    let base = y2 * IMG_WIDTH;
                    if cx >= dx && cx - dx < IMG_WIDTH {
                        let idx = base + cx - dx;
                        data[idx] = data[idx].saturating_add(v);
                    }
                    if cx + dx < IMG_WIDTH {
                        let idx = base + cx + dx;
                        data[idx] = data[idx].saturating_add(v);
                    }
                }
            }
        }
    }

    let info = RawImageInfo {
        width: IMG_WIDTH,
        height: IMG_HEIGHT,
        gain: 1,
        offset: 0,
        max_value: 65535,
        cfa,
        bin: 1,
        frame_type: astra_lite::hal::FrameType::Lights,
        exposure: EXPECTED_EXPOSURE,
        camera: "SyntheticCamera".to_string(),
        ccd_temp: Some(-10.0),
        ..Default::default()
    };

    let cfa_arr = info.cfa.get_array();
    RawImage::new(info, data, cfa_arr)
}

/// Maximum pixel distance allowed between an expected and a detected star.
const STAR_TOLERANCE: f64 = 10.0;

/// Tracks the order of events and key data from the frame-processing pipeline.
#[derive(Default, Debug)]
struct State {
    events_received: Vec<String>,
    histogram_raw_count: usize,
    raw_frame_info_count: usize,
    image_count: usize,
    preview_frame_count: usize,
    light_frame_info_count: usize,
    frame_info_count: usize,
    shot_finished: bool,
    frame_is_ok: Option<bool>,
    /// FWHM reported by star analysis (if available).
    fwhm: Option<f32>,
    /// Ovality reported by star analysis (if available).
    ovality: Option<f32>,
    /// Detected star (x, y) coordinates.
    star_coords: Vec<(f64, f64)>,
    /// Raw frame median from RawFrameInfo — used to verify background level.
    raw_median: Option<u16>,
    /// Raw frame mean from RawFrameInfo.
    raw_mean: Option<f32>,
    /// Background level from LightFrameInfo (post-demosaic).
    light_background: Option<i32>,
    /// Background percent from LightFrameInfo.
    light_bg_percent: Option<f32>,
    /// Demosaic'd image dimensions.
    image_width: Option<usize>,
    image_height: Option<usize>,
    is_color: Option<bool>,
    /// Preview RGB data length.
    preview_rgb_len: Option<usize>,
    preview_width: Option<u32>,
    preview_height: Option<u32>,
    /// RAW standard deviation.
    raw_std_dev: Option<f32>,
    /// Exposure from LightFrameInfo.
    exposure: Option<f64>,
    /// Processing time from ShotProcessingFinished.
    processing_time: Option<f64>,
    /// RawImageInfo from ShotProcessingFinished.
    raw_info_width: Option<usize>,
    raw_info_height: Option<usize>,
    raw_info_gain: Option<i32>,
    raw_info_offset: Option<i32>,
    raw_info_max_value: Option<u16>,
    raw_info_cfa: Option<CfaType>,
    raw_info_bin: Option<u8>,
    raw_info_frame_type: Option<FrameType>,
    raw_info_exposure: Option<f64>,
    raw_info_camera: Option<String>,
    raw_info_ccd_temp: Option<f64>,
    idle_seconds: i64,
}

/// Returns a temporary file path for the synthetic FITS.
fn temp_fits_path() -> PathBuf {
    std::env::temp_dir().join("astra_lite_test_synthetic.fits")
}

/// Runs the full frame-processing test with the given CFA type.
fn run_full_frame_processing(cfa: CfaType, expected_is_color: bool) {
    // Create system core (no camera required).
    let core = Core::new();

    // Write synthetic FITS to disk.
    let fits_path = temp_fits_path();
    let raw_image = create_synthetic_raw_image(cfa);
    save_raw_image_to_fits_file(&raw_image, &fits_path)
        .expect("failed to save synthetic FITS file");

    // Shared state for the event handler.
    let shared_state = Arc::new(Mutex::new(State::default()));

    // Subscribe to frame-processing events.
    core.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessResult { data, .. }) = &event {
                let mut state = shared_state.lock().unwrap();
                state.idle_seconds = 0;

                match data {
                    FrameProcessResultData::ShotProcessingStarted => {
                        state.events_received.push("ShotProcessingStarted".to_string());
                        println!("  Event: ShotProcessingStarted");
                    }

                    FrameProcessResultData::HistogramRaw(_) => {
                        state.events_received.push("HistogramRaw".to_string());
                        state.histogram_raw_count += 1;
                        println!("  Event: HistogramRaw");
                    }

                    FrameProcessResultData::RawFrameInfo(info) => {
                        state.events_received.push("RawFrameInfo".to_string());
                        state.raw_frame_info_count += 1;
                        state.raw_median = Some(info.median);
                        state.raw_mean = Some(info.mean);
                        state.raw_std_dev = Some(info.std_dev);
                        println!(
                            "  Event: RawFrameInfo (mean={}, median={}, std_dev={})",
                            info.mean, info.median, info.std_dev
                        );
                    }

                    FrameProcessResultData::Image(image) => {
                        state.events_received.push("Image".to_string());
                        state.image_count += 1;
                        let img = image.read().unwrap();
                        state.image_width = Some(img.width());
                        state.image_height = Some(img.height());
                        state.is_color = Some(img.is_color());
                        drop(img);
                        println!("  Event: Image");
                    }

                    FrameProcessResultData::PreviewFrame(preview) => {
                        state.events_received.push("PreviewFrame".to_string());
                        state.preview_frame_count += 1;
                        state.preview_rgb_len = Some(preview.rgb_data.bytes.as_ref().len());
                        state.preview_width = Some(preview.rgb_data.width as u32);
                        state.preview_height = Some(preview.rgb_data.height as u32);
                        println!("  Event: PreviewFrame");
                    }

                    FrameProcessResultData::LightFrameInfo(info) => {
                        state.events_received.push("LightFrameInfo".to_string());
                        state.light_frame_info_count += 1;
                        state.fwhm = info.stars.info.fwhm;
                        state.ovality = info.stars.info.ovality;
                        state.light_background = Some(info.image.background);
                        state.light_bg_percent = Some(info.image.bg_percent);
                        state.exposure = Some(info.image.exposure);
                        state.star_coords = info
                            .stars
                            .items
                            .iter()
                            .map(|s| (s.x, s.y))
                            .collect();
                        println!(
                            "  Event: LightFrameInfo (stars={}, bg={}, bg_pct={:.1}%, \
                             fwhm={:?}, ovality={:?})",
                            info.stars.items.len(),
                            info.image.background,
                            info.image.bg_percent,
                            info.stars.info.fwhm,
                            info.stars.info.ovality
                        );
                    }

                    FrameProcessResultData::FrameInfo => {
                        state.events_received.push("FrameInfo".to_string());
                        state.frame_info_count += 1;
                        println!("  Event: FrameInfo");
                    }

                    FrameProcessResultData::ShotProcessingFinished {
                        frame_is_ok,
                        processing_time,
                        raw_image_info,
                        ..
                    } => {
                        state.events_received.push("ShotProcessingFinished".to_string());
                        state.shot_finished = true;
                        state.frame_is_ok = Some(*frame_is_ok);
                        state.processing_time = Some(*processing_time);
                        state.raw_info_width = Some(raw_image_info.width);
                        state.raw_info_height = Some(raw_image_info.height);
                        state.raw_info_gain = Some(raw_image_info.gain);
                        state.raw_info_offset = Some(raw_image_info.offset);
                        state.raw_info_max_value = Some(raw_image_info.max_value);
                        state.raw_info_cfa = Some(raw_image_info.cfa);
                        state.raw_info_bin = Some(raw_image_info.bin);
                        state.raw_info_frame_type = Some(raw_image_info.frame_type);
                        state.raw_info_exposure = Some(raw_image_info.exposure);
                        state.raw_info_camera = Some(raw_image_info.camera.clone());
                        state.raw_info_ccd_temp = raw_image_info.ccd_temp;
                        println!(
                            "  Event: ShotProcessingFinished (ok={}, time={:.3}s, \
                             size={}x{}, gain={}, offset={}, max={}, cfa={:?}, bin={}, \
                             frame={}, exposure={}, camera={:?}, ccd_temp={:?})",
                            frame_is_ok, processing_time,
                            raw_image_info.width, raw_image_info.height,
                            raw_image_info.gain, raw_image_info.offset,
                            raw_image_info.max_value, raw_image_info.cfa,
                            raw_image_info.bin, raw_image_info.frame_type.to_str(),
                            raw_image_info.exposure,
                            raw_image_info.camera,
                            raw_image_info.ccd_temp
                        );
                    }

                    other => {
                        let name = match other {
                            FrameProcessResultData::PreviewLiveRes(_) => "PreviewLiveRes",
                            FrameProcessResultData::FrameInfoLiveRes => "FrameInfoLiveRes",
                            FrameProcessResultData::HistogramLiveRes => "HistogramLiveRes",
                            FrameProcessResultData::MasterSaved { .. } => "MasterSaved",
                            _ => "Unknown",
                        };
                        println!("  Event: {}", name);
                    }
                }
            }
        }
    });

    println!("Opening synthetic FITS: {:?}", fits_path);

    // Trigger frame processing.
    core
        .open_image_from_file(&fits_path)
        .expect("failed to open image from file");

    // Wait for processing to complete.
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        if state.shot_finished {
            break;
        }

        state.idle_seconds += 1;
        if state.idle_seconds >= TIMEOUT_SECS {
            panic!(
                "Timeout waiting for ShotProcessingFinished after {} seconds.\n\
                 Events received: {:?}",
                TIMEOUT_SECS, state.events_received
            );
        }
    }

    // Clean up temp file.
    let _ = std::fs::remove_file(&fits_path);

    // Analyze results.
    let state = shared_state.lock().unwrap();

    println!("\n=== Final State ===");
    println!("Events: {:?}", state.events_received);
    println!("frame_is_ok: {:?}", state.frame_is_ok);
    println!("fwhm: {:?}", state.fwhm);
    println!("ovality: {:?}", state.ovality);

    // --- Assertions ---

    // Every expected event must be present at least once.
    assert!(
        state.histogram_raw_count >= 1,
        "expected at least 1 HistogramRaw event, got {}",
        state.histogram_raw_count
    );
    assert_eq!(
        state.raw_frame_info_count, 1,
        "expected exactly 1 RawFrameInfo event, got {}",
        state.raw_frame_info_count
    );
    assert_eq!(
        state.image_count, 1,
        "expected exactly 1 Image event, got {}",
        state.image_count
    );
    assert!(
        state.preview_frame_count >= 1,
        "expected at least 1 PreviewFrame event, got {}",
        state.preview_frame_count
    );
    assert_eq!(
        state.light_frame_info_count, 1,
        "expected exactly 1 LightFrameInfo event, got {}",
        state.light_frame_info_count
    );
    assert_eq!(
        state.frame_info_count, 1,
        "expected exactly 1 FrameInfo event, got {}",
        state.frame_info_count
    );
    assert!(state.shot_finished, "expected ShotProcessingFinished event");
    assert!(
        state.frame_is_ok == Some(true),
        "expected frame_is_ok=true, got {:?}",
        state.frame_is_ok
    );

    // --- Verify background level from histogram & light frame info ---

    let median = state.raw_median.expect("raw_median should be set");
    let mean = state.raw_mean.expect("raw_mean should be set");
    println!(
        "RAW background check: median={}, mean={:.1} (expected ~{})",
        median, mean, BACKGROUND_LEVEL
    );

    assert!(
        (median as f32 - BACKGROUND_LEVEL as f32).abs() <= BACKGROUND_TOLERANCE,
        "raw median {} deviates from background {} by more than {:.0}",
        median,
        BACKGROUND_LEVEL,
        BACKGROUND_TOLERANCE
    );

    // Mean is slightly above background due to star peaks — just verify it's not below it.
    assert!(
        mean >= BACKGROUND_LEVEL as f32,
        "raw mean {:.1} is below background {}",
        mean,
        BACKGROUND_LEVEL
    );

    // LightFrameInfo background (post-demosaic)
    let light_bg = state.light_background.expect("light_background should be set");
    let bg_pct = state.light_bg_percent.expect("bg_percent should be set");
    println!(
        "Light background check: bg={}, bg_pct={:.2}%",
        light_bg, bg_pct
    );

    // Demosaic + star peaks shift the background upward.
    assert!(
        light_bg >= BACKGROUND_LEVEL as i32,
        "light background {} is below raw background {}",
        light_bg,
        BACKGROUND_LEVEL
    );

    assert!(
        bg_pct > 0.0 && bg_pct < 1.0,
        "bg_pct {:.2}% is out of expected range (0..1%% of max_value)",
        bg_pct
    );

    // --- Image dimensions ---

    let img_w = state.image_width.expect("image_width should be set");
    let img_h = state.image_height.expect("image_height should be set");
    println!("Image dimensions: {} x {}", img_w, img_h);
    assert_eq!(img_w, IMG_WIDTH, "image width mismatch");
    assert_eq!(img_h, IMG_HEIGHT, "image height mismatch");

    // Verify color/mono matches the CFA type used.
    assert_eq!(
        state.is_color.expect("is_color should be set"),
        expected_is_color,
        "is_color mismatch: expected {}",
        expected_is_color
    );

    // --- Preview size ---

    let rgb_len = state.preview_rgb_len.expect("preview_rgb_len should be set");
    let pr_w = state.preview_width.expect("preview_width should be set");
    let pr_h = state.preview_height.expect("preview_height should be set");
    let expected_rgb_len = (pr_w * pr_h * 3) as usize;
    println!(
        "Preview: {} x {} (rgb_len={}, expected={})",
        pr_w, pr_h, rgb_len, expected_rgb_len
    );
    assert_eq!(
        rgb_len, expected_rgb_len,
        "preview RGB data length mismatch"
    );

    // --- std_dev RAW ---

    let std_dev = state.raw_std_dev.expect("raw_std_dev should be set");
    println!("RAW std_dev: {:.2}", std_dev);
    assert!(
        std_dev > 0.0,
        "raw std_dev must be positive (image is not uniform)"
    );

    // --- Exposure from LightFrameInfo ---

    let exposure = state.exposure.expect("exposure should be set");
    println!("Exposure from FITS: {:.1}s", exposure);
    assert!(
        (exposure - EXPECTED_EXPOSURE).abs() < 0.01,
        "exposure {:.1} does not match expected {:.1}",
        exposure, EXPECTED_EXPOSURE
    );

    // --- Processing time ---

    let proc_time = state.processing_time.expect("processing_time should be set");
    println!("Processing time: {:.3}s", proc_time);
    assert!(proc_time > 0.0, "processing time must be positive");

    // --- RawImageInfo from ShotProcessingFinished ---

    let ri_width = state.raw_info_width.expect("raw_info_width should be set");
    let ri_height = state.raw_info_height.expect("raw_info_height should be set");
    let ri_gain = state.raw_info_gain.expect("raw_info_gain should be set");
    let ri_offset = state.raw_info_offset.expect("raw_info_offset should be set");
    let ri_max_value = state.raw_info_max_value.expect("raw_info_max_value should be set");
    let ri_cfa = state.raw_info_cfa.expect("raw_info_cfa should be set");
    let ri_bin = state.raw_info_bin.expect("raw_info_bin should be set");
    let ri_frame_type = state.raw_info_frame_type.expect("raw_info_frame_type should be set");
    let ri_exposure = state.raw_info_exposure.expect("raw_info_exposure should be set");
    let ri_camera = state.raw_info_camera.as_ref().expect("raw_info_camera should be set");
    let ri_ccd_temp = state.raw_info_ccd_temp.expect("raw_info_ccd_temp should be set");
    println!(
        "RawImageInfo: {}x{}, gain={}, offset={}, max={}, cfa={:?}, bin={}, \
         frame={}, exposure={}, camera={}, ccd_temp={}",
        ri_width, ri_height, ri_gain, ri_offset, ri_max_value, ri_cfa,
        ri_bin, ri_frame_type.to_str(), ri_exposure, ri_camera, ri_ccd_temp
    );

    assert_eq!(ri_width, IMG_WIDTH, "raw_info width mismatch");
    assert_eq!(ri_height, IMG_HEIGHT, "raw_info height mismatch");
    assert_eq!(ri_gain, 1, "raw_info gain mismatch");
    assert_eq!(ri_offset, 0, "raw_info offset mismatch");
    assert_eq!(ri_max_value, 65535, "raw_info max_value mismatch");
    assert_eq!(ri_cfa, cfa, "raw_info cfa mismatch");
    assert_eq!(ri_bin, 1, "raw_info bin mismatch");
    assert_eq!(ri_frame_type, FrameType::Lights, "raw_info frame_type mismatch");
    assert!(
        (ri_exposure - EXPECTED_EXPOSURE).abs() < 0.01,
        "raw_info exposure {:.1} does not match expected {:.1}",
        ri_exposure, EXPECTED_EXPOSURE
    );
    assert_eq!(ri_camera, "SyntheticCamera", "raw_info camera mismatch");
    assert!(
        (ri_ccd_temp - (-10.0)).abs() < 0.01,
        "raw_info ccd_temp {:.1} does not match expected -10.0",
        ri_ccd_temp
    );

    // FWHM / ovality should be computed for both mono and color.
    let fwhm = state.fwhm.expect("FWHM should be computed");
    let ovality = state.ovality.expect("ovality should be computed");
    println!("Star quality: FWHM={}, Ovality={}", fwhm, ovality);
    assert!(fwhm > 0.0, "FWHM must be positive");

    // --- Verify detected star coordinates ---

    let detected_count = state.star_coords.len();
    println!(
        "Detected {} stars (expected {})",
        detected_count,
        EXPECTED_STARS.len()
    );

    // At least half of the expected star positions must have a detected star within tolerance.
    let mut matched_expected = 0;
    for &(ex, ey) in &EXPECTED_STARS {
        let ex_f = ex as f64;
        let ey_f = ey as f64;
        let has_match = state.star_coords.iter().any(|&(sx, sy)| {
            let dx = sx - ex_f;
            let dy = sy - ey_f;
            (dx * dx + dy * dy).sqrt() <= STAR_TOLERANCE
        });
        if has_match {
            matched_expected += 1;
        } else {
            println!(
                "  WARNING: expected star at ({}, {}) not detected",
                ex, ey
            );
        }
    }

    let match_ratio = matched_expected as f64 / EXPECTED_STARS.len() as f64;
    println!(
        "Star coordinate match: {}/{} ({:.0}%)",
        matched_expected,
        EXPECTED_STARS.len(),
        match_ratio * 100.0
    );

    assert!(
        match_ratio >= 0.5,
        "too few expected stars detected: {}/{} ({:.0}%), expected >= 50%%",
        matched_expected,
        EXPECTED_STARS.len(),
        match_ratio * 100.0
    );

    // Verify the correct event order (first occurrence of each).
    let expected_order: &[&str] = &[
        "ShotProcessingStarted",
        "HistogramRaw",
        "RawFrameInfo",
        "Image",
        "PreviewFrame",
        "LightFrameInfo",
        "FrameInfo",
        "ShotProcessingFinished",
    ];

    let mut last_idx = 0;
    for expected in expected_order.iter() {
        let Some(idx) = state.events_received.iter().position(|e| e == *expected) else {
            panic!("Expected event '{}' not found in sequence", expected);
        };
        assert!(
            idx >= last_idx,
            "Event '{}' appeared out of order (expected after previous events)",
            expected
        );
        last_idx = idx + 1;
    }

    // Verify core.cur_frame.image is not empty.
    assert!(
        !core.cur_frame.image.read().unwrap().is_empty(),
        "core.cur_frame.image must not be empty after processing"
    );

    // Verify core is still in WaitingMode (OpeningImgFile does not switch modes).
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should remain in WaitingMode after opening an image file"
    );

    // Cleanup.
    core.stop();
}

/// Tests the full frame-processing pipeline with a color Bayer (RGGB) RAW image.
/// Verifies that the demosaic produces a color image.
#[test]
#[serial_test::serial]
fn full_frame_processing_color() {
    run_full_frame_processing(CfaType::RGGB, true);
}

/// Tests the full frame-processing pipeline with a monochrome RAW image.
/// Verifies that the output image is monochrome.
#[test]
#[serial_test::serial]
fn full_frame_processing_mono() {
    run_full_frame_processing(CfaType::None, false);
}
