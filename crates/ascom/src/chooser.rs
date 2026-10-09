//! The platform driver chooser (`ASCOM.Utilities.Chooser`).
//!
//! Strongly recommended by the ASCOM COM guide: the chooser lists the drivers
//! registered for a device type, forces the user to open the driver's own config
//! dialog before accepting a change, and can hand back a dynamic driver that bridges
//! to an Alpaca device — all without the application knowing anything about it.
//!
//! The chooser is an in-process .NET server, so its dialog runs on *our* thread;
//! that is why this module creates its own STA thread and pumps messages around the
//! modal call.
//!
//! Needs a human and a desktop. For a non-interactive list of the same registrations,
//! see [`crate::drivers`].

use std::thread;

use crate::com::apartment;
use crate::com::dispatch::Dispatch;
use crate::com::variant::Variant;
use crate::com::ComGuard;
use crate::error::{AscomError, AscomErrorKind, Result};

/// ProgID of the platform chooser.
pub const CHOOSER_PROG_ID: &str = "ASCOM.Utilities.Chooser";

/// Device families the chooser can list.
///
/// `Dome` and the rest are listed because the platform supports them, even though
/// this crate only wraps the three interfaces in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceType {
    Telescope,
    Focuser,
    Camera,
    Dome,
    Rotator,
    FilterWheel,
    CoverCalibrator,
    ObservingConditions,
    SafetyMonitor,
    Switch,
    Video,
    /// Anything the platform does not have a fixed name for.
    Other(&'static str),
}

impl DeviceType {
    /// The string the `DeviceType` property expects.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Telescope => "Telescope",
            Self::Focuser => "Focuser",
            Self::Camera => "Camera",
            Self::Dome => "Dome",
            Self::Rotator => "Rotator",
            Self::FilterWheel => "FilterWheel",
            Self::CoverCalibrator => "CoverCalibrator",
            Self::ObservingConditions => "ObservingConditions",
            Self::SafetyMonitor => "SafetyMonitor",
            Self::Switch => "Switch",
            Self::Video => "Video",
            Self::Other(name) => name,
        }
    }
}

/// Shows the platform chooser and returns the ProgID the user picked.
///
/// `current` pre-selects a previously saved ProgID (the platform's usual
/// "remember my last choice" flow); pass `None` to start with nothing selected.
/// `Ok(None)` means the user cancelled.
///
/// Runs on its own STA thread so the caller's thread — which may be a worker with no
/// COM initialisation, or a UI thread — is not disturbed.
pub fn choose(device_type: DeviceType, current: Option<&str>) -> Result<Option<String>> {
    let hint = current.unwrap_or("").to_string();
    let wanted = device_type.as_str();
    thread::Builder::new()
        .name(format!("ascom-chooser-{wanted}"))
        .spawn(move || choose_on_this_thread(wanted, &hint))
        .map_err(|e| {
            AscomError::local(
                AscomErrorKind::Com,
                "Chooser",
                format!("could not start the chooser thread: {e}"),
            )
        })?
        .join()
        .map_err(|_| {
            AscomError::local(AscomErrorKind::Com, "Chooser", "the chooser thread panicked")
        })?
}

/// The chooser body. Must run on a thread with no COM initialisation yet.
fn choose_on_this_thread(device_type: &str, hint: &str) -> Result<Option<String>> {
    let _com = ComGuard::new()?;
    let chooser = Dispatch::from_prog_id(CHOOSER_PROG_ID)?;
    chooser.set_string("DeviceType", device_type)?;

    let value = {
        let hint_v = Variant::from_str(hint);
        chooser.call("Choose", &[&hint_v])
    };
    // The modal dialog pumps itself, but anything left in the queue after it closes
    // belongs to windows we still own.
    apartment::pump_messages();

    match value? {
        Some(v) => {
            let prog_id = v.as_str()?;
            // An empty string is how the chooser reports "the user pressed Cancel".
            Ok(if prog_id.trim().is_empty() { None } else { Some(prog_id) })
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_type_names_match_the_platform_strings() {
        assert_eq!(DeviceType::Telescope.as_str(), "Telescope");
        assert_eq!(DeviceType::FilterWheel.as_str(), "FilterWheel");
        assert_eq!(DeviceType::CoverCalibrator.as_str(), "CoverCalibrator");
        assert_eq!(DeviceType::Other("Custom").as_str(), "Custom");
    }

    #[test]
    fn an_unknown_device_type_is_still_passed_through() {
        // The platform defines the accepted names; nothing here rejects an unknown
        // family, so a newer platform gains new types without a crate change.
        assert_eq!(DeviceType::Other("Spectrograph").as_str(), "Spectrograph");
    }
}
