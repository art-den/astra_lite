use std::{sync::{Arc, Mutex}, time::Duration};

use astra_lite::{core::{core::*, events::*, frame_processing::{FrameProcessResult, FrameProcessResultData}}, hal::{DeviceType, HalImpl}};

/// Exposure time per frame in seconds.
const EXPOSURE_SECS: f64 = 1.0;

/// Duration (in seconds) the LiveView test runs before being stopped.
const LIVE_VIEW_DURATION_SECS: u64 = 5;

/// Max seconds of silence before the watchdog panics.
const WATCHDOG_TIMEOUT_SECS: i64 = 5;

/// Runs a continuous capture in LiveView mode, then stops it after a timeout.
/// Verifies that at least one frame was processed during the session.
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
#[serial_test::serial]
fn live_view() {
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
        drop(options);
        std::thread::sleep(Duration::from_secs(1));
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

    // Configure exposure for LiveView and start the mode
    core.options.write().unwrap().cam.frame.set_exposure(EXPOSURE_SECS);
    core.start_live_view().unwrap();

    // Shared state for the event handler
    #[derive(Default)]
    struct State {
        finished_count: usize,    // number of ShotProcessingFinished events received
        idle_seconds: i64,         // silence watchdog timer
    }

    let shared_state = Arc::new(Mutex::new(State::default()));

    // Subscribe to frame processing events from Core.
    // LiveView continuously emits: ShotProcessingStarted -> ... -> ShotProcessingFinished.
    core.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessResult { data, .. }) = &event {
                match data {
                    FrameProcessResultData::ShotProcessingStarted => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    FrameProcessResultData::ShotProcessingFinished { frame_is_ok, .. } => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        if *frame_is_ok {
                            state.finished_count += 1;
                            println!(
                                "FrameProcessResultData::ShotProcessingFinished (ok, #{})",
                                state.finished_count
                            );
                        } else {
                            println!("FrameProcessResultData::ShotProcessingFinished (bad frame)");
                        }
                        assert!(*frame_is_ok, "captured frame quality check failed");
                    }

                    _ => {}
                }
            }
        }
    });

    let mut time_passed = 0_i64; // in seconds

    // LiveView runs continuously — stop it after collecting enough frames or a timeout.
    // The watchdog panics if no FrameProcessing event arrives for 5+ seconds,
    // protecting the test from hanging on a stuck camera or server.
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();

        // Stop if we have enough frames or the time limit has been reached.
        time_passed += 1;
        if state.finished_count >= 2 && time_passed >= WATCHDOG_TIMEOUT_SECS {
            break;
        }

        state.idle_seconds += 1;
        if state.idle_seconds >= WATCHDOG_TIMEOUT_SECS {
            panic!("No events in the last 5 seconds — camera or server may be unresponsive");
        }
    }

    // Stop the core to terminate the LiveView mode.
    core.stop();

    // Verify that at least one frame was processed during the session.
    let state = shared_state.lock().unwrap();
    assert!(
        state.finished_count >= 1,
        "expected at least 1 finished frame in {} seconds, got {}",
        LIVE_VIEW_DURATION_SECS,
        state.finished_count
    );

    // Verify the current image is not empty.
    assert!(
        !core.cur_frame.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );

    // Verify the core has returned to WaitingMode after being stopped.
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after LiveView is stopped"
    );
}
