//! End-to-end tests of the public glue, [`starsolver::run_into`]: the whole
//! chain CLI -> index dirs -> sweep -> the stdout success block, with the sinks
//! in memory — the seam the crate-internal unit tests deliberately bypass.
//!
//! The data gating lives in `tests/common` (self-skip without data;
//! `ASTROMETRY_REQUIRE_TEST_DATA=1` turns a skip into a panic).

mod common;

use std::path::Path;

use clap::Parser;
use starsolver::{Args, run_into};

use common::Indices;

/// The success block is exactly five lines.
const SOLUTION_LINES: usize = 5;
/// Rounding of the `{:.6}`-printed center.
const PRINTED_CENTER_EPS: f64 = 5e-6;
/// C `%g` keeps six significant digits, so that is the printed-vs-data slack.
const PRINTED_G_EPS: f64 = 1e-4;

fn solve_args(
    index_dir: Option<&Path>,
    extras: &[&str],
    file: &Path,
    width: &str,
    height: &str,
) -> Args {
    let mut argv: Vec<String> = vec!["starsolver".into()];
    if let Some(dir) = index_dir {
        argv.push("-I".into());
        argv.push(dir.to_string_lossy().into_owned());
    }
    argv.extend(extras.iter().map(|arg| (*arg).to_string()));
    argv.extend([
        "-w".into(),
        width.into(),
        "-e".into(),
        height.into(),
        file.to_string_lossy().into_owned(),
    ]);
    Args::parse_from(argv)
}

/// The `--index-dir` to pin for a resolved index set (`System` lets the
/// library use its own discovery, like the bare binary).
fn pin(index: &Indices) -> Option<&Path> {
    match index {
        Indices::System => None,
        Indices::Pin(dir) => Some(dir),
    }
}

/// The exit-2 leg (`Err`), hermetic: the pinned empty index dir resolves
/// fine, the missing `.xyls` fails the run before anything is printed.
#[test]
fn missing_input_is_an_error() {
    let empty = common::empty_dir("missing-input");
    let args = solve_args(
        Some(&empty),
        &[],
        Path::new("no-such-field.xyls"),
        "100",
        "100",
    );
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let err = run_into(&args, &mut out, &mut progress).expect_err("a missing .xyls must be an Err");
    assert!(
        out.is_empty(),
        "an Err run must not print a solution: {err:#}"
    );
}

/// A pinned index dir without index files is an input error like any other
/// (the exit-2 leg: `index_files` insists), not a silent "not solved". The
/// `.xyls` is read before the dirs are scanned, so a real star file is
/// needed to reach that error.
#[test]
fn empty_index_dir_is_an_error() {
    let Some(file) = common::testdata_file("1920x1080-1.xyls") else {
        return;
    };
    let empty = common::empty_dir("empty-index");
    let args = solve_args(Some(&empty), &[], &file, "1920", "1080");
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let err =
        run_into(&args, &mut out, &mut progress).expect_err("an index dir without files must Err");
    assert!(
        out.is_empty(),
        "an Err run must not print a solution: {err:#}"
    );
}

/// The exit-1 leg (`Ok(None)`): the indices load but none fits the asked
/// scale range; the glue warning must reach the `progress` sink, and `out`
/// must stay empty.
#[test]
fn unmatched_scale_range_is_not_solved() {
    let Some(file) = common::testdata_file("1920x1080-1.xyls") else {
        return;
    };
    let Some(index) = common::indices() else {
        return;
    };
    // A scale range no shipped index can fit.
    let args = solve_args(
        pin(&index),
        &["-u", "arcsecperpix", "-L", "0.0001", "-H", "0.0002"],
        &file,
        "1920",
        "1080",
    );
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let sol = run_into(&args, &mut out, &mut progress).expect("a scale miss is not an error");
    assert!(sol.is_none(), "no index fits 0.0001..0.0002 arcsec/pixel");
    assert!(
        out.is_empty(),
        "no solution block when not solved: {}",
        String::from_utf8_lossy(&out)
    );
    let text = String::from_utf8(progress).expect("progress is UTF-8");
    assert!(text.contains("Warning: no index in "), "progress: {text}");
}

/// The exit-0 leg end-to-end: the returned `Solution` and the printed success
/// block are the same data; a blind non-verbose run keeps `progress` empty.
#[test]
fn solved_field_writes_the_solution_block() {
    let Some(file) = common::testdata_file("1920x1080-1.xyls") else {
        return;
    };
    let Some(index) = common::indices() else {
        return;
    };
    let args = solve_args(pin(&index), &[], &file, "1920", "1080");
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let sol = run_into(&args, &mut out, &mut progress)
        .expect("run")
        .expect("the installed index set must solve 1920x1080-1 blind");
    let text = String::from_utf8(out).expect("the solution block is UTF-8");
    assert_eq!(
        text.lines().count(),
        SOLUTION_LINES,
        "the success block is five lines:\n{text}"
    );
    let printed = common::parse_solution(&text).expect("the five-line block parses");
    // The printed values are the returned data, at the printed precision.
    assert!(
        f64::abs(printed.ra - sol.ra) < PRINTED_CENTER_EPS,
        "printed {} vs data {}",
        printed.ra,
        sol.ra
    );
    assert!(
        f64::abs(printed.dec - sol.dec) < PRINTED_CENTER_EPS,
        "printed {} vs data {}",
        printed.dec,
        sol.dec
    );
    assert!(
        common::rel_frac(printed.width_deg, sol.width_deg) < PRINTED_G_EPS
            && common::rel_frac(printed.height_deg, sol.height_deg) < PRINTED_G_EPS,
        "printed size {}x{} vs data {}x{}",
        printed.width_deg,
        printed.height_deg,
        sol.width_deg,
        sol.height_deg
    );
    assert!(
        common::rel_frac(printed.orient_deg, sol.orient_deg) < PRINTED_G_EPS,
        "printed {} vs data {}",
        printed.orient_deg,
        sol.orient_deg
    );
    assert_eq!(printed.parity == "neg", sol.parity_neg);
    assert!(
        progress.is_empty(),
        "a blind non-verbose run wrote: {}",
        String::from_utf8_lossy(&progress)
    );
}

/// The hidden `--dump-index` leg: the dump lands on `out`, nothing is solved
/// (the `run` wrapper still exits 0 for this case).
#[test]
fn dump_index_writes_its_summary_to_out() {
    let Some(fits) = common::one_index_file() else {
        return;
    };
    let fits_name = fits.to_string_lossy().into_owned();
    let args = Args::parse_from(["starsolver", "--dump-index", &fits_name]);
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let sol = run_into(&args, &mut out, &mut progress).expect("--dump-index must run");
    assert!(sol.is_none(), "--dump-index never solves");
    assert!(!out.is_empty(), "the dump must reach `out`");
    assert!(progress.is_empty(), "--dump-index prints no progress");
}
