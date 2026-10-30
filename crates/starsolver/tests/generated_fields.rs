//! Fields whose sky truth is exact by construction.
//!
//! [`starsolver::genfield`] projects reference stars of an installed index file
//! through a WCS the test chooses, so the printed center, size, rotation and
//! parity have nothing to be compared against but the four numbers that produced
//! the star list - unlike the shipped `testdata/` fields, whose truth is a
//! reference run carrying real-observation noise. These cases therefore need no
//! `testdata/`, only the installed index set, and they gate the whole CLI ->
//! index-dir -> sweep -> stdout chain about ten times tighter than
//! `tests/ground_truth.rs` can.
//!
//! The projection itself lives in the crate and is reached through the
//! `test-util` feature (see `Cargo.toml`); this target deliberately holds no
//! copy of it, which is the reason that module is published at all.
//!
//! Gating lives in `tests/common`: a case self-skips when the installed set
//! cannot produce a field, and `ASTROMETRY_REQUIRE_TEST_DATA=1` turns a skip
//! into a panic.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use starsolver::genfield::{self, Truth};
use starsolver::{Args, run_into};

/// Frame of every generated field here.
const WIDTH: f64 = 1920.0;
const HEIGHT: f64 = 1080.0;

/// Star count of the distractor-only field (the depth ramp is a function of the
/// star count, so a long noise list is a long run, not a stronger gate).
const NOISE_STARS: usize = 30;

/// Blind draws of the API-level gate, and the index picks spent to get them. The
/// unit gate in `sweep.rs` makes ten over the whole installed set; this one
/// covers the CLI half - the blind scale range and the library's own index
/// selection - so three draws are enough to catch a wrong band or a selection
/// that prunes the winner. A pick whose index hosts no field at this frame is
/// re-rolled, which is what the pick budget is for.
const BLIND_DRAWS: usize = 3;
const BLIND_PICKS: usize = 30;
/// First seed of the blind draws: draw `n` is driven by `BLIND_SEED + n`.
const BLIND_SEED: u64 = 200;

/// The numbers of a report, whichever side they came from: the returned
/// `Solution` or the success block parsed back from stdout.
struct Report {
    ra: f64,
    dec: f64,
    width_deg: f64,
    height_deg: f64,
    orient_deg: f64,
    parity_neg: bool,
}

impl From<&starsolver::Solution> for Report {
    fn from(s: &starsolver::Solution) -> Self {
        Report {
            ra: s.ra,
            dec: s.dec,
            width_deg: s.width_deg,
            height_deg: s.height_deg,
            orient_deg: s.orient_deg,
            parity_neg: s.parity_neg,
        }
    }
}

impl From<&common::Printed> for Report {
    fn from(p: &common::Printed) -> Self {
        Report {
            ra: p.ra,
            dec: p.dec,
            width_deg: p.width_deg,
            height_deg: p.height_deg,
            orient_deg: p.orient_deg,
            parity_neg: p.parity == "neg",
        }
    }
}

/// The gate: a report against the exact truth. The center budget is in pixels of
/// the field's own scale, so it means the same thing at every plate scale.
fn check_against_truth(got: &Report, truth: &Truth) {
    let off_px = common::center_off_arcsec(got.ra, got.dec, truth.ra_deg, truth.dec_deg)
        / truth.scale_arcsec;
    assert!(
        off_px <= genfield::TOL_CENTER_PX,
        "center off by {off_px} px (budget {}): got ({}, {}), truth ({}, {})",
        genfield::TOL_CENTER_PX,
        got.ra,
        got.dec,
        truth.ra_deg,
        truth.dec_deg
    );
    let drot = common::normalize_deg(got.orient_deg - truth.orient_deg);
    assert!(
        f64::abs(drot) <= genfield::TOL_ORIENT_DEG,
        "rotation off by {drot} deg: got {}, truth {}",
        got.orient_deg,
        truth.orient_deg
    );
    for (name, got_deg, want_deg) in [
        ("width", got.width_deg, truth.width_deg),
        ("height", got.height_deg, truth.height_deg),
    ] {
        assert!(
            common::rel_frac(got_deg, want_deg) <= genfield::TOL_SIZE_FRAC,
            "{name} {got_deg} deg vs truth {want_deg} deg"
        );
    }
    assert_eq!(got.parity_neg, truth.parity_neg, "parity");
}

/// The index directory to pin, plus the index file the field's stars come from.
/// The run is pinned to that directory rather than left to the library's own
/// discovery: the field only solves with the index its band came from in play.
fn index_set() -> Option<(PathBuf, PathBuf)> {
    let dir = common::one_index_dir()?;
    match genfield::default_index(&dir) {
        Ok(file) => Some((dir, file)),
        Err(e) => {
            common::skip(&format!("{}: {e:#}", dir.display()));
            None
        }
    }
}

/// `Params` of a seeded field, or a skip when the installed set has no plate
/// scale for a frame this size.
fn params(index: &Path, seed: u64) -> Option<genfield::Params> {
    match genfield::random_params(index, seed, WIDTH, HEIGHT) {
        Ok(p) => Some(p),
        Err(e) => {
            common::skip(&format!(
                "no generated field from {}: {e:#}",
                index.display()
            ));
            None
        }
    }
}

/// The `-w`/`-e` arguments of these fields, from the frame consts.
fn frame_args() -> (String, String) {
    (WIDTH.to_string(), HEIGHT.to_string())
}

/// `Params` of the blind draws: fields from pseudo-randomly chosen index files of
/// `dir`, in pick order, stopping at `BLIND_DRAWS`. Files that host no field at
/// this frame (a coarse all-sky index has no narrow field) are skipped, not
/// failed - which files do is a property of the install.
fn blind_draws(dir: &Path) -> Vec<genfield::Params> {
    let mut draws = Vec::new();
    for n in 0..BLIND_PICKS as u64 {
        if draws.len() == BLIND_DRAWS {
            break;
        }
        let seed = BLIND_SEED + n;
        let Ok(index) = genfield::random_index(dir, seed) else {
            break;
        };
        if let Ok(p) = genfield::random_params(&index, seed, WIDTH, HEIGHT) {
            draws.push(p);
        }
    }
    draws
}

/// Writes a field as `.xyls` into a fresh scratch directory.
fn write_field(tag: &str, field: &genfield::Field) -> PathBuf {
    let path = common::empty_dir(tag).join("field.xyls");
    genfield::write_xyls(field, &path).expect("write the generated .xyls");
    path
}

/// The CLI scale window of a field, as `-L`/`-H` arguments.
fn scale_args(scale_arcsec: f64) -> (String, String) {
    let (lo, hi) = genfield::scale_window(scale_arcsec);
    (lo.to_string(), hi.to_string())
}

/// Gate: a clean generated field solves through the public glue, and both the
/// returned `Solution` and the printed success block equal the truth the field
/// was projected from. Also the `progress`-stays-empty promise for a field with
/// nothing unusual in it.
#[test]
fn generated_field_solves_to_its_exact_truth() {
    let Some((dir, index)) = index_set() else {
        return;
    };
    let Some(p) = params(&index, 101) else {
        return;
    };
    let field = genfield::generate(&p).expect("generate");
    let path = write_field("generated-solve", &field);
    let (lo, hi) = scale_args(p.scale_arcsec);
    let (w_s, h_s) = frame_args();
    let dir_s = dir.to_string_lossy().into_owned();
    let path_s = path.to_string_lossy().into_owned();

    let args = Args::parse_from([
        "starsolver",
        "-I",
        &dir_s,
        "-u",
        "arcsecperpix",
        "-L",
        &lo,
        "-H",
        &hi,
        "-w",
        &w_s,
        "-e",
        &h_s,
        &path_s,
    ]);
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let sol = run_into(&args, &mut out, &mut progress)
        .expect("run")
        .expect("a clean generated field must solve with the index it was made from");
    check_against_truth(&Report::from(&sol), &field.truth);

    let text = String::from_utf8(out).expect("the success block is UTF-8");
    let printed = common::parse_solution(&text).expect("the five-line block parses");
    check_against_truth(&Report::from(&printed), &field.truth);
    assert!(
        progress.is_empty(),
        "a blind non-verbose run of a clean field wrote: {}",
        String::from_utf8_lossy(&progress)
    );
}

/// Gate against false positives through the whole chain: a frame of
/// uniform-random stars - the bad-pixel pattern, with no real pattern under it -
/// is not solved, so nothing is printed and the run is the exit-1 `Ok(None)`.
#[test]
fn generated_distractor_only_field_is_not_solved() {
    let Some((dir, index)) = index_set() else {
        return;
    };
    // Only the plate scale of a real field is borrowed, so the run is pointed at
    // a band that exists instead of the blind 0.1-180 deg field-width range.
    let Some(p) = params(&index, 102) else {
        return;
    };
    let field = genfield::noise_field(WIDTH, HEIGHT, p.scale_arcsec, NOISE_STARS, 7);
    let path = write_field("generated-noise", &field);
    let (lo, hi) = scale_args(p.scale_arcsec);
    let (w_s, h_s) = frame_args();
    let dir_s = dir.to_string_lossy().into_owned();
    let path_s = path.to_string_lossy().into_owned();

    let args = Args::parse_from([
        "starsolver",
        "-I",
        &dir_s,
        "-u",
        "arcsecperpix",
        "-L",
        &lo,
        "-H",
        &hi,
        "-w",
        &w_s,
        "-e",
        &h_s,
        &path_s,
    ]);
    let (mut out, mut progress) = (Vec::new(), Vec::new());
    let sol = run_into(&args, &mut out, &mut progress).expect("noise is not an input error");
    assert!(
        sol.is_none(),
        "{NOISE_STARS} random stars solved: {:?}",
        sol.map(|s| (s.ra, s.dec, s.orient_deg, s.parity_neg))
    );
    assert!(
        out.is_empty(),
        "no solution block when not solved: {}",
        String::from_utf8_lossy(&out)
    );
}

/// Gate: the built binary, given a `--ra/--dec` hint at the truth center, exits
/// 0 and prints the truth. This is the hint path end to end on a field with an
/// exact center: the hint also prunes the index tiles, and the field still has
/// to come back from the tile that contains it.
#[test]
fn generated_field_solves_through_the_binary_with_a_position_hint() {
    let Some((dir, index)) = index_set() else {
        return;
    };
    let Some(p) = params(&index, 103) else {
        return;
    };
    let field = genfield::generate(&p).expect("generate");
    let path = write_field("generated-binary", &field);
    let (lo, hi) = scale_args(p.scale_arcsec);
    let (w_s, h_s) = frame_args();
    let dir_s = dir.to_string_lossy().into_owned();
    let path_s = path.to_string_lossy().into_owned();
    let ra = p.ra_deg.to_string();
    let dec = p.dec_deg.to_string();

    let run = Command::new(env!("CARGO_BIN_EXE_starsolver"))
        .args([
            "-I",
            &dir_s,
            "-u",
            "arcsecperpix",
            "-L",
            &lo,
            "-H",
            &hi,
            "--ra",
            &ra,
            "--dec",
            &dec,
            "-w",
            &w_s,
            "-e",
            &h_s,
            &path_s,
        ])
        .output()
        .expect("run the built binary");
    assert!(
        run.status.success(),
        "exit {:?}, stderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    let text = String::from_utf8(run.stdout).expect("the success block is UTF-8");
    let printed = common::parse_solution(&text).expect("the five-line block parses");
    check_against_truth(&Report::from(&printed), &field.truth);
}

/// Gate: fields from pseudo-randomly chosen index files, run with no `-L`/`-H`
/// and no hint - the blind mode a user gets by default. Unlike the leg above,
/// nothing points the run at the band the field was made from, so the CLI's scale
/// fallback and the sweep's index selection are both under test here; the index
/// that wins need not be the one the stars came from.
#[test]
fn generated_fields_solve_blind_through_the_api() {
    let Some(dir) = common::one_index_dir() else {
        return;
    };
    let draws = blind_draws(&dir);
    if draws.is_empty() {
        common::skip(&format!(
            "{} hosts no index a narrow generated field can come from",
            dir.display()
        ));
        return;
    }
    let dir_s = dir.to_string_lossy().into_owned();
    let (w_s, h_s) = frame_args();
    for p in &draws {
        let field = genfield::generate(p).expect("generate");
        let path = write_field("generated-blind", &field);
        let path_s = path.to_string_lossy().into_owned();
        let args = Args::parse_from(["starsolver", "-I", &dir_s, "-w", &w_s, "-e", &h_s, &path_s]);
        let (mut out, mut progress) = (Vec::new(), Vec::new());
        let sol = run_into(&args, &mut out, &mut progress)
            .expect("run")
            .unwrap_or_else(|| {
                panic!(
                    "{} ({} stars at {} arcsec/pixel) did not solve blind",
                    p.index.display(),
                    field.truth.n_source,
                    field.truth.scale_arcsec
                )
            });
        check_against_truth(&Report::from(&sol), &field.truth);
        let text = String::from_utf8(out).expect("the success block is UTF-8");
        let printed = common::parse_solution(&text).expect("the five-line block parses");
        check_against_truth(&Report::from(&printed), &field.truth);
        assert!(
            progress.is_empty(),
            "a blind non-verbose run wrote: {}",
            String::from_utf8_lossy(&progress)
        );
    }
}
