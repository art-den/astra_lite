use std::sync::Arc;
use crate::indi;

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum CcdPurpose {
    MainTelescopeCcd,
    SecodnaryTelescopeCcd,
    GuiderCcd,
    Unknown,
}

pub struct CcdPurposeItem {
    pub device_name: Arc<String>,
    pub cam_ccd:     indi::CamCcd,
    pub purpose:     CcdPurpose,
}

pub fn get_all_ccd_with_purposes_list(indi: &indi::Connection) -> anyhow::Result<Vec<CcdPurposeItem>> {
    struct SensorSize {
        device: indi::ExportDevice,
        sensor_width: isize,
    }

    let mut all_cemeras: Vec<_> = indi.get_devices_list_by_interface(indi::DriverInterface::CCD)
        .iter()
        .filter_map(|d| {
            let fun = || -> anyhow::Result<SensorSize> {
                let (pixel_size_x, _) = indi.camera_get_pixel_size_um(&d.name, indi::CamCcd::Primary)?;
                let (sensor_width, _) = indi.camera_get_max_frame_size(&d.name, indi::CamCcd::Primary)?;
                Ok(SensorSize {
                    device: d.clone(),
                    sensor_width: (pixel_size_x * sensor_width as f64) as _,
                })
            };
            fun().ok()
        })
        .collect();

    all_cemeras.sort_by_key(|ss| -ss.sensor_width);
    let all_cemeras_len = all_cemeras.len();

    let mut result = Vec::new();

    for (idx, camera) in all_cemeras.into_iter().enumerate() {
        let purpose = if idx == 0 || *camera.device.name == "CCD Simulator" {
            CcdPurpose::MainTelescopeCcd
        } else if (idx == 1 && all_cemeras_len == 2) || *camera.device.name == "Guide Simulator" {
            CcdPurpose::GuiderCcd
        } else {
            CcdPurpose::Unknown
        };

        result.push(CcdPurposeItem {
            device_name: camera.device.name.clone(),
            cam_ccd: indi::CamCcd::Primary,
            purpose,
        });

        if indi.property_exists(&camera.device.name, "CCD2", None).unwrap_or(false) {
            let purpose = if idx == 0 {
                CcdPurpose::SecodnaryTelescopeCcd
            } else {
                CcdPurpose::Unknown
            };
            result.push(CcdPurposeItem {
                device_name: camera.device.name.clone(),
                cam_ccd:     indi::CamCcd::Secondary,
                purpose,
            });
        }
    }
    Ok(result)
}

pub fn get_ccd_purpose(
    indi:        &indi::Connection,
    device_name: &str,
    cam_ccd:     indi::CamCcd
) -> anyhow::Result<CcdPurpose> {
    let all_ccd_list = get_all_ccd_with_purposes_list(indi)?;
    let result = all_ccd_list
        .iter()
        .find(|ccd| ccd.cam_ccd == cam_ccd && *ccd.device_name == device_name)
        .map(|ccd| ccd.purpose)
        .unwrap_or(CcdPurpose::Unknown);
    Ok(result)
}
