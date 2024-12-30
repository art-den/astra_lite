#[derive(Clone)]
pub struct CamInfo {
    pub names:  &'static [&'static str],
    pub sensor: &'static str,
    pub wb:     [f32; 3],
}

const CAM_INFO: &[CamInfo] = &[
    CamInfo {
        names:  &["asi294mc", "sv405cc"],
        sensor: "Sony IMX294",
        wb:     [1.255, 1.000, 1.607],
    },

    CamInfo {
        names:  &["atr3cmos26000kpa", "touptek atr2600c"],
        sensor: "Sony IMX571",
        wb:     [1.251, 1.000, 1.548],
    },

    CamInfo {
        names:  &["asi6200mc"],
        sensor: "Sony IMX455",
        wb:     [1.225, 1.000, 1.526],
    },

    CamInfo {
        names:  &["asi178mc"],
        sensor: "Sony IMX178",
        wb:     [1.332, 1.000, 1.572],
    },

    CamInfo {
        names:  &["asi183mc"],
        sensor: "Sony IMX183",
        wb:     [1.293, 1.000, 1.574],
    },
];

pub fn get_cam_info(cam_name: &str) -> Option<CamInfo> {
    let cam_name_lc = cam_name.to_lowercase();
    CAM_INFO.iter()
        .find(|info|
            info.names.iter().any(|name|
                cam_name_lc.contains(name)
            )
        )
        .cloned()
}