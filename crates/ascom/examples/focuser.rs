//! Drives a focuser: connect, read geometry, move, halt, temperature
//! compensation. ProgID from `ASCOM_FOCUSER_PROG_ID`, default OmniSim.
//!
//! Note: IFocuserV4 has no `CanXxx` members — capabilities come from `Absolute`,
//! `TempCompAvailable` and (COM only) `Link`, and `Halt` is probed once.

use ascom::prelude::*;
use std::time::Duration;

/// Renders a read as its value, or as the driver's refusal.
fn report<T: ToString>(result: Result<T>) -> String {
    result.map_or_else(|error| format!("unreadable ({error})"), |value| value.to_string())
}

/// Attempts `Halt()` after a move that did not finish cleanly.
///
/// `Move` is not idempotent and never retried, so the driver may already be running
/// when its call fails. The stop is attempted and its outcome printed, never
/// propagated over the error that prompted it.
fn try_halt(focuser: &Focuser) {
    match focuser.halt() {
        Ok(()) => println!("Halt: ok"),
        Err(error) => println!("Halt after the incomplete move: {error}"),
    }
}

/// Everything done while the focuser is connected. Errors are returned rather than
/// propagated with `?` so the caller always gets to run its disconnect cleanup.
fn drive(focuser: &Focuser) -> Result<()> {
    // Connect before reading identity: not every driver allows it while disconnected.
    focuser.set_connected(true)?;
    println!("{} / {}", focuser.name()?, focuser.driver_version()?);
    println!("connected, interface v{}", focuser.interface_version()?);

    let absolute = focuser.absolute()?;
    println!(
        "Absolute={absolute} IsMoving={} Position={} Temperature={:.2}",
        focuser.is_moving()?,
        focuser.position()?,
        focuser.temperature()?,
    );
    println!(
        "StepSize={:?} MaxIncrement={:?} MaxStep={:?}",
        focuser.step_size().ok(),
        focuser.max_increment().ok(),
        focuser.max_step().ok(),
    );

    // Move by up to 50 steps, clipped by MaxIncrement. For a relative focuser the
    // driver interprets Move's argument as an offset; for an absolute one as a
    // target — the spec's single `Move` member covers both.
    let start = focuser.position()?;
    // MaxIncrement is a mandatory member, so a failed read propagates instead of
    // standing in for a move size; `0` is the driver's "undocumented", which leaves
    // no size to derive, so the move is ruled out rather than invented.
    let max_increment = focuser.max_increment()?;
    let step = if max_increment <= 0 {
        println!("note: MaxIncrement={max_increment} offers no move size, Move skipped");
        None
    } else {
        Some(i32::min(max_increment, 50))
    };
    if let Some(step) = step {
        // saturating: `start` is driver data and must not overflow the addition.
        let target = if absolute { start.saturating_add(step) } else { step };
        println!("Move({target}) from position {start}");
        if let Err(error) = focuser.move_to_and_wait(target, WaitSpec::new(Duration::from_secs(60)))
        {
            println!("Move({target}) failed: {error}");
            try_halt(focuser);
            return Err(error);
        }
        // Reads here are not propagated: the restore below must run even when the
        // driver refuses to report where it ended up.
        println!(
            "arrived at {} (IsMoving={})",
            report(focuser.position()),
            report(focuser.is_moving()),
        );

        // Restored in both modes: a relative `Move` consumed a delta, so the device
        // sits off `start` either way. A failed restore is printed, not propagated —
        // the remaining sections and the disconnect still have their turn.
        let back = if absolute { start } else { -step };
        if let Err(error) = focuser.move_to_and_wait(back, WaitSpec::new(Duration::from_secs(60)))
        {
            println!("restore Move({back}) failed: {error}");
            try_halt(focuser);
        }
        println!("ended at position {}", report(focuser.position()));
    }

    // Halt on an idle focuser is legal: the spec says it must simply do nothing.
    // V3 drivers raise MethodNotImplemented, which probe_halt records once.
    println!("Halt supported: {:?}", focuser.probe_halt());
    if focuser.halt_supported() == Some(true) {
        println!("Halt: {:?}", focuser.halt().map(|_| "ok"));
    }

    // Temperature compensation: readable everywhere, writable when supported.
    let before = focuser.temp_comp()?;
    match focuser.set_temp_comp(!before) {
        Ok(()) => {
            // Read without `?` so a refusal cannot skip the write that restores it.
            println!("TempComp flipped {before} -> {}", report(focuser.temp_comp()));
            focuser.set_temp_comp(before)?;
        }
        Err(error) if error.is_unsupported() => println!("TempComp is read-only here"),
        Err(error) => println!("set TempComp: {error}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let prog_id = std::env::var("ASCOM_FOCUSER_PROG_ID")
        .unwrap_or_else(|_| "ASCOM.OmniSim.Focuser".to_string());
    println!("== focuser {prog_id} ==");

    let focuser = Focuser::open(&DeviceSpec::new(&prog_id))?;
    // From here the focuser may hold a connection, and the library never disconnects
    // on drop (src/actor.rs), so no `?` may skip the cleanup below.
    let outcome = drive(&focuser);

    // Always attempt the disconnect and report its outcome; a failure is printed
    // here, never propagated past the cleanup it follows.
    match focuser.set_connected(false) {
        Ok(()) => println!("disconnected, bye"),
        Err(error) => println!("disconnect failed: {error}"),
    }
    outcome?;
    Ok(())
}
