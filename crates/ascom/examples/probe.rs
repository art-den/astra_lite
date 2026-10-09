//! Walks any ASCOM driver over pure late binding and prints what it finds.
//!
//! A probe must never abort on a driver's answer, so every member is printed either
//! as a value or as the error the driver raised — that answer is the information.
//!
//! ProgID from argv[1], else `ASCOM_PROG_ID`, else `ASCOM.OmniSim.Telescope`.

use ascom::com::variant;
use ascom::device::DeviceSpec;
use ascom::prelude::*;
use std::time::Duration;

/// Prints a member's value, or the error it raised.
fn show(label: &str, read: impl FnOnce() -> Result<String>) {
    match read() {
        Ok(value) => println!("{label:<20}: {value}"),
        Err(error) => println!("{label:<20}: {error}"),
    }
}

/// The family a ProgID claims by its last segment, lowercased. ProgIDs are
/// case-insensitive (`CoCreateInstance` accepts any spelling), so every match
/// against this segment must be too.
fn family_of(prog_id: &str) -> String {
    prog_id.rsplit('.').next().unwrap_or_default().to_ascii_lowercase()
}

/// Members worth reading once connected, per family. The last segment of a ProgID
/// is conventionally the family name (`ASCOM.OmniSim.FilterWheel`).
fn family_members(prog_id: &str) -> &'static [&'static str] {
    match family_of(prog_id).as_str() {
        "telescope" => &["Altitude", "RightAscension", "Declination", "Slewing", "AtHome", "AtPark"],
        "focuser" => &["Absolute", "IsMoving", "Position", "StepSize", "Temperature"],
        "camera" => &["CameraXSize", "CameraYSize", "ExposureState", "TemperatureSetPoint"],
        "filterwheel" => &["Connecting", "Names", "FocusOffsets", "Position"],
        _ => &["Connecting"],
    }
}

/// Connects and reads the connected-state members. Errors are returned rather than
/// propagated with `?` so the caller can always run its disconnect cleanup first.
fn probe_connected(device: &Telescope, prog_id: &str) -> Result<()> {
    device.set_connected(true)?;
    std::thread::sleep(Duration::from_millis(100));
    println!("Connected         : {} (connected)", device.connected()?);

    // A couple of interface-specific reads, each tolerated when missing.
    for member in family_members(prog_id) {
        show(member, || device.probe(member));
    }
    Ok(())
}

fn main() -> Result<()> {
    let prog_id = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("ASCOM_PROG_ID").ok())
        .unwrap_or_else(|| "ASCOM.OmniSim.Telescope".to_string());
    println!("== probing {prog_id} ==");

    // Any of the four families answers the common members; a Telescope handle is
    // as good a vehicle as any for the probe itself.
    let device = Telescope::open(&DeviceSpec::new(&prog_id))?;

    // The spec requires these five to work while disconnected. A driver that says
    // `NotConnected` here is deviating, and this is where that becomes visible.
    println!("-- identity, as required while disconnected --");
    show("Name", || device.name());
    show("Description", || device.description());
    show("DriverInfo", || device.driver_info());
    show("DriverVersion", || device.driver_version());
    show("InterfaceVersion", || device.interface_version().map(|v| v.to_string()));
    show("Connected", || device.connected().map(|v| v.to_string()));
    show("UTCDate", || {
        device
            .utc_date()
            // Same human-readable UTC form Variant::describe() uses for VT_DATE.
            .map(|when| variant::format_date_utc(variant::system_time_to_date(when)))
    });
    show("SupportedActions", || device.supported_actions().map(|a| format!("{a:?}")));

    // The flag table is read through the telescope's own member list, so printing it for
    // another family would only list members that family does not have.
    let is_telescope = family_of(&prog_id) == "telescope";
    match device.capabilities() {
        Ok(snapshot) if is_telescope => {
            println!("interface         : v{}", snapshot.interface_version);
            for (member, value) in &snapshot.flags {
                // `None` means the member raised "not implemented", which is not `false`.
                let text = match value {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "not implemented",
                };
                println!("  {member:<18}: {text}");
            }
        }
        Ok(snapshot) => println!(
            "interface         : v{} (flag table is telescope-specific, skipped)",
            snapshot.interface_version
        ),
        Err(error) => println!("capabilities      : {error}"),
    }

    // The two-argument Action contract: an unknown name must come back as
    // ActionNotImplemented, never E_INVALIDARG (see docs/KNOWN_DRIVER_QUIRKS.md).
    for (name, params) in [
        ("__nope__", ""),
        ("__nope__", "0"),
        ("AssemblyVersionNumber", ""),
    ] {
        match device.action(name, params) {
            Ok(answer) => println!("Action({name},{params:?}) : Ok({answer:?})"),
            Err(error) => println!(
                "Action({name},{params:?}) : {:?} :: {error}",
                error.kind
            ),
        }
    }

    // From here the device may hold a connection, and the library never disconnects
    // on drop (src/actor.rs), so no `?` may skip the cleanup below.
    let outcome = probe_connected(&device, &prog_id);

    // Always attempt the disconnect and report its outcome; a failure is printed
    // here, never propagated past the cleanup it follows.
    match device.set_connected(false) {
        Ok(()) => println!("disconnect        : ok"),
        Err(error) => println!("disconnect        : failed: {error}"),
    }
    outcome?;

    println!("-- identity again, now that Connected is false --");
    show("Name", || device.name());
    show("InterfaceVersion", || device.interface_version().map(|v| v.to_string()));
    println!("disconnected, bye");
    Ok(())
}
