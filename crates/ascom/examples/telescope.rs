//! Drives a mount end to end: connect, unpark, slew, axis motion, guide pulse,
//! final state, disconnect. ProgID from `ASCOM_TELESCOPE_PROG_ID`, default OmniSim.

use ascom::device::AscomDevice;
use ascom::prelude::*;
use ascom::telescope::TelescopeAxis;
use ascom::wait;
use std::time::Duration;

/// Takes the mount out of a park left behind by an earlier run.
///
/// The simulator outlives the process, and a parked mount (`AtPark == true`,
/// `Tracking == false`) raises `ParkedException` on every motion call, so this must
/// run before any motion. `Unpark` alone does not stop the park motion, and V4
/// requires `AbortSlew` to refuse while the driver still calls itself parked, so
/// each step tolerates the error it may raise. Same sequence as `ensure_unparked`
/// in the live tests.
fn ensure_unparked(scope: &Telescope) {
    match scope.at_park() {
        Ok(true) => println!("note: mount found parked, unparking"),
        Ok(false) => return,
        Err(error) => {
            println!("note: AtPark unreadable, leaving the park state alone: {error}");
            return;
        }
    }
    if let Err(error) = scope.unpark() {
        println!("note: Unpark: {error}");
    }
    if let Err(error) = scope.abort_slew() {
        println!("note: AbortSlew: {error}");
    }
    if let Err(error) =
        wait::wait_flag_false(scope.actor(), "Slewing", WaitSpec::new(Duration::from_secs(30)))
    {
        println!("note: Slewing did not clear after unparking: {error}");
    }
}

/// Renders a read as its value, or as the driver's refusal.
fn report(read: Result<bool>) -> String {
    read.map_or_else(|error| error.to_string(), |value| value.to_string())
}

/// Everything done while the mount is connected. Errors are returned rather than
/// propagated with `?` so the caller always gets to run its disconnect cleanup.
fn drive(scope: &Telescope) -> Result<()> {
    // Connect before reading identity: not every driver allows it while disconnected.
    scope.set_connected(true)?;
    println!("{} / {}", scope.name()?, scope.driver_version()?);
    println!("connected, interface v{}", scope.interface_version()?);

    // Before any motion call: a leftover park survives the process and would make
    // every motion below raise `ParkedException`.
    ensure_unparked(scope);

    // Where we start.
    println!(
        "start: RA={:.4}h Dec={:.4}deg alt={:.2} az={:.2} slewing={}",
        scope.right_ascension()?,
        scope.declination()?,
        scope.altitude()?,
        scope.azimuth()?,
        scope.slewing()?,
    );

    // Site for the simulator's sake; drivers without CanSetSiteXxx reject the write.
    let site: [(&str, std::result::Result<(), String>); 3] = [
        ("SiteLatitude", scope.set_site_latitude(48.9).map_err(|e| e.to_string())),
        ("SiteLongitude", scope.set_site_longitude(2.3).map_err(|e| e.to_string())),
        ("SiteElevation", scope.set_site_elevation(35.0).map_err(|e| e.to_string())),
    ];
    for (what, result) in site {
        let outcome = match result {
            Ok(()) => "ok".to_string(),
            Err(error) => format!("rejected ({error})"),
        };
        println!("set {what}: {outcome}");
    }

    let caps = scope.capabilities()?;
    println!(
        "CanSlew={} CanSlewAsync={} CanFindHome={} CanPark={}",
        caps.supports("CanSlew"),
        caps.supports("CanSlewAsync"),
        caps.supports("CanFindHome"),
        caps.supports("CanPark"),
    );
    // `CanMoveAxis` is a method taking an axis, not a `CanXxx` flag, so it is read
    // per axis rather than from the capability snapshot.
    println!(
        "CanMoveAxis(Primary)={}",
        scope.can_move_axis(TelescopeAxis::Primary)?
    );

    // `SlewToCoordinatesAsync` requires `Tracking == true`, and the parked state
    // rejects the write, so this runs after `ensure_unparked`.
    if caps.supports("CanSetTracking") {
        match scope.set_tracking(true) {
            Ok(()) => println!("tracking enabled"),
            Err(error) => println!("set Tracking: rejected ({error})"),
        }
    }

    // Slew to a concrete target and wait the spec's completion property out.
    let wait = WaitSpec::new(Duration::from_secs(60));
    scope.slew_to_coordinates(12.0, 45.0, wait)?;
    println!(
        "after slew: RA={:.4}h Dec={:.4}deg (target {} / {})",
        scope.right_ascension()?,
        scope.declination()?,
        scope.target_right_ascension()?,
        scope.target_declination()?,
    );

    // Axis rates and a short rate move, stopped by a zero-rate MoveAxis.
    // `Primary` is the RA axis of a German equatorial mount.
    match scope.axis_rates(TelescopeAxis::Primary) {
        Ok(rates) => {
            println!("AxisRates(Primary): {} ranges", rates.len());
            let movable = scope.can_move_axis(TelescopeAxis::Primary).unwrap_or(false);
            if let Some(range) = rates.first().copied().filter(|_| movable) {
                let rate = (range.minimum + range.maximum) / 2.0;
                println!("MoveAxis(Primary, {rate:.3} deg/s) for 500 ms");
                let start = scope.move_axis(TelescopeAxis::Primary, rate);
                std::thread::sleep(Duration::from_millis(500));
                // The zero-rate stop runs unconditionally and no `?` may escape
                // between the two calls: a driver can rate-move despite a failed
                // start, and a rate move survives the process.
                let stop = scope.move_axis(TelescopeAxis::Primary, 0.0);
                if let Err(error) = start {
                    println!("MoveAxis(Primary, {rate:.3}) failed: {error}");
                }
                match stop {
                    Ok(()) => println!("MoveAxis(Primary, 0): stopped"),
                    Err(error) => println!("MoveAxis(Primary, 0) stop failed: {error}"),
                }
            }
        }
        Err(error) => println!("AxisRates: {error}"),
    }

    // Guide pulse: fire it, then watch the completion flag with the async variant.
    if caps.supports("CanPulseGuide") {
        scope.pulse_guide_and_wait(
            ascom::device::GuideDirection::North,
            200,
            WaitSpec::new(Duration::from_secs(20)),
        )?;
        println!("pulse guide done");
    }

    println!("tracking_rates: {:?}", scope.tracking_rates()?);
    println!("alignment: {:?}", scope.alignment_mode()?);

    // The state the simulator keeps once this process is gone. Nothing here parks
    // the mount on purpose: the next run would start at `AtPark == true`.
    println!(
        "final state: AtPark={} Slewing={} Tracking={}",
        report(scope.at_park()),
        report(scope.slewing()),
        report(scope.tracking()),
    );
    Ok(())
}

fn main() -> Result<()> {
    let prog_id = std::env::var("ASCOM_TELESCOPE_PROG_ID")
        .unwrap_or_else(|_| "ASCOM.OmniSim.Telescope".to_string());
    println!("== telescope {prog_id} ==");

    let scope = Telescope::open(&DeviceSpec::new(&prog_id))?;
    // From here the mount may hold a connection, and the library never disconnects
    // on drop (src/actor.rs), so no `?` may skip the cleanup below.
    let outcome = drive(&scope);

    // Always attempt the disconnect and report its outcome; a failure is printed
    // here, never propagated past the cleanup it follows.
    match scope.set_connected(false) {
        Ok(()) => println!("disconnected, bye"),
        Err(error) => println!("disconnect failed: {error}"),
    }
    outcome?;
    Ok(())
}
