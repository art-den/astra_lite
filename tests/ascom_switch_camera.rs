// Everything below drives real ASCOM drivers, so it is compiled on Windows only
#[cfg(windows)]
use astra_lite::{
    core::{engine::*, events::*, frame_processing::{FrameProcessNotification, FrameProcessEvent}},
};

#[cfg(windows)]
mod common;

/// The hub camera and the plain camera it hubs underneath on the dev machine.
#[cfg(windows)]
const PLAIN_CAM: &str = "ASCOM.OmniSim.Camera";
#[cfg(windows)]
const HUB_CAM: &str = "ASCOM.JustAHub64.Camera";

/// Exposure time per frame in seconds.
#[cfg(windows)]
const EXPOSURE_SECS: f64 = 1.0;

/// Regression test: switching plain -> hub -> plain must leave the plain
/// camera usable. A hub driver disconnects its underlying driver when it is
/// deactivated and both share one COM class instance, so the app must
/// deactivate the previous device BEFORE activating the new one
/// (`CurDevices::change_*_impl`); otherwise the hub's disconnect lands after
/// the reactivation and leaves the driver `NotConnected`.
///
/// Skipped when one of the drivers is not registered or the hub cannot
/// connect (its underlying-driver choice is the hub's own external config).
#[test]
#[serial_test::serial]
fn ascom_switch_camera() {
    #[cfg(windows)]
    run_switch_camera();

    #[cfg(not(windows))]
    println!("ASCOM Classic (COM) is Windows only: nothing to check here");
}

/// Driver round-trip through the selected camera. Must NOT be `is_active()`:
/// the bug was exactly a wrapper reporting `Active` over a disconnected driver.
#[cfg(windows)]
fn assert_camera_roundtrip(engine: &Engine, context: &str) {
    let cam = engine.cur_devices.camera_or_err().expect(context);
    cam.exposure_range().expect(context);
    cam.set_binning(1, 1).expect(context); // the exact call that failed for the user
}

#[cfg(windows)]
fn run_switch_camera() {
    let _lock = common::HardwareLock::acquire().expect("acquiring the hardware test lock");

    let engine = Engine::new();
    let _teardown = common::EngineTeardown::new(&engine);

    let plain_cam = std::env::var("ASTRA_ASCOM_TEST_PLAIN_CAMERA")
        .unwrap_or_else(|_| PLAIN_CAM.to_string());
    let hub_cam = std::env::var("ASTRA_ASCOM_TEST_HUB_CAMERA")
        .unwrap_or_else(|_| HUB_CAM.to_string());

    let ascom = engine.hal.ascom_impl();
    if ascom.find_camera(&plain_cam).is_none() || ascom.find_camera(&hub_cam).is_none() {
        println!("skipping: both {plain_cam} and {hub_cam} must be registered");
        return;
    }

    // Activate the hub as a probe: when its own setup cannot connect, stop here
    engine.cur_devices.apply_camera(&hub_cam);
    if engine.cur_devices.camera().is_none_or(|c| !c.is_active().unwrap_or(false)) {
        println!("skipping: {hub_cam} cannot be activated on this machine");
        return;
    }

    // Hub -> plain: the minimal sequence that regressed (the hub's disconnect
    // must not outlive the plain camera's activation)
    engine.cur_devices.change_camera(&plain_cam);
    assert_camera_roundtrip(&engine, "plain camera must work after switching off the hub");

    // The user's full sequence: plain -> hub -> plain
    engine.cur_devices.change_camera(&hub_cam);
    engine.cur_devices.change_camera(&plain_cam);
    assert_camera_roundtrip(&engine, "plain camera must survive the hub deactivation");

    // Full shot like the user's Take Shot click
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    engine.events.connect({
        let finished = std::sync::Arc::clone(&finished);
        move |event| {
            if let Event::FrameProcessing(FrameProcessNotification {
                event: FrameProcessEvent::ShotProcessingFinished { .. }, ..
            }) = &event {
                finished.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });

    engine.options.write().unwrap().cam.frame.set_exposure(EXPOSURE_SECS);
    engine.start_single_shot().expect("single shot must start on the reactivated camera");

    for _ in 0..15 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if finished.load(std::sync::atomic::Ordering::Relaxed)
            && engine.modes().active.kind() == ModeKind::Waiting
        {
            assert!(
                !engine.preview.image.read().unwrap().is_empty(),
                "current frame image must not be empty"
            );
            return;
        }
    }
    panic!("the shot did not finish within 15 seconds");
}
