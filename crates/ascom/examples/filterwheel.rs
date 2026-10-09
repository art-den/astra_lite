//! Drives a filter wheel: connect, list the slots, move, wait for the move to end.
//! ProgID from `ASCOM_FILTERWHEEL_PROG_ID`, default OmniSim.
//!
//! Note: IFilterWheelV3 has no `CanXxx` members and no `IsMoving`. `Position` is both
//! the command and the completion property: writing it starts the wheel, and reading it
//! back gives -1 while the wheel is turning.

use ascom::prelude::*;
use std::time::Duration;

/// Everything done while the wheel is connected. Errors are returned rather than
/// propagated with `?` so the caller always gets to run its disconnect cleanup.
fn drive(wheel: &FilterWheel) -> Result<()> {
    // Connect before reading anything: not every driver allows it while disconnected.
    wheel.set_connected(true)?;
    println!("{} / {}", wheel.name()?, wheel.driver_version()?);
    println!("connected, interface v{}", wheel.interface_version()?);

    let names = wheel.names()?;
    let offsets = wheel.focus_offsets()?;
    println!("{} slots:", names.len());
    for (index, name) in names.iter().enumerate() {
        // A driver that implements only Names leaves the offsets empty.
        match offsets.get(index) {
            Some(offset) => println!("  {index}: {name} (focus offset {offset})"),
            None => println!("  {index}: {name}"),
        }
    }

    // The MOVING sentinel is not a slot: a wheel left turning by an earlier run must
    // settle before there is a position to remember and restore. The wheel is not
    // cancellable, so waiting is all that can be done; if it never settles there is no
    // start slot, and writing to a moving wheel is refused anyway, so the move is
    // skipped with the Timeout surfaced rather than a slot invented for it.
    let mut start = wheel.position()?;
    if start == ascom::filterwheel::MOVING {
        println!("the wheel was left turning, waiting for it to stop");
        start = match wheel.wait_until_stopped(WaitSpec::new(Duration::from_secs(30))) {
            Ok(slot) => slot,
            Err(error) => {
                println!("the wheel never settled: {error}; move skipped, nothing to restore to");
                return Err(error);
            }
        };
    }
    println!("Position={start} moving={}", wheel.is_moving()?);
    if names.len() < 2 {
        println!("nothing to move to, bye");
        return Ok(());
    }

    // Write the target, then let the wrapper poll the sentinel until the wheel stops.
    let target = if start == 0 { 1 } else { 0 };
    println!("Position={target} from slot {start}");
    let arrived = wheel.move_to_and_wait(target, WaitSpec::new(Duration::from_secs(30)))?;
    // `arrived` is exactly what the driver answered and the wheel may stop elsewhere,
    // so it is printed raw and only looked up in `names`, never used as an index.
    let name = usize::try_from(arrived)
        .ok()
        .and_then(|index| names.get(index))
        .map_or("unnamed", |name| name.as_str());
    println!("stopped on slot {arrived} ({name})");

    // `start` is a real slot here (the sentinel was filtered above), so the restore
    // never writes -1, which no wheel accepts.
    println!("leaving the wheel where it started");
    wheel.move_to_and_wait(start, WaitSpec::new(Duration::from_secs(30)))?;
    println!("back on slot {:?}", wheel.filter()?);
    Ok(())
}

fn main() -> Result<()> {
    let prog_id = std::env::var("ASCOM_FILTERWHEEL_PROG_ID")
        .unwrap_or_else(|_| "ASCOM.OmniSim.FilterWheel".to_string());
    println!("== filter wheel {prog_id} ==");

    let wheel = FilterWheel::open(&DeviceSpec::new(&prog_id))?;
    // From here the wheel may hold a connection, and the library never disconnects on
    // drop (src/actor.rs), so no `?` may skip the cleanup below. The wheel cannot be
    // cancelled mid-rotation, so leaving it connected would hand the next client of
    // this shared Singleton a turning device.
    let outcome = drive(&wheel);

    // Always attempt the disconnect and report its outcome; a failure is printed
    // here, never propagated past the cleanup it follows.
    match wheel.set_connected(false) {
        Ok(()) => println!("disconnected, bye"),
        Err(error) => println!("disconnect failed: {error}"),
    }
    outcome?;
    Ok(())
}
