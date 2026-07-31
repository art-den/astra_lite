use std::{sync::{Arc, Mutex}, time::Duration};

use astra_lite::{core::{core::*, events::*, frame_processing::{FrameProcessResult, FrameProcessResultData}}, hal::{DeviceType, HalImpl}};

/// Exposure time per frame in seconds.
const EXPOSURE_SECS: f64 = 1.0;

/// Duration (in seconds) the LiveView test runs before being stopped.
const LIVE_VIEW_DURATION_SECS: u64 = 5;

/// Runs a continuous capture in LiveView mode, then stops it after a timeout.
/// Verifies that at least one frame was processed during the session.
/// Run with `cargo test -- --nocapture` to see event output.
#[test]
fn live_view() {
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

    // Configure exposure for LiveView and start the mode
    core.options.write().unwrap().cam.frame.set_exposure(EXPOSURE_SECS);
    core.start_live_view().unwrap();

    // Shared state for the event handler
    #[derive(Default)]
    struct State {
        finished_count: usize,    // number of ShotProcessingFinished events received
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
                        println!("FrameProcessResultData::ShotProcessingStarted");
                    }

                    FrameProcessResultData::ShotProcessingFinished { frame_is_ok, .. } => {
                        let mut state = shared_state.lock().unwrap();
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

    // LiveView runs continuously — stop it after a fixed timeout.
    std::thread::sleep(Duration::from_secs(LIVE_VIEW_DURATION_SECS));

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

    // Verify the core has returned to WaitingMode after being stopped.
    assert_eq!(
        core.mode().active.get_type(),
        ModeType::Waiting,
        "core should be in WaitingMode after LiveView is stopped"
    );
}
