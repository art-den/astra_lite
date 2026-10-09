//! Machine-readable list of the ASCOM drivers registered on this host.
//!
//! The platform's own answer to "which drivers are there?" is the modal Chooser
//! ([`crate::chooser`]), which needs a human and a desktop. This module reads what the
//! Chooser reads — the registration key each driver writes when it installs,
//! `HKLM\SOFTWARE\ASCOM\<Family> Drivers\<ProgID>` — so a CLI, a service or a test
//! suite can enumerate drivers without a dialog.
//!
//! There is no COM call for this. `ASCOM.Utilities.Chooser` exposes only `DeviceType`
//! and `Choose`; asking it for a `Drivers` property raises `DISP_E_UNKNOWNNAME`
//! (verified over `IDispatch` against platform 7.1), which is why the registry is read
//! directly.
//!
//! ```no_run
//! use ascom::chooser::DeviceType;
//! use ascom::drivers::installed_prog_ids;
//!
//! # fn main() -> ascom::error::Result<()> {
//! for prog_id in installed_prog_ids(DeviceType::Telescope)? {
//!     println!("{prog_id}");
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;

use crate::chooser::DeviceType;
use crate::com::registry::{self, Hive, View};
use crate::error::Result;

/// One driver registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverInfo {
    /// ProgID to hand to [`DeviceSpec::new`](crate::device::DeviceSpec), e.g.
    /// `ASCOM.OmniSim.Focuser`.
    pub prog_id: String,
    /// The registration key's default value, e.g. `ASCOM OmniSim Focuser`. `None` when
    /// the driver registered without a description.
    pub description: Option<String>,
}

/// Drivers can be registered machine-wide or per user; both are read.
const HIVES: [Hive; 2] = [Hive::Machine, Hive::User];

/// Most ASCOM drivers are 32-bit and register under `WOW6432Node`, while a 64-bit
/// reader sees the 64-bit view by default, so both views have to be merged.
const VIEWS: [View; 2] = [View::SixtyFour, View::ThirtyTwo];

/// `SOFTWARE\ASCOM\<Family> Drivers`, the key a driver installs itself under.
fn family_key(device_type: DeviceType) -> String {
    format!(r"SOFTWARE\ASCOM\{} Drivers", device_type.as_str())
}

/// Every driver registered for a device family, ordered by ProgID.
///
/// An empty list means nothing is registered for that family — including the case of
/// no ASCOM platform at all, which is deliberately not an error.
///
/// Being listed only means the driver registered itself; it does not prove the driver
/// loads, so treat activation errors from [`crate::device::DeviceSpec`] as normal for
/// stale registrations.
pub fn installed_drivers(device_type: DeviceType) -> Result<Vec<DriverInfo>> {
    let mut found: BTreeMap<String, DriverInfo> = BTreeMap::new();
    for hive in HIVES {
        let path = family_key(device_type);
        for view in VIEWS {
            for prog_id in registry::sub_key_names(hive, &path, view)? {
                let description =
                    registry::default_string(hive, &format!(r"{path}\{prog_id}"), view)?;
                record(&mut found, DriverInfo { prog_id, description });
            }
        }
    }
    Ok(found.into_values().collect())
}

/// Just the ProgIDs, for callers that do not care about the descriptions.
pub fn installed_prog_ids(device_type: DeviceType) -> Result<Vec<String>> {
    Ok(installed_drivers(device_type)?.into_iter().map(|d| d.prog_id).collect())
}

/// Adds a driver unless the same ProgID was already seen in another hive or view.
fn record(found: &mut BTreeMap<String, DriverInfo>, driver: DriverInfo) {
    // Registry key names are case-insensitive, so the same driver can surface twice.
    let slot = driver.prog_id.to_lowercase();
    match found.get_mut(&slot) {
        // Keep the first spelling seen; fill in a description the other view lacked.
        Some(existing) if existing.description.is_none() => existing.description = driver.description,
        Some(_) => {}
        None => {
            found.insert(slot, driver);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_keys_match_the_names_the_installers_use() {
        assert_eq!(family_key(DeviceType::Telescope), r"SOFTWARE\ASCOM\Telescope Drivers");
        assert_eq!(family_key(DeviceType::CoverCalibrator), r"SOFTWARE\ASCOM\CoverCalibrator Drivers");
        // An unknown family is passed through, so a newer platform needs no crate change.
        assert_eq!(
            family_key(DeviceType::Other("Spectrograph")),
            r"SOFTWARE\ASCOM\Spectrograph Drivers"
        );
    }

    #[test]
    fn the_same_prog_id_in_two_views_collapses_to_one_entry() {
        let mut found = BTreeMap::new();
        record(&mut found, DriverInfo { prog_id: "Acme.Focuser".into(), description: None });
        record(
            &mut found,
            DriverInfo { prog_id: "ACME.FOCUSER".into(), description: Some("Acme".into()) },
        );
        let drivers: Vec<DriverInfo> = found.values().cloned().collect();
        assert_eq!(drivers.len(), 1);
        // The first spelling wins, the missing description was filled from the second.
        assert_eq!(drivers[0].prog_id, "Acme.Focuser");
        assert_eq!(drivers[0].description.as_deref(), Some("Acme"));
    }

    #[test]
    fn results_are_ordered_by_prog_id() {
        let mut found = BTreeMap::new();
        for name in ["Zeta.One", "alpha.Two", "Mid.Three"] {
            record(&mut found, DriverInfo { prog_id: name.into(), description: None });
        }
        let ids: Vec<&str> = found.values().map(|d| d.prog_id.as_str()).collect();
        assert_eq!(ids, ["alpha.Two", "Mid.Three", "Zeta.One"]);
    }
}
