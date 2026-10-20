use std::sync::Arc;

use crate::{core::engine::EngineModes, hal::{Camera, CameraInfo, CcdPurpose, Hal}, options::{CamCtrlOptions, FrameOptions, Options}, utils::log_utils::log_if_error};

pub fn take_shot(
    camera:    &Arc<dyn Camera + Send + Sync>,
    frame:     &FrameOptions,
    cam_ctrl:  &CamCtrlOptions,
) -> eyre::Result<()> {
    // Initialization before start

    camera.init_before_shot()?;

    // Conversion gain

    if let Some(conv_gain_str) = &cam_ctrl.conv_gain_str
    && camera.is_conversion_gain_supported()? {
        camera.set_conversion_gain(conv_gain_str)?;
    }

    // Low noise mode

    if camera.is_low_noise_supported()? {
        camera.enable_low_noise_mode(cam_ctrl.low_noise)?;
    }

    // High fullwell mode

    if camera.is_high_fullwell_supported()? {
        camera.enable_high_fullwell_mode(cam_ctrl.high_fullwell)?;
    }

    // Binning

    let mut binning = 1;
    if camera.is_binning_supported()? {
        binning = frame.binning.get_ratio();
        camera.set_binning(binning, binning)?;
    }

    // Frame type

    camera.set_frame_type(frame.frame_type)?;

    // Frame

    if camera.is_frame_supported()? {
        let (width, height) = camera.ccd_size()?;
        // Floor both the size and the origin to the bin grid: the ROI stays inside the sensor
        // and the delivered frame keeps matching `FrameOptions::active_sensor_size()`
        let align_to_binning = |size| binning * (size / binning);
        let crop_width = align_to_binning(frame.crop.translate(width));
        let crop_height = align_to_binning(frame.crop.translate(height));
        let crop_left = align_to_binning((width - crop_width) / 2);
        let crop_top = align_to_binning((height - crop_height) / 2);
        camera.set_frame(crop_left, crop_top, crop_width, crop_height)?;
    }

    // Gain

    if camera.is_gain_supported()? {
        camera.set_gain(frame.gain)?;
    }

    // Offset

    if camera.is_offset_supported()? {
        camera.set_offset(frame.offset as _)?;
    }

    // Start exposure

    camera.start_exposure(frame.exposure())?;

    Ok(())
}

pub fn control_camera_cooling(
    camera:  &Arc<dyn Camera + Send + Sync>,
    options: &CamCtrlOptions
) -> eyre::Result<()> {
    if camera.is_cooler_supported()? {
        if options.enable_cooler {
            log::info!("Setting camera temperature = {}", options.temperature);
            camera.set_temperature(Some(options.temperature))?;
        } else {
            camera.set_temperature(None)?;
        }
    }
    Ok(())
}

pub fn control_camera_fan(
    camera:  &Arc<dyn Camera + Send + Sync>,
    options: &CamCtrlOptions,
) -> eyre::Result<()> {
    if camera.is_fan_ctrl_supported()? {
        let fan_enabled = options.enable_fan || options.enable_cooler;
        log::info!("Setting camera fan = {}", fan_enabled);
        camera.enable_fan(fan_enabled)?;
    }
    Ok(())
}

pub fn control_camera_heater(
    camera:  &Arc<dyn Camera + Send + Sync>,
    options: &CamCtrlOptions
) -> eyre::Result<()> {
    if camera.is_heater_supported()?
    && let Some(heater_str) = &options.heater_str {
        log::info!("Setting camera heater = {}", heater_str);
        camera.control_heater(heater_str)?;
    }
    Ok(())
}

pub fn restart_camera_exposure(
    camera:     &Arc<dyn Camera + Send + Sync>,
    mode:       &mut EngineModes,
    frame_opts: &FrameOptions,
    ctrl_opts:  &CamCtrlOptions,
) -> eyre::Result<()> {
    log::info!("Restarting camera exposure {}...", camera.id());

    // Try to restart exposure by current mode
    let restarted_by_mode = mode.active.restart_cam_exposure()?;
    if restarted_by_mode {
        log::info!("Exposure of camera {} restarted by mode!", camera.id());
        return Ok(());
    }

    // Mode did not restart the camera exposure. Do it manually

    let abort_res = camera.abort_exposure();
    log_if_error(&abort_res, "Abort camera exposure before manual restart");

    let mode_cam_opts = mode.active
        .frame_options_to_restart_exposure()
        .unwrap_or(frame_opts);

    take_shot(camera, mode_cam_opts, ctrl_opts)?;

    log::info!("Exposure of camera {} restarted!", camera.id());
    Ok(())
}

pub fn set_focal_len_for_cameras(hal: &Hal, options: &Options) -> eyre::Result<()> {
    let cameras = hal.cameras()?;
    for cam_info in cameras {
        let Some(focal_len) = focal_len_for_cam(&cam_info, options) else {
            continue;
        };

        let camera_res = hal.camera(&cam_info.id);
        log_if_error(&camera_res, "Get camera from HAL");
        let Ok(camera) = camera_res else { continue; };

        // INDI drivers may not expose SCOPE_INFO, so errors are just logged here
        let focal_len_res = camera.set_telescope_focal_len(focal_len);
        log_if_error(&focal_len_res, "Set telescope focal length for camera");
    }
    Ok(())
}

/// Focal length (mm) to push to this camera; `None` means skip the camera.
/// The camera-to-telescope mapping source of truth is `CameraInfo::ccd`
pub fn focal_len_for_cam(cam_info: &CameraInfo, options: &Options) -> Option<f64> {
    match cam_info.ccd {
        // Camera on the main telescope (the secondary telescope camera shares it,
        // same as `PlatesolveMode::calc_focal_len()` treats it)
        CcdPurpose::MainTelescopeCcd |
        CcdPurpose::SecondaryTelescopeCcd => Some(options.telescope.real_focal_length()),
        // Camera on the guide telescope
        CcdPurpose::GuiderCcd => Some(options.guiding.foc_len),
        // `Unknown` (all ASCOM/Alpaca cameras, extra INDI devices): treat the active one as main
        CcdPurpose::Unknown if cam_info.id == options.cam.device_id =>
            Some(options.telescope.real_focal_length()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(ccd: CcdPurpose, id: &str) -> CameraInfo {
        CameraInfo { id: id.to_string(), name: String::new(), ccd }
    }

    #[test]
    fn focal_len_for_cam_selection() {
        let mut options = Options::default();
        options.telescope.focal_len = 600.0;
        options.telescope.barlow = 2.0;
        options.guiding.foc_len = 200.0;
        options.cam.device_id = "cam_main".to_string();

        assert_eq!(focal_len_for_cam(&cam(CcdPurpose::MainTelescopeCcd, "x"), &options), Some(1200.0));
        assert_eq!(focal_len_for_cam(&cam(CcdPurpose::GuiderCcd, "x"), &options), Some(200.0));
        // Unknown purpose (ASCOM/Alpaca): the active camera is treated as the main one
        assert_eq!(focal_len_for_cam(&cam(CcdPurpose::Unknown, "cam_main"), &options), Some(1200.0));
        assert_eq!(focal_len_for_cam(&cam(CcdPurpose::Unknown, "other"), &options), None);
        // Secondary telescope camera is on the main telescope too
        assert_eq!(focal_len_for_cam(&cam(CcdPurpose::SecondaryTelescopeCcd, "x"), &options), Some(1200.0));
    }
}
