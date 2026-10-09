//! Drives a camera: connect, pick a deliberately **non-square** sub-frame,
//! take a light exposure, verify the frame shape, save FITS.
//!
//! ProgID from `ASCOM_CAMERA_PROG_ID`, default OmniSim.

use ascom::prelude::*;
use std::time::Duration;

/// Current binning along one axis. A driver that does not answer the member keeps
/// the unbinned limit (1); a binding failure propagates instead of guessing, and a
/// nonsense answer is clamped so the division below cannot panic.
fn live_bin(read: Result<i32>) -> Result<i32> {
    match read {
        Ok(value) => Ok(i32::max(value, 1)),
        Err(error) if error.is_unsupported() => Ok(1),
        Err(error) => Err(error),
    }
}

/// Everything done while the camera is connected. Errors are returned rather than
/// propagated with `?` so the caller always gets to run its disconnect cleanup.
fn drive(camera: &Camera) -> Result<()> {
    // Connect first: the OmniSim camera answers `Name` with `NotConnected` while
    // disconnected, which the spec does not allow (docs/KNOWN_DRIVER_QUIRKS.md).
    camera.set_connected(true)?;
    println!("{} / {}", camera.name()?, camera.driver_version()?);
    println!("connected, interface v{}", camera.interface_version()?);
    println!(
        "sensor {}x{} px, MaxBin {}x{}, MaxADU={}, sensor type {:?}",
        camera.camera_x_size()?,
        camera.camera_y_size()?,
        camera.max_bin_x()?,
        camera.max_bin_y()?,
        camera.max_adu()?,
        camera.sensor_type()?,
    );
    println!(
        "exposure range {:?}..{:?} step {:?}",
        camera.exposure_min().ok(),
        camera.exposure_max().ok(),
        camera.exposure_resolution().ok(),
    );
    println!("gains: {:?}", camera.gains().ok());
    println!("offsets: {:?}", camera.offsets().ok());
    println!("readout modes: {:?}", camera.readout_modes().ok());

    // A non-square sub-frame: the classic transposition trap is invisible on a
    // square frame, so we deliberately avoid squares here (playbook section 9).
    // The frame is in binned pixels, so the limit is CameraXSize / BinX — the same
    // one `set_sub_frame` enforces; a driver left at binning > 1 could not produce
    // a frame sized for unbinned pixels.
    let sensor_x = camera.camera_x_size()?;
    let sensor_y = camera.camera_y_size()?;
    let bin_x = live_bin(camera.bin_x())?;
    let bin_y = live_bin(camera.bin_y())?;
    let limit_x = i32::max(sensor_x / bin_x, 1);
    let limit_y = i32::max(sensor_y / bin_y, 1);
    let num_x = limit_x.min(96);
    let num_y = limit_y.min(48);
    let (num_x, num_y) = if num_x == num_y { (num_x, num_y / 2) } else { (num_x, num_y) };
    println!(
        "sub-frame {num_x}x{num_y} at 0,0 (non-square on purpose) \
         within {limit_x}x{limit_y} at binning {bin_x}x{bin_y}"
    );
    camera.set_sub_frame(0, 0, num_x, num_y)?;

    let duration = camera.exposure_min().unwrap_or(0.1).max(0.1);
    println!("light exposure {duration:?} s");
    let image = match camera.expose(duration, true, WaitSpec::new(Duration::from_secs(120))) {
        Ok(image) => image,
        Err(error) => {
            // `StartExposure` already fired and is never retried, so a failed wait
            // leaves the camera mid-exposure; abort so the next client does not
            // inherit a running frame.
            match camera.abort_exposure() {
                Ok(()) => println!("abort_exposure: ok"),
                Err(abort) => println!("abort_exposure: failed: {abort}"),
            }
            return Err(error);
        }
    };

    println!(
        "frame: {}x{} planes={} BITPIX={} wide_elements={}",
        image.width(),
        image.height(),
        image.planes(),
        image.data.bitpix(),
        image.wide_elements(),
    );
    assert_eq!(image.width(), num_x as usize, "NumX must arrive as the width");
    assert_eq!(image.height(), num_y as usize, "NumY must arrive as the height");
    println!(
        "memory order: {} (ambiguous={})",
        image.layout.orientation.as_str(),
        image.layout.ambiguous
    );
    if let Some(stats) = image.stats() {
        println!(
            "stats: count={} min={:.1} max={:.1} mean={:.2}",
            stats.count, stats.min, stats.max, stats.mean
        );
    }

    let path = std::env::temp_dir().join("ascom_camera_example.fits");
    match image.write_fits(&path) {
        Ok(()) => println!("wrote {}", path.display()),
        Err(error) => println!("FITS write failed: {error}"),
    }

    Ok(())
}

fn main() -> Result<()> {
    let prog_id = std::env::var("ASCOM_CAMERA_PROG_ID")
        .unwrap_or_else(|_| "ASCOM.OmniSim.Camera".to_string());
    println!("== camera {prog_id} ==");

    let camera = Camera::open(&DeviceSpec::new(&prog_id))?;
    // From here the camera may hold a connection, and the library never disconnects
    // on drop (src/actor.rs), so no `?` may skip the cleanup below.
    let outcome = drive(&camera);

    // Always attempt the disconnect and report its outcome; a failure is printed
    // here, never propagated past the cleanup it follows.
    match camera.set_connected(false) {
        Ok(()) => println!("disconnected, bye"),
        Err(error) => println!("disconnect failed: {error}"),
    }
    outcome?;
    Ok(())
}
