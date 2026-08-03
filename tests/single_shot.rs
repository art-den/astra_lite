use std::{sync::{Arc, Mutex}, time::Duration};

use astra_lite::{core::{core::*, events::*, frame_processing::{FrameProcessResult, FrameProcessResultData}}, hal::{DeviceType, HalImpl}};

/// Exposure time per frame in seconds.
const EXPOSURE_SECS: f64 = 1.0;

/// Runs a single-shot capture
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
#[serial_test::serial]
fn single_shot() {
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

    // Configure exposure and start single-shot mode
    core.options.write().unwrap().cam.frame.set_exposure(EXPOSURE_SECS);
    core.start_single_shot().unwrap();

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
    core.events.connect({
        let shared_state = Arc::clone(&shared_state);
        move |event| {
            if let Event::FrameProcessing(FrameProcessResult {data, ..}) = &event {
                match data {
                    // Reset watchdog — a frame processing cycle has just started
                    FrameProcessResultData::ShotProcessingStarted => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    // Cycle completed — frame quality must be acceptable.
                    // `frame_is_ok` is true when CCD temp, FWHM, ovality and offset are within configured limits.
                    FrameProcessResultData::ShotProcessingFinished {frame_is_ok, ..} => {
                        let mut state = shared_state.lock().unwrap();
                        state.idle_seconds = 0;
                        state.finished_flag = true;
                        println!("FrameProcessResultData::ShotProcessingFinished");
                        assert!(frame_is_ok, "captured frame quality check failed");
                    }
                    _ => {},
                }
            }
            // Core emits ModeChanged after replacing the active mode (SingleShot -> WaitingMode).
            // Without waiting for this event we would not know if the regime was properly torn down.
            if let Event::ModeChanged = &event {
                println!("Event::ModeChanged");
                let mut state = shared_state.lock().unwrap();
                state.idle_seconds = 0;
                state.mode_changed = true;
            }
        }
    });

    // Wait for the processing pipeline to finish, with a safety watchdog.
    // The watchdog panics if no FrameProcessing event arrives for 5+ seconds,
    // protecting the test from hanging on a stuck camera or server.
    loop {
        std::thread::sleep(Duration::from_secs(1));

        let mut state = shared_state.lock().unwrap();
        // Both events must arrive: frame processed AND mode switched away from SingleShot.
        if state.finished_flag && state.mode_changed {
            break;
        }

        state.idle_seconds += 1;
        if state.idle_seconds >= 5 {
            panic!("No events in the last 5 seconds — camera or server may be unresponsive");
        }
    }

    // Verify the current image is not empty
    assert!(
        !core.cur_frame.image.read().unwrap().is_empty(),
        "current frame image must not be empty"
    );

    // Verify the core has returned to WaitingMode
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after SingleShot completes"
    );
}
