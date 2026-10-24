//! The end-to-end tolerance gate: run the built binary on every shipped field,
//! parse the five-line success block it prints back, and keep the printed WCS
//! inside the documented budgets.
//!
//! `FIELDS` is the record of the reference runs: astrometry.net's `solve-field`
//! (the installed 0.98 reference binary) on these four `.xyls` files. Each
//! entry's comment keeps what that run reported beyond the printed WCS - the
//! index that solved it, its log-odds and match counts, engine CPU and, where
//! the run published one, its full WCS - and nothing regenerates them: they are
//! what that binary said against the index set of the machine it ran on, which
//! is not the installed set these tests sweep (see `AGENTS.md`, "Data
//! directories"). The same truth is written a second time as the
//! `Truth` consts of `src/testutil.rs` (what the unit gates demand), pinned to
//! this table by `truth_consts_match_the_cargo_gate` and to the budgets below by
//! `gate_budgets_match_the_cargo_gate`.
//!
//! Beyond `testdata/` and an index set the gate needs no data.
//!
//! Two modes, because they gate different code paths:
//! - blind: the frame size is the only prior;
//! - hinted: `--ra`/`--dec` at the truth center, which is what drives the
//!   position gate - healpix tile pruning, rejecting code hits whose matched
//!   stars fall outside the circle, and discarding a finished solution whose
//!   refined center lies outside it.
//!
//! `GATE_FIELD=name[,name...]` limits a run to some of the fields; an unknown
//! name fails loudly, so a typo cannot read as a passing run.
//!
//! Time budget caveat: the budgets were calibrated on the release binary, while
//! `CARGO_BIN_EXE_starsolver` under a plain `cargo test` is the test-profile one
//! (`opt-level = 1`, several times slower), so the budget is scaled by
//! `DEBUG_TIME_FACTOR` there and by nothing under `cargo test --release`. The
//! check happens after the process exits; there is no kill - a run that goes on
//! too long ends on the solver's own `--cpulimit` wall-clock deadline (default
//! 300 s), which prints the timeout line to stderr and exits 1.

mod common;

use std::process::Command;
use std::time::Instant;

use common::Indices;

// Gate budgets. The center budget is deliberately in pixels of the field's own
// scale: 1" is 0.37 px at 2.71"/px but 1.00 px at 1.0"/px, so a fixed arcsec
// figure would not mean the same error on both.
const TOL_CENTER_PX: f64 = 1.5;
const TOL_SCALE_FRAC: f64 = 0.01;
const TOL_ORIENT_DEG: f64 = 0.5;
const TOL_SIZE_FRAC: f64 = 0.005;
/// Default per-field solve budget, seconds; a field may carry its own.
const TOL_TIME_S: f64 = 60.0;
const ARCSEC_PER_DEG: f64 = 3600.0;
/// See the module docs: the debug-profile binary is measured against a relaxed
/// budget, the release one against the budget as written.
const DEBUG_TIME_FACTOR: f64 = 4.0;
/// How much of a failing run's stderr to quote.
const STDERR_TAIL_CHARS: usize = 200;

/// A shipped field: the sky truth published for it, the frame the solver is run
/// on, and the field's overrides of the default budgets.
#[derive(Debug, Clone, Copy)]
struct FieldTruth {
    name: &'static str,
    /// Input `.xyls`, relative to the repo root.
    input: &'static str,
    width_pix: f64,
    height_pix: f64,
    ra_deg: f64,
    dec_deg: f64,
    width_deg: f64,
    height_deg: f64,
    orient_deg: f64,
    parity_neg: bool,
    scale_arcsec: f64,
    /// Absolute center budget, arcsec; wins over the pixel one. Set on the
    /// real-centroid `1920x1080-*` fields: the reference run removed a
    /// quadratic distortion term there, which our similarity-only refine cannot
    /// follow, so their center residual is not comparable with the 2.7"/px
    /// fields' one.
    tol_center_arcsec: Option<f64>,
    /// Solve budget, seconds.
    timeout_s: f64,
}

const FIELDS: &[FieldTruth] = &[
    // Reference run: `solve-field` 0.98 on index-4112, no SIP tweak, no
    // verification conflicts. It logged 183.248 log-odds from 18 matches over
    // 22 index stars and 498 field stars, 38 quads tried, a code error of
    // 2.16e-4 and 0.137 s of engine CPU. Its WCS: CRVAL (59.7194055429,
    // 35.9547818436), CRPIX (2651.20111448, 2327.06991884), CD deg/pix =
    // [[-7.53336628101e-4, -1.38160452186e-5], [1.38160452186e-5,
    // -7.53336628101e-4]], TAN with a positive recorded determinant; read off
    // that CD the rotation is -178.94932 deg at 2.7124679 arcsec/px. The
    // numbers in this entry are the console print of the same map, so they
    // carry its 6-digit rounding - part of the truth's own noise floor.
    FieldTruth {
        name: "4656x3522-1",
        input: "testdata/4656x3522-1.xyls",
        width_pix: 4656.0,
        height_pix: 3522.0,
        ra_deg: 60.031038,
        dec_deg: 36.375976,
        width_deg: 3.50687,
        height_deg: 2.65305,
        orient_deg: -178.949,
        parity_neg: true,
        scale_arcsec: 2.71247,
        tol_center_arcsec: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 on index-4111, no SIP tweak, no
    // conflicts. 341.624 log-odds from 37 matches over 41 index stars and 442
    // field stars, 47 quads tried, a code error of 1.06e-3 and 0.436 s of
    // engine CPU. Its WCS: CRVAL (43.5602492359, 61.1337539914), CRPIX
    // (2446.00435181, 825.814342432), CD deg/pix = [[-7.52541553906e-4,
    // -3.14245371815e-5], [3.14245371815e-5, -7.52541553906e-4]], which is
    // -177.608839 deg at 2.7115106 arcsec/px. This is the field whose raw 3-star
    // fit used to be race-bimodal; the refined, field-centred print lands on
    // these values within the budgets below.
    FieldTruth {
        name: "4656x3522-2",
        input: "testdata/4656x3522-2.xyls",
        width_pix: 4656.0,
        height_pix: 3522.0,
        ra_deg: 43.67983,
        dec_deg: 60.425902,
        width_deg: 3.50552,
        height_deg: 2.65188,
        orient_deg: -177.609,
        parity_neg: true,
        scale_arcsec: 2.71151,
        tol_center_arcsec: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 on the index-4206 healpix tile, with a
    // SIP tweak applied, 1.36 s of engine CPU; its log-odds and WCS were not
    // recorded. The tweak is why this field carries an absolute 2.0 arcsec
    // center budget: our similarity-only refine cannot follow a quadratic
    // distortion term, so gating this center with the pixel budget of the
    // 2.7 arcsec/px fields would demand a distortion model we do not have.
    FieldTruth {
        name: "1920x1080-1",
        input: "testdata/1920x1080-1.xyls",
        width_pix: 1920.0,
        height_pix: 1080.0,
        ra_deg: 202.484305,
        dec_deg: 47.232352,
        width_deg: 0.531127,
        height_deg: 0.298915,
        orient_deg: -74.729,
        parity_neg: true,
        scale_arcsec: 0.996643,
        tol_center_arcsec: Some(2.0),
        timeout_s: 300.0,
    },
    // Reference run: the same index-4206 tile and SIP tweak as the entry above,
    // and the slowest of the four reference runs at 23.9 s of engine CPU - the
    // origin of the 300 s budget both narrow fields carry. No log-odds or WCS
    // recorded.
    FieldTruth {
        name: "1920x1080-2",
        input: "testdata/1920x1080-2.xyls",
        width_pix: 1920.0,
        height_pix: 1080.0,
        ra_deg: 250.423162,
        dec_deg: 36.479129,
        width_deg: 0.531737,
        height_deg: 0.299092,
        orient_deg: 106.362,
        parity_neg: true,
        scale_arcsec: 0.997071,
        tol_center_arcsec: Some(2.0),
        timeout_s: 300.0,
    },
];

/// Per-field |dRA*cos(dec)|/|ddec| center budget, arcsec: the absolute one wins
/// over `TOL_CENTER_PX` times the field's own scale.
fn center_tol(f: &FieldTruth) -> f64 {
    f.tol_center_arcsec
        .unwrap_or(TOL_CENTER_PX * f.scale_arcsec)
}

/// The fields to run: all of them, or the `GATE_FIELD` subset.
fn selected() -> Vec<&'static FieldTruth> {
    let Some(want) = std::env::var("GATE_FIELD")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return FIELDS.iter().collect();
    };
    let want: Vec<&str> = want
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let unknown: Vec<&str> = want
        .iter()
        .copied()
        .filter(|w| !FIELDS.iter().any(|f| *w == f.name))
        .collect();
    assert!(
        unknown.is_empty(),
        "GATE_FIELD names unknown field(s): {}; known: {}",
        unknown.join(", "),
        FIELDS.iter().map(|f| f.name).collect::<Vec<_>>().join(", ")
    );
    FIELDS.iter().filter(|f| want.contains(&f.name)).collect()
}

/// Runs one field and records its checks. With `hint`, the field position is
/// passed as `--ra`/`--dec` at the truth center.
fn run_field(
    f: &FieldTruth,
    indices: &Indices,
    hint: bool,
    checks: &mut usize,
    failures: &mut Vec<String>,
) {
    let mut check = |label: &str, ok: bool, detail: String| {
        *checks += 1;
        if !ok {
            failures.push(format!("{} {label}: {detail}", f.name));
        }
    };

    // The subprocess keeps the gate external: the real stdout bytes and the
    // real exit code are what the product promises. Positional .xyls first:
    // with `--ra -4 -4 -60` style tokens a trailing positional could otherwise
    // be taken as an option value.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_starsolver"));
    cmd.arg(common::repo_root().join(f.input));
    if let Indices::Pin(dir) = indices {
        cmd.args(["--index-dir", &dir.to_string_lossy()]);
    }
    let width = format!("{:.0}", f.width_pix);
    let height = format!("{:.0}", f.height_pix);
    cmd.args(["-w", &width, "-e", &height]);
    let (ra, dec) = (format!("{:.6}", f.ra_deg), format!("{:.6}", f.dec_deg));
    if hint {
        cmd.args(["--ra", &ra, "--dec", &dec]);
    }

    let started = Instant::now();
    let proc = cmd.output().expect("the binary runs");
    let elapsed = started.elapsed().as_secs_f64();
    let factor = if cfg!(debug_assertions) {
        DEBUG_TIME_FACTOR
    } else {
        1.0
    };
    let budget = f.timeout_s * factor;

    let text = String::from_utf8_lossy(&proc.stdout);
    match common::parse_solution(&text) {
        None => {
            let why = if proc.status.success() {
                "exit 0 but the success block is incomplete".to_string()
            } else {
                // Char-safe tail: the stderr may quote non-ASCII paths.
                let stderr = String::from_utf8_lossy(&proc.stderr);
                let reversed: String = stderr
                    .trim()
                    .chars()
                    .rev()
                    .take(STDERR_TAIL_CHARS)
                    .collect();
                let tail: String = reversed.chars().rev().collect();
                format!(
                    "not solved (exit {:?}); stderr tail: {tail}",
                    proc.status.code()
                )
            };
            check("solved", false, why);
        }
        Some(p) => {
            check(
                "exit code",
                proc.status.success(),
                format!("exit {:?}", proc.status.code()),
            );
            let tol = center_tol(f);
            let dra = common::normalize_deg(p.ra - f.ra_deg)
                * f64::cos(f64::to_radians(p.dec))
                * ARCSEC_PER_DEG;
            let ddec = (p.dec - f.dec_deg) * ARCSEC_PER_DEG;
            check(
                "dRA*cos(dec)",
                f64::abs(dra) <= tol,
                format!("{dra:+.4}\" over the {tol:.3}\" budget"),
            );
            check(
                "ddec",
                f64::abs(ddec) <= tol,
                format!("{ddec:+.4}\" over the {tol:.3}\" budget"),
            );
            // Our stdout has no scale line; derive it from the width.
            let scale_got = p.width_deg * ARCSEC_PER_DEG / f.width_pix;
            check(
                "scale",
                common::rel_frac(scale_got, f.scale_arcsec) <= TOL_SCALE_FRAC,
                format!("{scale_got:.5} vs {:.5}\"/px", f.scale_arcsec),
            );
            let dorient = common::normalize_deg(p.orient_deg - f.orient_deg);
            check(
                "orient",
                f64::abs(dorient) <= TOL_ORIENT_DEG,
                format!("{dorient:+.4} deg over the {TOL_ORIENT_DEG} deg budget"),
            );
            check(
                "width_deg",
                common::rel_frac(p.width_deg, f.width_deg) <= TOL_SIZE_FRAC,
                format!("{} vs {}", p.width_deg, f.width_deg),
            );
            check(
                "height_deg",
                common::rel_frac(p.height_deg, f.height_deg) <= TOL_SIZE_FRAC,
                format!("{} vs {}", p.height_deg, f.height_deg),
            );
            let parity = if f.parity_neg { "neg" } else { "pos" };
            check(
                "parity",
                (p.parity == "neg") == f.parity_neg,
                format!("{} vs {}", p.parity, parity),
            );
        }
    }
    check(
        "time",
        elapsed < budget,
        format!("{elapsed:.2} s over the {budget:.0} s budget"),
    );
}

fn gate(hint: bool) {
    let fields = selected();

    // Data gate: every input .xyls plus a usable index set.
    for f in &fields {
        // `skip` panics under ASTROMETRY_REQUIRE_TEST_DATA, returns true
        // otherwise, so the whole test is abandoned only on a real skip.
        if !common::repo_root().join(f.input).is_file()
            && common::skip(&format!("{} is absent", f.input))
        {
            return;
        }
    }
    let Some(indices) = common::indices() else {
        return;
    };

    let mut checks = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for f in &fields {
        run_field(f, &indices, hint, &mut checks, &mut failures);
    }

    if !failures.is_empty() {
        panic!(
            "{}/{} ground-truth checks failed:\n{}",
            failures.len(),
            checks,
            failures.join("\n")
        );
    }
    println!(
        "{checks} {} ground-truth checks within tolerance",
        if hint { "hinted" } else { "blind" }
    );
}

#[test]
fn ground_truth_fields_stay_within_tolerance() {
    gate(false);
}

#[test]
fn ground_truth_fields_stay_within_tolerance_with_position_hint() {
    gate(true);
}
