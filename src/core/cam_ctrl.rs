use std::sync::Arc;

use crate::{core::core::ModeData, hal::{Camera, CcdPurpose, Hal}, options::{CamCtrlOptions, FrameOptions, Options}};

pub fn take_shot(
    camera:    &Arc<dyn Camera + Send + Sync>,
    frame:     &FrameOptions,
    cam_ctrl:  &CamCtrlOptions,
) -> eyre::Result<u64> {
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

    if camera.is_binning_supported()? {
        camera.set_binning(frame.binning.get_ratio(), frame.binning.get_ratio())?;
    }

    // Frame type

    camera.set_frame_type(frame.frame_type)?;

    // Frame

    if camera.is_frame_supported()? {
        let (width, height) = camera.ccd_size()?;
        let crop_width = frame.crop.translate(width);
        let crop_height = frame.crop.translate(height);
        camera.set_frame(
            (width - crop_width) / 2, (height - crop_height) / 2,
            crop_width, crop_height
        )?;
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

    Ok(0)
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
    mode:       &mut ModeData,
    frame_opts: &FrameOptions,
    ctrl_opts:  &CamCtrlOptions,
) -> eyre::Result<()> {
    log::info!("Begin restart exposure of camera {}...", camera.id());

    // Try to restart exposure by current mode
    let restarted_by_mode = mode.active.restart_cam_exposure()?;
    if restarted_by_mode {
        log::info!("Exposure of camera {} restarted by mode!", camera.id());
        return Ok(());
    }

    // Mode not restarted the camera exposure. Do it itself
    _ = camera.abort_exposure();

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
        if cam_info.ccd == CcdPurpose::Unknown {
            continue;
        }

        let camera = hal.camera(&cam_info.id)?;

        match cam_info.ccd {
            CcdPurpose::MainTelescopeCcd => {
                camera.set_telescope_focal_len(options.telescope.real_focal_length())?;
            }
            CcdPurpose::GuiderCcd => {
                camera.set_telescope_focal_len(options.guiding.foc_len)?;
            }
            _ => {},
        }
    }
    Ok(())
}
