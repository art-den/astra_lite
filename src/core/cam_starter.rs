use std::{sync::{Arc, RwLock}};

use crate::{core::{consts::INDI_SET_PROP_TIMEOUT, core::ModeType}, indi, options::{Binning, CamCtrlOptions, DeviceAndProp, FrameOptions}};

pub struct CamStarter {
    before_shot_fun: RwLock<Option<Box<dyn Fn(ModeType) + Send + Sync + 'static>>>,
    indi:            Arc<indi::Connection>,
}

impl CamStarter {
    pub fn new(indi: &Arc<indi::Connection>) -> Self {
        Self {
            before_shot_fun: RwLock::new(None),
            indi:            Arc::clone(indi),
        }
    }

    pub fn connect_before_shot_fun(&self, fun: impl Fn (ModeType) + Send + Sync + 'static) {
        let mut before_shot_fun = self.before_shot_fun.write().unwrap();
        *before_shot_fun = Some(Box::new(fun));
    }

    pub fn take_shot(
        &self,
        mode_type: ModeType,
        device:    &DeviceAndProp,
        frame:     &FrameOptions,
        cam_ctrl:  &CamCtrlOptions,
    ) -> anyhow::Result<u64> {

        let before_shot_fun = self.before_shot_fun.read().unwrap();
        if let Some(before_shot_fun) = &*before_shot_fun {
            before_shot_fun(mode_type);
        }
        drop(before_shot_fun);

        let cam_ccd = indi::CamCcd::from_ccd_prop_name(&device.prop);

        // Disable fast toggle

        if self.indi.camera_is_fast_toggle_supported(&device.name).unwrap_or(false) {
            self.indi.camera_enable_fast_toggle(&device.name, false, false, None)?;
        }

        // Conversion gain

        if let Some(conv_gain_str) = &cam_ctrl.conv_gain_str {
            if self.indi.camera_is_conversion_gain_str_supported(&device.name)? {
                self.indi.camera_set_conversion_gain_str(
                    &device.name,
                    conv_gain_str,
                    true,
                    INDI_SET_PROP_TIMEOUT
                )?;
            }
        }

        // Low noise mode

        if self.indi.camera_is_low_noise_supported(&device.name)? {
            self.indi.camera_set_low_noise(
                &device.name,
                cam_ctrl.low_noise,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // High fullwell mode

        if self.indi.camera_is_high_fullwell_supported(&device.name)? {
            self.indi.camera_set_high_fullwell(
                &device.name,
                cam_ctrl.high_fullwell,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Polling period

        if self.indi.device_is_polling_period_supported(&device.name)? {
            self.indi.device_set_polling_period(&device.name, 500, true, None)?;
        }

        // Frame type

        use crate::image::raw::*; // for FrameType::
        let frame_type = match frame.frame_type {
            FrameType::Lights => indi::FrameType::Light,
            FrameType::Flats  => indi::FrameType::Flat,
            FrameType::Darks  => indi::FrameType::Dark,
            FrameType::Biases => indi::FrameType::Bias,
            FrameType::Undef  => panic!("Undefined frame type"),
        };

        self.indi.camera_set_frame_type(
            &device.name,
            cam_ccd,
            frame_type,
            true,
            INDI_SET_PROP_TIMEOUT
        )?;

        // Frame size

        if self.indi.camera_is_frame_supported(&device.name, cam_ccd)? {
            let (width, height) = self.indi.camera_get_max_frame_size(&device.name, cam_ccd)?;
            let crop_width = frame.crop.translate(width);
            let crop_height = frame.crop.translate(height);
            self.indi.camera_set_frame_size(
                &device.name,
                cam_ccd,
                (width - crop_width) / 2,
                (height - crop_height) / 2,
                crop_width,
                crop_height,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Make binning mode is alwais AVG (if camera supports it)

        if self.indi.camera_is_binning_mode_supported(&device.name, cam_ccd)?
        && frame.binning != Binning::Orig {
            self.indi.camera_set_binning_mode(
                &device.name,
                indi::BinningMode::Avg,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Binning

        if self.indi.camera_is_binning_supported(&device.name, cam_ccd)? {
            self.indi.camera_set_binning(
                &device.name,
                cam_ccd,
                frame.binning.get_ratio(),
                frame.binning.get_ratio(),
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Gain

        if self.indi.camera_is_gain_supported(&device.name)? {
            self.indi.camera_set_gain(
                &device.name,
                frame.gain,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Offset

        if self.indi.camera_is_offset_supported(&device.name)? {
            self.indi.camera_set_offset(
                &device.name,
                frame.offset as f64,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Capture format = RAW

        if self.indi.camera_is_capture_format_supported(&device.name)? {
            self.indi.camera_set_capture_format(
                &device.name,
                indi::CaptureFormat::Raw,
                true,
                INDI_SET_PROP_TIMEOUT
            )?;
        }

        // Start exposure

        let shot_id = self.indi.camera_start_exposure(
            &device.name,
            indi::CamCcd::from_ccd_prop_name(&device.prop),
            frame.exposure()
        )?;

        Ok(shot_id)
    }

    pub fn abort(
        &self,
        device: &DeviceAndProp,
    ) -> anyhow::Result<()> {
        self.indi.camera_abort_exposure(
            &device.name,
            indi::CamCcd::from_ccd_prop_name(&device.prop)
        )?;
        Ok(())
    }


}

