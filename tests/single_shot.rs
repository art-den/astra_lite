use std::{sync::{Arc, Mutex}, time::Duration};

use astra_lite::{core::{core::*, events::*, frame_processing::{FrameProcessResult, FrameProcessResultData}}, hal::{DeviceType, HalImpl}};

/// Exposure time per frame in seconds.
const EXPOSURE_SECS: f64 = 1.0;

/// Runs a single-shot capture
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
fn single_shot() {
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

    // Configure exposure and start single-shot mode
    core.options.write().unwrap().cam.frame.set_exposure(EXPOSURE_SECS);
    core.start_single_shot().unwrap();

    // Shared state for the event handler
    #[derive(Default)]
    struct State {
        finished_flag: bool, // completion flag
        time_since_no_events: i64, // silence watchdog timer
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
                        state.time_since_no_events = 0;
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    // Cycle completed — frame quality must be acceptable.
                    // `frame_is_ok` is true when CCD temp, FWHM, ovality and offset are within configured limits.
                    FrameProcessResultData::ShotProcessingFinished {frame_is_ok, ..} => {
                        let mut state = shared_state.lock().unwrap();
                        state.time_since_no_events = 0;
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
                state.time_since_no_events = 0;
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

        state.time_since_no_events += 1;
        if state.time_since_no_events >= 5 {
            panic!("No events in the last 5 seconds — camera or server may be unresponsive");
        }
    }

    // Verify the core has returned to WaitingMode
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after SingleShot completes"
    );
}
