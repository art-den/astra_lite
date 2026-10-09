//! Platform Chooser: opens the platform's **modal** device-selection dialog.
//!
//! This example needs a visible desktop and a human to click OK/Cancel, so it is
//! never part of automated tests. Run it manually:
//! `cargo run --example chooser`

use ascom::device::DeviceSpec;
use ascom::prelude::*;

/// Renders a read as its value, or as the driver's refusal.
fn report<T: ToString>(result: Result<T>) -> String {
    result.map_or_else(|error| format!("unreadable ({error})"), |value| value.to_string())
}

fn main() -> Result<()> {
    // Context, not a preview of the dialog: the dialog can run the picked driver's
    // Setup dialog and can hand back a dynamic driver that bridges to an Alpaca
    // device (src/chooser.rs), so the name it returns need not be one of these.
    match installed_prog_ids(DeviceType::Telescope) {
        Ok(ids) => println!("installed telescope drivers: {ids:?}"),
        Err(error) => println!("could not read the registry: {error}"),
    }

    println!("a modal dialog will open; pick a telescope driver (Cancel yields None)");
    match choose(DeviceType::Telescope, None)? {
        Some(prog_id) => {
            println!("chosen: {prog_id}");
            // Prove the picked name is usable immediately. Only the activation may
            // abort the run: identity members are printed as a value or as the
            // driver's refusal, because a legacy driver has no `Name` and no
            // `InterfaceVersion` at all.
            let device = Telescope::open(&DeviceSpec::new(&prog_id))?;
            println!("{} — v{}", report(device.name()), report(device.interface_version()));
        }
        None => println!("nothing chosen (dialog cancelled)"),
    }
    Ok(())
}
