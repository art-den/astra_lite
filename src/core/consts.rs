pub const INDI_SET_PROP_TIMEOUT: Option<u64> = Some(1000);

pub const DIRECTORY: &str = "Astro";
pub const RAW_FRAMES_DIR: &str = "RawFrames";
pub const LIVE_STACKING_DIR: &str = "LiveStacking";

/// Maximum time in seconds to wait while mount slew to target
pub const MAX_GOTO_TIME: usize = 180;

/// Maximus length of guide impuilse in seconds
pub const MAX_TIMED_GUIDE_TIME: f64 = 3.0;

/// How many seconds to wait after mount position correction
pub const AFTER_MOUNT_MOVE_WAIT_TIME: usize = 2;

/// Speed for mount calibration and correction if mount support it
/// (in mount track speed)
pub const MOUNT_CALIBR_SPEED: f64 = 1.0;