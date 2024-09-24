use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};

use crate::{image::raw::*, indi, options::*};


#[derive(Default)]
pub struct FileNameUtils {
    device: DeviceAndProp,
    sensor_width: usize,
    sensor_height: usize,
    cooler_supported: bool,
}

impl FileNameUtils {
    pub fn init(&mut self, indi: &Arc<indi::Connection>, device: &DeviceAndProp) {
        self.device = device.clone();
        let cam_ccd = indi::CamCcd::from_ccd_prop_name(&device.prop);
        let (sensor_width, sensor_height) =
            indi
                .camera_get_max_frame_size(&device.name, cam_ccd)
                .unwrap_or((0, 0));
        self.cooler_supported = indi
            .camera_is_cooler_supported(&device.name)
            .unwrap_or(false);
        self.sensor_width = sensor_width;
        self.sensor_height = sensor_height;
    }

    pub fn master_only_file_name(
        &self,
        time:        Option<DateTime<Utc>>, // used for flat frames
        cam_options: &CamOptions,
    ) -> PathBuf {
        cam_options.raw_master_file_name(
            time,
            self.sensor_width,
            self.sensor_height,
            self.cooler_supported
        ).into()
    }

    pub fn master_dark_file_name(
        &self,
        cam_options: &CamOptions,
        options:     &Options
    ) -> PathBuf {
        let mut cam_dark = cam_options.clone();
        cam_dark.frame.frame_type = FrameType::Darks;
        let master_dark_name =
            self.master_only_file_name(None, &cam_dark);
        let mut path = PathBuf::new();
        path.push(&options.calibr.dark_library_path);
        path.push(&self.device.to_file_name_part());
        path.push(&master_dark_name);
        path
    }

    pub fn defect_pixels_file_name(
        &self,
        cam_options: &CamOptions,
        options:     &Options
    ) -> PathBuf {
        let defect_pixels_file_name = cam_options.defect_pixels_file_name(
            self.sensor_width,
            self.sensor_height,
        );
        let mut path = PathBuf::new();
        path.push(&options.calibr.dark_library_path);
        path.push(&self.device.to_file_name_part());
        path.push(&defect_pixels_file_name);
        path
    }

    pub fn raw_file_dest_dir(
        &self,
        time:        DateTime<Utc>, // used for flat frames
        cam_options: &CamOptions,
    ) -> PathBuf {
        let save_dir = cam_options.raw_file_dest_dir(
            time,
            self.sensor_width,
            self.sensor_height,
            self.cooler_supported
        );
        PathBuf::from(&save_dir)
    }
}

