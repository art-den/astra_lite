//! Tests for driver discovery.
//!
//! These assert facts about a host that has the ASCOM Platform installed rather than
//! facts about a device, so no simulator has to be running — only the platform, which
//! every machine here has. They cost milliseconds, touch no device and move nothing.
//!
//! Run: `cargo test --test drivers` (part of a plain `cargo test`).

use ascom::chooser::DeviceType;
use ascom::drivers::{DriverInfo, installed_drivers, installed_prog_ids};
// Registry reads are safe in parallel, but `#[serial]` stays: one rule for every
// live suite, so a new test cannot be added to the wrong lane by accident.
use serial_test::serial;

/// The three families this crate wraps.
const FAMILIES: [DeviceType; 3] = [DeviceType::Telescope, DeviceType::Focuser, DeviceType::Camera];

/// The simulator every ASCOM Platform 7 install ships with must be listed, which is how
/// a caller can find a driver to test against without hard-coding a ProgID.
#[serial]
#[test]
fn the_shipped_simulator_is_listed_for_every_wrapped_family() {
    for family in FAMILIES {
        let wanted = format!("ASCOM.OmniSim.{}", family.as_str());
        let ids = installed_prog_ids(family).unwrap_or_else(|e| panic!("{}: {e}", family.as_str()));
        assert!(ids.contains(&wanted), "expected {wanted} among {ids:?}");
    }
}

/// A ProgID is what gets handed to `CoCreateInstance`, so a malformed entry would make
/// the whole list useless.
#[serial]
#[test]
fn every_entry_looks_like_a_prog_id() {
    for family in FAMILIES {
        let drivers = installed_drivers(family).expect("readable");
        assert!(!drivers.is_empty(), "{} lists nothing", family.as_str());
        for DriverInfo { prog_id, .. } in &drivers {
            assert!(!prog_id.trim().is_empty(), "empty ProgID in {}", family.as_str());
            assert!(prog_id.contains('.'), "{prog_id} is not Manufacturer.DeviceType");
        }
    }
}

/// The registration key's default value is the human-readable name; a list without any
/// of them is not useful in a picker.
#[serial]
#[test]
fn descriptions_are_reported_where_the_driver_supplied_one() {
    for family in FAMILIES {
        let drivers = installed_drivers(family).expect("readable");
        let named = drivers.iter().filter(|d| d.description.is_some()).count();
        assert!(
            named > 0,
            "no driver in {} registered a description: {drivers:?}",
            family.as_str()
        );
    }
}

/// Listing must not load a driver. Instantiating the simulators would leave devices
/// connected, so this is the only side-effect check the suite can safely make.
#[serial]
#[test]
fn listing_is_repeatable_and_ordered() {
    for family in FAMILIES {
        let first = installed_prog_ids(family).expect("first read");
        // An empty list is equal to itself and trivially sorted, so without this the
        // test would stay green even if the read stopped seeing the 32-bit registry
        // view entirely (`drivers::installed_drivers` maps a missing key to an empty
        // list, deliberately).
        assert!(
            !first.is_empty(),
            "{} lists no drivers, so neither assertion below can fail: {first:?}",
            family.as_str()
        );
        let second = installed_prog_ids(family).expect("second read");
        assert_eq!(first, second, "{} changed between reads", family.as_str());
        let mut sorted = first.clone();
        sorted.sort_unstable_by_key(|id| id.to_lowercase());
        assert_eq!(first, sorted, "{} is not ordered by ProgID", family.as_str());
    }
}
