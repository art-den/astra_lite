// Everything below drives real ASCOM drivers, so it is compiled on Windows only
#[cfg(windows)]
use std::{sync::{Arc, Mutex}, time::Duration};

#[cfg(windows)]
use astra_lite::core::{engine::*, events::*, frame_processing::{FrameProcessNotification, FrameProcessEvent}};

#[cfg(windows)]
mod common;

/// Exposure time per frame in seconds.
#[cfg(windows)]
const EXPOSURE_SECS: f64 = 1.0;

/// Runs a single-shot capture through an ASCOM Classic (COM) driver.
///
/// Needs the ASCOM Platform installed with a camera driver registered. The
/// ProgID is taken from `ASTRA_ASCOM_TEST_CAMERA` (ASCOM OmniSim by default).
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
#[serial_test::serial]
fn ascom_single_shot() {
    #[cfg(windows)]
    run_single_shot();

    #[cfg(not(windows))]
    println!("ASCOM Classic (COM) is Windows only: nothing to check here");
}

#[cfg(windows)]
fn run_single_shot() {
    let _lock = common::HardwareLock::acquire().expect("acquiring the hardware test lock");

    // Create system engine
    let engine = Engine::new();
    let _teardown = common::EngineTeardown::new(&engine);

    let prog_id = std::env::var("ASTRA_ASCOM_TEST_CAMERA")
        .unwrap_or_else(|_| "ASCOM.OmniSim.Camera".to_string());

    assert!(
        engine.hal.ascom_impl().find_camera(&prog_id).is_some(),
        "ASCOM driver {prog_id} is not registered. Is the ASCOM Platform installed?"
    );

    // For ASCOM Classic selecting a driver is the connection
    engine.cur_devices.change_camera(&prog_id);

    let camera = engine.cur_devices.camera_or_err()
        .expect("selecting the ASCOM camera must make it available in Core");
    assert!(
        camera.is_active().unwrap_or(false),
        "camera {prog_id} must be active right after selection"
    );

    // Configure exposure and start single-shot mode
    engine.options.write().unwrap().cam.frame.set_exposure(EXPOSURE_SECS);
    engine.start_single_shot().unwrap();

    // Shared state for the event handler
    #[derive(Default)]
    struct State {
        finished_flag: bool, // completion flag
        idle_seconds: i64,         // silence watchdog timer
        mode_changed: bool, // ensures the core actually switched out of SingleShot into WaitingMode after the shot.
    }

    let shared_state = Arc::new(Mutex::new(State::default()));

    // Subscribe to frame processing events from Core.
    // The pipeline emits: ShotProcessingStarted -> RawFrameInfo -> Image -> PreviewFrame -> ShotProcessingFinished.
    engine.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessNotification {event, ..}) = &event {
                match event {
                    // Reset watchdog — a frame processing cycle has just started
                    FrameProcessEvent::ShotProcessingStarted => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    // Cycle completed — frame quality must be acceptable.
                    FrameProcessEvent::ShotProcessingFinished {frame_is_ok, ..} => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        state.finished_flag = true;
                        println!("FrameProcessResultData::ShotProcessingFinished");
                        assert!(*frame_is_ok, "captured frame quality check failed");
                    }
                    _ => {},
                }
            }
            // Core emits ModeChanged after replacing the active mode (SingleShot -> WaitingMode).
            if let Event::ModeChanged = &event {
                println!("Event::ModeChanged");
                let mut state = shared_state.lock().unwrap();
                state.idle_seconds = 0;
                state.mode_changed = true;
            }
        }
    });

    // Wait for the processing pipeline to finish, with a safety watchdog.
    // The watchdog fails if no FrameProcessing event arrives for 5+ seconds,
    // protecting the test from hanging on a stuck driver.
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        // Both events must arrive: frame processed AND mode switched away from SingleShot.
        if state.finished_flag && state.mode_changed {
            break;
        }

        state.idle_seconds += 1;
        if state.idle_seconds >= 5 {
            panic!("No events in the last 5 seconds — the camera driver may be unresponsive");
        }
    }

    // Verify the current image is not empty
    assert!(
        !engine.preview.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );

    // Verify the core has returned to WaitingMode
    assert_eq!(
        engine.modes().active.kind(),
        ModeKind::Waiting,
        "core should be in WaitingMode after SingleShot completes"
    );
}
