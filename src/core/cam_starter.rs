use std::sync::Arc;

use crate::{hal::Camera, options::{CamCtrlOptions, FrameOptions}};

pub fn take_shot(
    camera:    &Arc<dyn Camera + Send + Sync>,
    frame:     &FrameOptions,
    cam_ctrl:  &CamCtrlOptions,
) -> anyhow::Result<u64> {
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

    // Binning

    if camera.is_binning_supported()? {
        camera.set_binning(frame.binning.get_ratio(), frame.binning.get_ratio())?;
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
