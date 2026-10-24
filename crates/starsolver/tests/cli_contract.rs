//! The binary's own contract, checked out-of-process: the exit codes of
//! `src/main.rs` (0 solved / 1 not solved / 2 error) and a `--verbose`
//! liveness guard — the verbose lines are printed from rayon workers, so a
//! process-stdio lock held across the solve would deadlock them (the exact
//! regression this file exists for).

mod common;

use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::Indices;

/// A `-v` solve of the small field finishes well under this on any profile;
/// exceeding it means a hang, not slowness.
const VERBOSE_BUDGET: Duration = Duration::from_secs(120);

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_starsolver"))
}

/// Polls `try_wait` until the child exits or the budget is spent; kills and
/// returns `None` on timeout (`std` has no timed `wait`).
fn wait_timeout(child: &mut Child, budget: Duration) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn pin_args(index: &Indices, cmd: &mut Command) {
    if let Indices::Pin(dir) = index {
        cmd.args(["--index-dir", &dir.to_string_lossy()]);
    }
}

/// The exit-2 leg: a missing `.xyls` is an input error. Hermetic (an empty
/// pinned index dir resolves fine; the star read fails regardless).
#[test]
fn missing_input_exits_2() {
    let empty = common::empty_dir("cli-missing-input");
    let empty_name = empty.to_string_lossy().into_owned();
    let proc = binary()
        .arg("no-such-field.xyls")
        .args(["--index-dir", &empty_name, "-w", "100", "-e", "100"])
        .output()
        .expect("the binary runs");
    assert_eq!(
        proc.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&proc.stderr)
    );
    assert!(
        String::from_utf8_lossy(&proc.stderr).contains("Error:"),
        "stderr: {}",
        String::from_utf8_lossy(&proc.stderr)
    );
    assert!(proc.stdout.is_empty(), "an error run prints no solution");
}

/// The exit-1 leg: indices present but none fits the scale range.
#[test]
fn unsolved_field_exits_1() {
    let Some(file) = common::testdata_file("1920x1080-1.xyls") else {
        return;
    };
    let Some(index) = common::indices() else {
        return;
    };
    let mut cmd = binary();
    cmd.arg(&file).args([
        "-u",
        "arcsecperpix",
        "-L",
        "0.0001",
        "-H",
        "0.0002",
        "-w",
        "1920",
        "-e",
        "1080",
    ]);
    pin_args(&index, &mut cmd);
    let proc = cmd.output().expect("the binary runs");
    assert_eq!(
        proc.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&proc.stderr)
    );
    assert!(
        String::from_utf8_lossy(&proc.stderr).contains("Field not solved"),
        "stderr: {}",
        String::from_utf8_lossy(&proc.stderr)
    );
    assert!(proc.stdout.is_empty(), "an unsolved run prints no solution");
}

/// The exit-0 leg for the hidden `--dump-index` flag: it solves nothing yet
/// must still exit 0 with a non-empty dump.
#[test]
fn dump_index_exits_0() {
    let Some(fits) = common::one_index_file() else {
        return;
    };
    let fits_name = fits.to_string_lossy().into_owned();
    let proc = binary()
        .args(["--dump-index", &fits_name])
        .output()
        .expect("the binary runs");
    assert!(
        proc.status.success(),
        "exit {:?}, stderr: {}",
        proc.status.code(),
        String::from_utf8_lossy(&proc.stderr)
    );
    assert!(!proc.stdout.is_empty(), "the dump must reach stdout");
}

/// The deadlock regression: with `-v` the workers print `[verify]`/`[hit]`
/// lines to stderr while the main thread sweeps. The run must finish, and
/// the sinks go to `null` so a merely-full pipe can never fake a hang.
#[test]
fn verbose_solve_finishes_without_deadlocking() {
    let Some(file) = common::testdata_file("1920x1080-1.xyls") else {
        return;
    };
    let Some(index) = common::indices() else {
        return;
    };
    let mut cmd = binary();
    cmd.arg(&file).args(["-v", "-w", "1920", "-e", "1080"]);
    pin_args(&index, &mut cmd);
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary spawns");
    let status = wait_timeout(&mut child, VERBOSE_BUDGET).expect("waiting works");
    match status {
        Some(status) => assert!(status.success(), "verbose run failed: {status}"),
        None => panic!(
            "the verbose run outlived {VERBOSE_BUDGET:?} - a stdio lock held across the \
             solve deadlocks the worker prints"
        ),
    }
}
