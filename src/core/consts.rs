pub const DIRECTORY: &str = "Astro";
pub const RAW_FRAMES_DIR: &str = "RawFrames";
pub const LIVE_STACKING_DIR: &str = "LiveStacking";

/// Maximum time in seconds to wait while mount slews to target
pub const MAX_GOTO_TIME: usize = 180;

/// Maximum length of guide impulse in seconds
pub const MAX_TIMED_GUIDE_TIME: f64 = 3.0;

/// How many seconds to wait after mount position correction
pub const AFTER_MOUNT_MOVE_WAIT_TIME: usize = 2;

/// Speed for mount calibration and correction if the mount supports it
/// (relative to mount tracking speed)
pub const MOUNT_CALIBR_SPEED: f64 = 1.0;

pub const AFTER_GOTO_WAIT_TIME: usize = 3; // seconds
