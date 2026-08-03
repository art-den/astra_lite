use std::{path::Path, sync::{Arc, Mutex}, time::Duration};

use astra_lite::{core::{core::*, events::*, frame_processing::{FrameProcessResult, FrameProcessResultData}}, hal::{DeviceType, FrameType, HalImpl}, image::io::load_raw_image_from_fits_file};

/// Exposure time per frame in seconds.
const EXPOSURE_SECS: f64 = 1.0;

/// Number of frames to capture and verify in the test.
const EXPECTED_FRAME_COUNT: usize = 5;

/// Validates a single FITS frame file by reading it back and checking its contents.
fn validate_fits_frame(
    file_path: &Path,
    expected_frame_type: FrameType,
    expected_exposure: f64,
) {
    let raw = load_raw_image_from_fits_file(file_path)
        .unwrap_or_else(|e| panic!("failed to load FITS file '{}': {}", file_path.display(), e));
    let info = raw.info();

    // Dimensions must be non-zero
    assert!(
        info.width > 0 && info.height > 0,
        "file '{}': image dimensions are zero ({}x{})",
        file_path.display(),
        info.width,
        info.height
    );

    // Exposure must match
    assert!((info.exposure - expected_exposure).abs() < 0.01,
        "file '{}': expected EXPTIME {} but got {}",
        file_path.display(),
        expected_exposure,
        info.exposure
    );

    // Frame type must match
    assert_eq!(
        info.frame_type, expected_frame_type,
        "file '{}': expected FRAME {:?} but got {:?}",
        file_path.display(),
        expected_frame_type,
        info.frame_type
    );

    // Pixel data must not be all zeros
    let data = raw.as_slice();
    assert!(!data.is_empty(), "file '{}': pixel data is empty", file_path.display());
    let mean: f64 = data.iter().map(|&v| v as f64).sum::<f64>() / data.len() as f64;
    assert!(mean > 0.0,
        "file '{}': all pixels are zero (mean = {})",
        file_path.display(),
        mean
    );
}

/// Runs a multi-frame capture in LiveStacking mode (5 frames, 1 s exposure).
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
#[serial_test::serial]
fn live_stacking() {
    // Create system core
    let core = Core::new();
    let mut options = core.options.write().unwrap();

    #[cfg(target_os = "linux")]
    {
        options.indi.address = "localhost".to_string();
        options.indi.remote = true;
        let indi_hal = core.hal.indi_impl();
        indi_hal.connect(
            options.indi.remote,
            &options.indi.address,
            &None, &None, &None, &None, &None, &None, &None, // All None because a remote connection is used.
        ).expect("connecting to INDI");
        drop(options);
        std::thread::sleep(Duration::from_secs(4)); // Waiting at least 4 sec to be sure all devices are initialized
    }

    #[cfg(target_os = "windows")]
    {
        let aa_hal = core.hal.ascom_alpaca_impl();
        aa_hal.connect(&options.ascom_alpaca.address).expect("connecting to ASCOM Alpaca");
        std::thread::sleep(Duration::from_secs(1));
        drop(options);
    }

    // Select the only connected camera and make it active in Core
    #[cfg(target_os = "linux")]
    let hal_impl = core.hal.indi_impl();
    #[cfg(target_os = "windows")]
    let hal_impl = core.hal.ascom_alpaca_impl();

    let all_cameras = hal_impl.devices(DeviceType::CAMERA).expect("requesting camera list");
    assert!(all_cameras.len() > 0, "At least one camera must be connected");
    core.cur_devices.change_camera(&all_cameras[0].id);
    drop(all_cameras);

    // Prepare a unique temporary output directory for original frames
    let random_suffix = format!("{:x}", rand::random::<u32>());
    let out_dir = std::env::temp_dir()
        .join(format!("astra_lite_test_live_stacking_{random_suffix}"));
    std::fs::create_dir_all(&out_dir).expect("creating temp output dir");

    // Configure 5-frame live stacking sequence with original frame saving.
    let mut opts = core.options.write().unwrap();
    opts.cam.frame.set_exposure(EXPOSURE_SECS);
    opts.live.use_cnt = true;
    opts.live.frame_cnt = EXPECTED_FRAME_COUNT;
    opts.live.save_orig = true;
    opts.raw_frames.out_path = out_dir.clone();
    drop(opts);

    core.check_before_saving_raw_or_live_stacking().unwrap();
    core.start_live_stacking().unwrap();

    // Shared state for the event handler
    #[derive(Default)]
    struct State {
        finished_count: usize,      // number of ShotProcessingFinished events received
        idle_seconds: i64,          // silence watchdog timer
        mode_changed: bool,         // ensures the core switched out of LiveStacking
    }

    let shared_state = Arc::new(Mutex::new(State::default()));

    // Subscribe to frame processing events from Core.
    // The pipeline emits: ShotProcessingStarted -> RawFrameInfo -> Image -> PreviewFrame -> ShotProcessingFinished.
    // For LiveStacking this cycle repeats `frame_cnt` times.
    core.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessResult { data, .. }) = &event {
                match data {
                    // Reset watchdog — a frame processing cycle has just started
                    FrameProcessResultData::ShotProcessingStarted => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    // Cycle completed — count successful frames.
                    FrameProcessResultData::ShotProcessingFinished { frame_is_ok, .. } => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        if *frame_is_ok {
                            state.finished_count += 1;
                            println!(
                                "FrameProcessResultData::ShotProcessingFinished (ok, #{}/{})",
                                state.finished_count,
                                EXPECTED_FRAME_COUNT
                            );
                        } else {
                            println!("FrameProcessResultData::ShotProcessingFinished (bad frame)");
                        }
                        assert!(*frame_is_ok, "captured frame quality check failed");
                    }

                    _ => {}
                }
            }

            // Core emits ModeChanged after replacing the active mode (LiveStacking -> WaitingMode).
            if let Event::ModeChanged = &event {
                println!("Event::ModeChanged");
                let mut state = shared_state.lock().unwrap();
                state.idle_seconds = 0;
                state.mode_changed = true;
            }
        }
    });

    // Wait for all frames to be processed and the mode to switch, with a safety watchdog.
    // 5 frames × 1 s exposure + processing overhead ≈ 10–15 s; watchdog triggers at 20 s of silence.
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        // All frames must be processed AND mode must have switched away from LiveStacking.
        if state.finished_count >= EXPECTED_FRAME_COUNT && state.mode_changed {
            break;
        }

        state.idle_seconds += 1;
        if state.idle_seconds >= 20 {
            panic!(
                "No events in the last 20 seconds — camera or server may be unresponsive \
                 (finished {}/{})",
                state.finished_count, EXPECTED_FRAME_COUNT
            );
        }
    }

    // Final assertion: exactly the expected number of frames were captured
    let state = shared_state.lock().unwrap();
    assert_eq!(
        state.finished_count, EXPECTED_FRAME_COUNT,
        "expected {} finished frames, got {}",
        EXPECTED_FRAME_COUNT, state.finished_count
    );

    // Verify all original frame files exist on disk
    let entries: Vec<_> = std::fs::read_dir(&out_dir)
        .expect("reading temp output dir")
        .filter_map(|e| e.ok())
        .flat_map(|e| {
            let path = e.path();
            if path.is_dir() {
                std::fs::read_dir(&path).ok().into_iter().flatten()
                    .filter_map(|f| f.ok().map(|f| f.path()))
                    .collect::<Vec<_>>()
            } else {
                vec![path]
            }
        })
        .filter(|p| p.extension().map_or(false, |ext| ext == "fits"))
        .collect();

    assert_eq!(
        entries.len(), EXPECTED_FRAME_COUNT,
        "expected {} FITS files on disk, got {}",
        EXPECTED_FRAME_COUNT, entries.len()
    );

    for entry in &entries {
        validate_fits_frame(entry, FrameType::Lights, EXPOSURE_SECS);
    }

    // Verify the live stacking result image is not empty
    assert!(
        !core.live_stacking.image.read().unwrap().is_empty(),
        "live stacking result image must not be empty"
    );

    // Verify the current image is not empty
    assert!(
        !core.cur_frame.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );

    // Verify the core has returned to WaitingMode
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after LiveStacking completes"
    );

    // Cleanup: remove temporary output directory
    std::fs::remove_dir_all(&out_dir).expect("removing temp output dir");
}
