pub struct CamInfo {
    /// Lowercase name fragments matched against the FITS `INSTRUME` value.
    /// Same-sensor models intentionally share one entry (one coefficient set).
    pub names:  &'static [&'static str],
    pub sensor: &'static str,
    /// Display WB multipliers, R/G/B. In the data below G is the 1.000 reference.
    pub wb:     [f64; 3],
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

/// Case-insensitive substring match of `cam_name` against `names`.
/// Only the query is case-folded (ASCII), so DB literals must stay lowercase.
pub fn get_cam_info(cam_name: &str) -> Option<&'static CamInfo> {
    CAM_INFO.iter()
        .find(|info|
            info.names.iter().any(|name|
                contains_ignore_ascii_case(cam_name, name)
            )
        )
}

// All DB needles are lowercase ASCII: byte-wise case-insensitive `contains`, no allocation
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() {
        return true;
    }
    if n.len() > h.len() {
        return false;
    }
    h.windows(n.len()).any(|w|
        w.iter().zip(n).all(|(hc, nc)| u8::to_ascii_lowercase(hc) == *nc)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_ignore_ascii_case_basics() {
        assert!(contains_ignore_ascii_case("", ""));
        assert!(!contains_ignore_ascii_case("abc", "abcd"));
        assert!(contains_ignore_ascii_case("ZWO ASI294MC-Pro", "asi294mc"));
        assert!(contains_ignore_ascii_case("ToupTek ATR2600C", "touptek atr2600c"));
        assert!(!contains_ignore_ascii_case("ZWO ASI178MM", "asi178mc"));
        // a non-ASCII needle never matches ASCII haystack bytes
        assert!(!contains_ignore_ascii_case("ASI294MC", "asi294mc\u{430}"));
    }

    #[test]
    fn get_cam_info_lookup() {
        assert_eq!(get_cam_info("ZWO ASI294MC").unwrap().sensor, "Sony IMX294");
        assert_eq!(get_cam_info("SV405CC").unwrap().sensor, "Sony IMX294");
        assert_eq!(get_cam_info("touptek atr2600c").unwrap().sensor, "Sony IMX571");
        assert_eq!(get_cam_info("ZWO ASI178MM").map(|c| c.sensor), None);
    }
}
