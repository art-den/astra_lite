use std::{sync::{Arc, Mutex}, time::{Duration, Instant}};

use astra_lite::{
    core::{
        engine::*,
        events::*,
        frame_processing::{FrameProcessEvent, LightFrameResult},
        mode_goto::GotoConfig,
        preview::ResultImageInfo,
    },
    hal::{DeviceType, HalImpl},
    sky_math::math::{degree_to_radian, hour_to_radian, radian_to_degree, EqCoord},
};

/// Target sky coordinates. The telescope emulator accepts any valid coordinates,
/// so they can be hardcoded. RA is in hours, Dec is in degrees.
///
/// NOTE: originally specified as RA = 28:13:33, which is out of the valid
/// 00..24h range; 08:13:33 is used instead.
const TARGET_RA:  f64 = 8.0 + 13.0 / 60.0 + 33.0 / 3600.0;  // hours
const TARGET_DEC: f64 = 28.0 + 13.0 / 60.0 + 33.0 / 3600.0; // degrees

/// Step 3: RA shift for the "slightly offset" goto, in arcmin.
/// The star fields of step 2 and step 4 shots must still overlap enough
/// (>= 10 common star triangles) for the Checking comparison to work.
const SHIFT_RA_ARCMIN: f64 = 15.0;

/// Exposure in seconds for the reference sky shot (step 2), at max camera gain.
const EXPOSURE_SECS: f64 = 2.0;

/// No events for this long => something (telescope/camera/plate solver) is stuck.
/// Must be larger than MAX_GOTO_TIME (180 s, src/core/consts.rs): during a
/// long but legitimate slew the mode emits no events at all.
const NO_EVENT_TIMEOUT: Duration = Duration::from_secs(240);

/// Total budget for the step 4 flow: 3 plate solves + 3 gotos + 3 shots.
const PLATESOLVE_FLOW_TIMEOUT: Duration = Duration::from_secs(600);

/// Waits until the core returns to WaitingMode (the previous mode has finished).
fn wait_for_waiting_mode(engine: &Engine, timeout: Duration) {
    let start = Instant::now();
    while engine.modes().active.kind() != ModeKind::Waiting {
        assert!(
            start.elapsed() < timeout,
            "Timed out ({} s) waiting for WaitingMode",
            timeout.as_secs()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Creates the engine, connects to the hardware HAL and selects the first
/// connected camera and telescope.
fn setup_engine() -> Arc<Engine> {
    let engine = Engine::new();

    #[cfg(target_os = "linux")]
    {
        let mut options = engine.options.write().unwrap();
        options.indi.address = "localhost".to_string();
        options.indi.remote = true;
        let indi_hal = engine.hal.indi_impl();
        indi_hal.connect(
            options.indi.remote,
            &options.indi.address,
            &None, &None, &None, &None, &None, &None, &None, // All None because a remote connection is used.
        ).expect("connecting to INDI");
        drop(options);
        std::thread::sleep(Duration::from_secs(5)); // Waiting at least 5 sec to be sure all devices are initialized
    }

    #[cfg(target_os = "windows")]
    {
        let options = engine.options.write().unwrap();
        let aa_hal = engine.hal.ascom_alpaca_impl();
        aa_hal.connect(&options.ascom_alpaca.address).expect("connecting to ASCOM Alpaca");
        drop(options);
        std::thread::sleep(Duration::from_secs(1));
    }

    // Select the first connected camera and telescope
    #[cfg(target_os = "linux")]
    let hal_impl = engine.hal.indi_impl();
    #[cfg(target_os = "windows")]
    let hal_impl = engine.hal.ascom_alpaca_impl();

    let all_cameras = hal_impl.devices(DeviceType::CAMERA).expect("requesting camera list");
    assert!(!all_cameras.is_empty(), "At least one camera must be connected");
    engine.cur_devices.change_camera(&all_cameras[0].id);
    drop(all_cameras);

    let all_telescopes = hal_impl.devices(DeviceType::TELESCOPE).expect("requesting telescope list");
    assert!(!all_telescopes.is_empty(), "At least one telescope must be connected");
    engine.cur_devices.change_telescope(&all_telescopes[0].id);
    drop(all_telescopes);

    // Reference shot parameters: 2 s exposure at max camera gain.
    // Note: these apply to the step 2 shot (SingleShot uses cam.frame);
    // the GotoMode shots use the plate_solver options instead.
    let camera = engine.cur_devices.camera_or_err().expect("camera is not available");
    let max_gain = if camera.is_gain_supported().expect("is_gain_supported") {
        *camera.gain_range().expect("gain_range").end()
    } else {
        f64::NAN
    };
    let mut options = engine.options.write().unwrap();
    options.cam.frame.set_exposure(EXPOSURE_SECS);
    if !max_gain.is_nan() {
        options.cam.frame.gain = max_gain;
    }
    println!(
        "Camera binning = {:?}, plate solver binning = {:?} \
         (they must match for the offset calculation to succeed)",
        options.cam.frame.binning, options.plate_solver.bin
    );
    println!("Reference shot: exposure = {} s, gain = {} (max)", EXPOSURE_SECS, max_gain);
    drop(options);

    engine
}

/// Runs the user workflow that leads to the "???" offset overlay:
///
/// 1. Goto to a fixed coordinate (OnlyGoto, no image correction).
/// 2. Take a sky shot — the image with stars becomes the current (reference) one.
/// 3. Goto to a slightly offset coordinate (OnlyGoto).
/// 4. "Platesolve current image and goto": plate solves the reference image,
///    goes back to its position with two correction passes, then in the
///    Checking state compares the new shot's stars with the reference ones.
///
/// `match_plate_solver_binning` forces the plate solver binning to the camera
/// binning (diagnostic variant: the pixel scales of both images become equal).
///
/// The function asserts that the Checking shot's `offset` is Some — i.e. the
/// overlay would show a real offset instead of "???".
///
/// Requirements: INDI server on localhost with a telescope emulator and a
/// camera connected, and the `solve-field` executable available.
/// Run with `cargo test --test goto_image_offset -- --nocapture`.
fn run_goto_image_flow(engine: &Arc<Engine>, match_plate_solver_binning: bool) {
    if match_plate_solver_binning {
        let mut options = engine.options.write().unwrap();
        options.plate_solver.bin = options.cam.frame.binning;
        println!(
            "Plate solver binning forced to the camera binning: {:?}",
            options.plate_solver.bin
        );
    }

    // Track plate solve results, the Checking frame and any core error across
    // the whole flow. A mode error aborts the mode (back to WaitingMode) and
    // sends Event::Error, so the error flag must be checked after every step.
    struct State {
        plate_solves:   usize,
        checking_frame: Option<Arc<LightFrameResult>>,
        overlay_text:   Option<Arc<String>>,
        last_event:     Instant,
        error:          Option<String>,
    }

    let shared = Arc::new(Mutex::new(State {
        plate_solves:   0,
        checking_frame: None,
        overlay_text:   None,
        last_event:     Instant::now(),
        error:          None,
    }));

    // The Checking shot is the first light frame processed after the third
    // plate solve result (reference image solve + first shot + final shot).
    engine.events.connect({
        let shared = Arc::clone(&shared);
        move |event| {
            let mut state = shared.lock().unwrap();
            state.last_event = Instant::now();

            if let Event::PlateSolve(_) = &event {
                state.plate_solves += 1;
                println!("  plate solve result #{}", state.plate_solves);
            }

            if let Event::FrameProcessing(fp) = &event
            && let FrameProcessEvent::LightFrameReady(info) = &fp.event
            && state.plate_solves >= 3
            && state.checking_frame.is_none() {
                println!("  Checking frame received");
                state.checking_frame = Some(Arc::clone(info));
            }

            if let Event::OverlayMessage { text, .. } = &event {
                state.overlay_text = Some(Arc::clone(text));
            }

            if let Event::Error(msg) = &event {
                state.error = Some(msg.clone());
            }
        }
    });

    let assert_no_error = |shared: &Arc<Mutex<State>>, step: &str| {
        let err = shared.lock().unwrap().error.clone();
        assert!(err.is_none(), "{} failed: {}", step, err.unwrap());
    };

    // === Step 1: goto to the target coordinate (no image correction) ===

    let coord1 = EqCoord {
        ra:  hour_to_radian(TARGET_RA),
        dec: degree_to_radian(TARGET_DEC),
    };
    println!("Step 1: goto RA = {} h, Dec = {} deg", TARGET_RA, TARGET_DEC);
    engine.start_goto_coord(&coord1, GotoConfig::OnlyGoto).expect("start_goto_coord");
    wait_for_waiting_mode(engine, Duration::from_secs(120));
    assert_no_error(&shared, "step 1 (goto)");
    println!("Step 1: done");

    // === Step 2: sky shot; the image with stars becomes the reference one ===

    println!("Step 2: single shot, {} s", EXPOSURE_SECS);
    engine.start_single_shot().expect("start_single_shot");
    wait_for_waiting_mode(engine, Duration::from_secs(60));
    assert_no_error(&shared, "step 2 (single shot)");

    assert!(
        !engine.preview.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );
    let ref_result: Arc<LightFrameResult> = {
        let info = engine.preview.info.read().unwrap();
        let ResultImageInfo::LightInfo(light_info) = &*info else {
            panic!("current image is not a light frame");
        };
        Arc::clone(light_info)
    };
    assert!(
        !ref_result.stars.items.is_empty(),
        "no stars recognized in the reference shot — increase exposure or gain"
    );
    println!(
        "Reference image: {}x{} px, stars = {}",
        ref_result.image.width, ref_result.image.height, ref_result.stars.items.len()
    );

    // === Step 3: goto to a slightly offset coordinate (no image correction) ===

    let coord2 = EqCoord {
        ra:  coord1.ra + hour_to_radian(SHIFT_RA_ARCMIN / 900.0), // 1 h RA = 900 arcmin
        dec: coord1.dec,
    };
    println!("Step 3: goto with RA shifted by {} arcmin", SHIFT_RA_ARCMIN);
    engine.start_goto_coord(&coord2, GotoConfig::OnlyGoto).expect("start_goto_coord");
    wait_for_waiting_mode(engine, Duration::from_secs(120));
    assert_no_error(&shared, "step 3 (goto with shift)");
    println!("Step 3: done");

    // === Step 4: "Platesolve current image and goto" ===

    println!("Step 4: platesolve current image and goto");
    engine.start_goto_image().expect("start_goto_image");

    let start = Instant::now();
    let checking = loop {
        let state = shared.lock().unwrap();
        if let Some(frame) = state.checking_frame.clone() {
            break Some(frame);
        }
        if state.error.is_some() {
            break None;
        }
        let idle = state.last_event.elapsed();
        drop(state);

        assert!(
            start.elapsed() < PLATESOLVE_FLOW_TIMEOUT,
            "Timed out ({} s) waiting for the Checking frame",
            PLATESOLVE_FLOW_TIMEOUT.as_secs()
        );
        assert!(
            idle < NO_EVENT_TIMEOUT,
            "No events for {} s — telescope, camera or plate solver may be stuck",
            idle.as_secs()
        );
        std::thread::sleep(Duration::from_millis(500));
    };

    let checking = checking.expect(
        "mode errored out before the Checking shot (see Event::Error above)"
    );

    // === Verification ===

    println!(
        "Checking  image: {}x{} px, stars = {}",
        checking.image.width, checking.image.height, checking.stars.items.len()
    );

    // The Checking overlay shows the offset in the pixels of the original
    // image (the event may arrive slightly after the frame result).
    let start = Instant::now();
    let overlay = loop {
        let overlay = shared.lock().unwrap().overlay_text.clone();
        if let Some(overlay) = overlay {
            break overlay;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no overlay message received from the Checking state"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    println!("Overlay message: {}", overlay.replace('\n', " | "));
    assert!(
        overlay.starts_with("Offset x="),
        "overlay shows '{overlay}' instead of the offset"
    );

    // The Checking state loops forever; abort the mode before asserting, so
    // the camera is stopped even if the assertion below panics.
    engine.abort_active_mode();
    wait_for_waiting_mode(engine, Duration::from_secs(30));

    let offset = checking.offset.clone();
    assert!(
        offset.is_some(),
        "offset is None — the Checking overlay would show \"???\". \
         Check the printed image sizes: if they differ (e.g. different binning), \
         Offset::calculate cannot match the star patterns."
    );
    let offset = offset.unwrap();
    println!(
        "Offset: x = {:.2} px, y = {:.2} px, rotation = {:.4} deg",
        offset.x, offset.y, radian_to_degree(offset.angle)
    );
}

/// The user's scenario: the reference shot is taken at the camera binning
/// (Orig, bin 1) while the GotoMode plate solve photos use the plate solver
/// binning (Bin2 by default). The Checking comparison must still compute the
/// offset, accounting for the different pixel scales.
///
/// Fails while the offset calculation ignores the image scale (the overlay
/// shows "???").
#[test]
#[serial_test::serial]
fn goto_image_offset() {
    let engine = setup_engine();
    run_goto_image_flow(&engine, false);
}

/// Diagnostic variant: the plate solver binning is forced to the camera
/// binning, so both images have the same pixel scale.
///
/// Expected to PASS in any case (regression check for the equal-scale path).
#[test]
#[serial_test::serial]
fn goto_image_offset_matched_binning() {
    let engine = setup_engine();
    run_goto_image_flow(&engine, true);
}
