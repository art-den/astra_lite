//! Lists every ASCOM driver registered on this machine, per device family.
//!
//! Headless on purpose: it reads the registration keys the platform's Chooser reads,
//! so it works over a remote shell with no desktop. No COM object is created, so no
//! driver is loaded and no device is touched.
//!
//! Optional argv[1] restricts the listing to one family name (e.g. `Camera`).
//!
//! Exit status: 0 when every family was read, 1 when at least one registry read
//! failed (the listing is then only partial), 2 when argv[1] names no known family.

use ascom::prelude::*;
use std::process::ExitCode;

/// Every family the platform knows, mirroring `DeviceType`. The list is written out
/// because the enum is `#[non_exhaustive]` and carries a runtime `Other(name)`, so it
/// cannot be enumerated outside the crate. The printable label, in contrast, comes
/// from `DeviceType::as_str` — the same helper the driver list uses to build the
/// registry key — so the label and the key read cannot drift apart.
const FAMILIES: [DeviceType; 11] = [
    DeviceType::Telescope,
    DeviceType::Focuser,
    DeviceType::Camera,
    DeviceType::Dome,
    DeviceType::Rotator,
    DeviceType::FilterWheel,
    DeviceType::CoverCalibrator,
    DeviceType::ObservingConditions,
    DeviceType::SafetyMonitor,
    DeviceType::Switch,
    DeviceType::Video,
];

/// True when `filter` names one of the listed families, case-insensitively.
fn known_family(filter: &str) -> bool {
    FAMILIES.iter().any(|family| filter.eq_ignore_ascii_case(family.as_str()))
}

fn main() -> ExitCode {
    let wanted = std::env::args().nth(1);
    if let Some(filter) = &wanted
        && !known_family(filter) {
            let names: Vec<&str> = FAMILIES.iter().map(|family| family.as_str()).collect();
            // A typo must not be reported as an install with no drivers.
            eprintln!("unknown family '{filter}'; known families: {}", names.join(", "));
            return ExitCode::from(2);
        }

    let mut total = 0usize;
    let mut listed = 0usize;
    let mut failed = 0usize;

    for family in FAMILIES {
        let name = family.as_str();
        if let Some(filter) = &wanted
            && !filter.eq_ignore_ascii_case(name) {
                continue;
            }
        // One family's registry failure must not hide the other families' answers.
        match installed_drivers(family) {
            Ok(drivers) => {
                println!("{name} ({}):", drivers.len());
                for DriverInfo { prog_id, description } in &drivers {
                    match description {
                        Some(text) => println!("  {prog_id:<34} {text}"),
                        None => println!("  {prog_id}"),
                    }
                }
                total += drivers.len();
                listed += 1;
            }
            Err(error) => {
                println!("{name}: {error}");
                failed += 1;
            }
        }
    }

    // A listing that could not read every family is incomplete, and must say so.
    match failed {
        0 => println!("total: {total} registered drivers"),
        n => {
            println!(
                "total: {total} registered drivers (partial: {n} of {} families failed to read)",
                listed + n
            );
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
