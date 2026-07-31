use std::{sync::{Arc, Mutex}, time::Duration};

use astra_lite::{core::{core::*, events::*, frame_processing::{FrameProcessResult, FrameProcessResultData}}, hal::{DeviceType, FrameType, HalImpl}};

/// Exposure time per frame in seconds.
const EXPOSURE_SECS: f64 = 1.0;

/// Number of frames to capture and verify in the test.
const EXPECTED_FRAME_COUNT: usize = 5;

/// Runs a multi-frame capture in SavingRawFrames mode (5 frames × 1 s exposure).
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
#[serial_test::serial]
fn saving_raw_frames() {
    // Create system core and connect to ASCOM Alpaca server
    let core = Core::new();
    let aa_hal = core.hal.ascom_alpaca_impl();
    let options = core.options.read().unwrap();
    aa_hal.connect(&options.ascom_alpaca.address).expect("connecting");
    drop(options);
    std::thread::sleep(Duration::from_secs(1));

    // Select the only connected camera and make it active in Core
    let all_cameras = aa_hal.devices(DeviceType::CAMERA).expect("requesting camera list");
    assert_eq!(all_cameras.len(), 1, "exactly one camera must be connected");
    core.cur_devices.change_camera(&all_cameras[0].id);
    drop(all_cameras);

    // Prepare a unique temporary output directory for raw frames
    let random_suffix = format!("{:x}", rand::random::<u32>());
    let out_dir = std::env::temp_dir()
        .join(format!("astra_lite_test_saving_raw_frames_{random_suffix}"));
    std::fs::create_dir_all(&out_dir).expect("creating temp output dir");

    // Configure 1-second exposure, Lights frame type, and 5-frame sequence
    let mut opts = core.options.write().unwrap();
    opts.cam.frame.set_exposure(EXPOSURE_SECS);
    opts.cam.frame.frame_type = FrameType::Lights;
    opts.raw_frames.use_cnt = true;
    opts.raw_frames.frame_cnt = EXPECTED_FRAME_COUNT;
    opts.raw_frames.out_path = out_dir.clone();
    opts.raw_frames.create_master = false;  // skip master creation for the test
    drop(opts);

    core.check_before_saving_raw_or_live_stacking().unwrap();
    core.start_saving_raw_frames().unwrap();

    // Shared state for the event handler
    #[derive(Default)]
    struct State {
        finished_count: usize,      // number of ShotProcessingFinished events received
        time_since_no_events: i64,  // silence watchdog timer
        mode_changed: bool,         // ensures the core switched out of SavingRawFrames
    }

    let shared_state = Arc::new(Mutex::new(State::default()));

    // Subscribe to frame processing events from Core.
    // The pipeline emits: ShotProcessingStarted -> RawFrameInfo -> Image -> PreviewFrame -> ShotProcessingFinished.
    // For SavingRawFrames this cycle repeats `frame_cnt` times.
    core.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessResult { data, .. }) = &event {
                match data {
                    // Reset watchdog — a frame processing cycle has just started
                    FrameProcessResultData::ShotProcessingStarted => {
                        let mut state = shared_state.lock().unwrap();
                        state.time_since_no_events = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    // Cycle completed — count successful frames.
                    FrameProcessResultData::ShotProcessingFinished { frame_is_ok, .. } => {
                        let mut state = shared_state.lock().unwrap();
                        state.time_since_no_events = 0;
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

            // Core emits ModeChanged after replacing the active mode (SavingRawFrames -> WaitingMode).
            if let Event::ModeChanged = &event {
                println!("Event::ModeChanged");
                let mut state = shared_state.lock().unwrap();
                state.time_since_no_events = 0;
                state.mode_changed = true;
            }
        }
    });

    // Wait for all frames to be processed and the mode to switch, with a safety watchdog.
    // 5 frames × 1 s exposure + processing overhead ≈ 10–15 s; watchdog triggers at 20 s of silence.
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        // All frames must be processed AND mode must have switched away from SavingRawFrames.
        if state.finished_count >= EXPECTED_FRAME_COUNT && state.mode_changed {
            break;
        }

        state.time_since_no_events += 1;
        if state.time_since_no_events >= 20 {
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

    // Verify all raw frame files exist on disk
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

    // Verify the current image is not empty
    assert!(
        !core.cur_frame.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );

    // Verify the core has returned to WaitingMode
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after SavingRawFrames completes"
    );

    // Cleanup: remove temporary output directory
    std::fs::remove_dir_all(&out_dir).expect("removing temp output dir");
}

/// Same as [saving_raw_frames] but aborts mid-capture and resumes.
/// Expects 5 frames: captures ~3, aborts, verifies WaitingMode, resumes, finishes remaining.
#[test]
#[serial_test::serial]
fn saving_raw_frames_with_abort_and_resume() {
    const ABORT_AFTER_FRAMES: usize = 3;

    // Create system core and connect to ASCOM Alpaca server
    let core = Core::new();
    let aa_hal = core.hal.ascom_alpaca_impl();
    let options = core.options.read().unwrap();
    aa_hal.connect(&options.ascom_alpaca.address).expect("connecting");
    drop(options);
    std::thread::sleep(Duration::from_secs(1));

    // Select the only connected camera and make it active in Core
    let all_cameras = aa_hal.devices(DeviceType::CAMERA).expect("requesting camera list");
    assert_eq!(all_cameras.len(), 1, "exactly one camera must be connected");
    core.cur_devices.change_camera(&all_cameras[0].id);
    drop(all_cameras);

    // Prepare a unique temporary output directory for raw frames
    let random_suffix = format!("{:x}", rand::random::<u32>());
    let out_dir = std::env::temp_dir()
        .join(format!("astra_lite_test_abort_resume_{random_suffix}"));
    std::fs::create_dir_all(&out_dir).expect("creating temp output dir");

    // Configure 1-second exposure, Lights frame type, and 5-frame sequence
    let mut opts = core.options.write().unwrap();
    opts.cam.frame.set_exposure(EXPOSURE_SECS);
    opts.cam.frame.frame_type = FrameType::Lights;
    opts.raw_frames.use_cnt = true;
    opts.raw_frames.frame_cnt = EXPECTED_FRAME_COUNT;
    opts.raw_frames.out_path = out_dir.clone();
    opts.raw_frames.create_master = false;
    drop(opts);

    core.check_before_saving_raw_or_live_stacking().unwrap();
    core.start_saving_raw_frames().unwrap();

    #[derive(Default)]
    struct State {
        finished_count: usize,
        time_since_no_events: i64,
        mode_changed: bool,
        mode_continued: bool,
    }

    let shared_state = Arc::new(Mutex::new(State::default()));

    core.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessResult { data, .. }) = &event {
                match data {
                    FrameProcessResultData::ShotProcessingStarted => {
                        let mut state = shared_state.lock().unwrap();
                        state.time_since_no_events = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    FrameProcessResultData::ShotProcessingFinished { frame_is_ok, .. } => {
                        let mut state = shared_state.lock().unwrap();
                        state.time_since_no_events = 0;
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

            if let Event::ModeChanged = &event {
                println!("Event::ModeChanged");
                let mut state = shared_state.lock().unwrap();
                state.time_since_no_events = 0;
                state.mode_changed = true;
            }

            if let Event::ModeContinued = &event {
                println!("Event::ModeContinued");
                let mut state = shared_state.lock().unwrap();
                state.time_since_no_events = 0;
                state.mode_continued = true;
            }
        }
    });

    // --- Phase 1: capture frames up to ABORT_AFTER_FRAMES, then abort ---
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        if state.finished_count >= ABORT_AFTER_FRAMES {
            drop(state);
            println!("Aborting after {} frames…", ABORT_AFTER_FRAMES);
            core.abort_active_mode();
            break;
        }

        state.time_since_no_events += 1;
        if state.time_since_no_events >= 20 {
            panic!(
                "No events in the last 20 seconds — abort phase stalled (finished {}/{})",
                state.finished_count, EXPECTED_FRAME_COUNT
            );
        }
    }

    // Verify core has returned to WaitingMode
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after abort"
    );

    // --- Phase 2: resume and capture remaining frames ---
    {
        let mut state = shared_state.lock().unwrap();
        state.mode_continued = false;
        state.mode_changed = false;
        state.time_since_no_events = 0;
    }

    println!("Resuming capture…");
    core.continue_prev_mode().expect("resuming previous mode");

    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        if state.finished_count >= EXPECTED_FRAME_COUNT && state.mode_changed {
            break;
        }

        state.time_since_no_events += 1;
        if state.time_since_no_events >= 20 {
            panic!(
                "No events in the last 20 seconds — resume phase stalled (finished {}/{})",
                state.finished_count, EXPECTED_FRAME_COUNT
            );
        }
    }

    // Final assertion: all frames were captured
    let state = shared_state.lock().unwrap();
    assert_eq!(
        state.finished_count, EXPECTED_FRAME_COUNT,
        "expected {} finished frames, got {}",
        EXPECTED_FRAME_COUNT, state.finished_count
    );
    assert!(
        state.mode_continued,
        "ModeContinued event was not received after resume"
    );

    // Verify all raw frame files exist on disk
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

    // Verify the current image is not empty
    assert!(
        !core.cur_frame.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );

    // Verify the core has returned to WaitingMode
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after SavingRawFrames completes"
    );

    // Cleanup: remove temporary output directory
    std::fs::remove_dir_all(&out_dir).expect("removing temp output dir");
}
