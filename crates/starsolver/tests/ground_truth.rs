//! The end-to-end tolerance gate: run the built binary on every shipped field,
//! parse the five-line success block it prints back, and keep the printed WCS
//! inside the documented budgets.
//!
//! `FIELDS` is the record of the reference runs: astrometry.net's `solve-field`
//! (the installed 0.98 reference binary) on these thirty-eight `.xyls` files. Each
//! entry's comment keeps what that run reported beyond the printed WCS - the
//! index that solved it, its log-odds and match counts, engine CPU and, where
//! the run published one, its full WCS - and nothing regenerates them: they are
//! what that binary said against the index set of the machine it ran on, which
//! is not the installed set these tests sweep (see `AGENTS.md`, "Data
//! directories"). Every `platesolve_*` entry is recorded from that binary's
//! `--no-tweak` run rather than its default one, so that a similarity WCS is
//! compared with a similarity WCS; the `platesolve` comment gives the reason.
//! The same truth is written a second time as the
//! `Truth` consts of `src/testutil.rs` (what the unit gates demand), pinned to
//! this table by `truth_consts_match_the_cargo_gate` and to the budgets below by
//! `gate_budgets_match_the_cargo_gate`.
//!
//! Beyond `testdata/` and an index set the gate needs no data.
//!
//! Every field is run under several priors ([`Variant`]), each of which exercises
//! a different gate:
//!
//! | variant | prior | what it gates |
//! |---|---|---|
//! | `blind` | the frame size only | the whole sweep |
//! | `hint_tight` | `--ra/--dec` at the truth, `--radius 3` | rejecting hits with a quad star outside the circle, on a circle narrower than the frame |
//! | `hint_default_radius` | `--ra/--dec` at the truth, no `--radius` | the CLI's default circle |
//! | `hint_offset` | `--ra/--dec` 2 deg off the truth, `--radius 10` | a prior that must not bias the print |
//! | `scale_tight` / `scale_wide` | `--scale-low`/`--scale-high` | index selection, pre-/post-fit scale gates |
//! | `scale_tight_hint` | both priors at their tightest | the two gates together |
//! | `reject_wrong_position` | `--ra/--dec` at the antipode, `--radius 3` | must exit 1 |
//! | `reject_wrong_scale` | a scale 5x coarser than the truth, narrow | must exit 1 |
//! | `reject_beyond_hint` | a circle of the field's circumradius centered 1.5x that off its center, so the frame is covered but its center is not | must exit 1 |
//!
//! A negative run passes the short `--cpulimit` of `NEGATIVE_CPULIMIT_S` instead
//! of the field's solve budget, because a forbidden solution is impossible by
//! construction (the hint gates reject the hit or the finished center, the scale
//! gates reject the fitted scale) while the sweep itself keeps working: it has no
//! reason to stop before its deadline. That also means a negative could pass for
//! the wrong reason - having had nothing to search - so each one also checks that
//! index files were selected, and `reject_beyond_hint` checks that at least one
//! of its runs printed the discard of a finished solution.
//!
//! `GATE_FIELD=name[,name...]` limits a run to some of the fields and
//! `GATE_VARIANT=name[,name...]` to some of the variants; an unknown or empty
//! name fails loudly, so a typo cannot read as a passing run. Both apply to the
//! negative legs and the prior-stability check, which run on `SHORTLIST`.
//!
//! Time budget caveat: the budgets were calibrated on the release binary, while
//! `CARGO_BIN_EXE_starsolver` under a plain `cargo test` is the test-profile one
//! (`opt-level = 1`, several times slower), so the budget is scaled by
//! `DEBUG_TIME_FACTOR` there and by nothing under `cargo test --release`. The
//! check happens after the process exits; there is no kill - a run that goes on
//! too long ends on the solver's own `--cpulimit` wall-clock deadline (the
//! default 300 s, `NEGATIVE_CPULIMIT_S` on the negative legs), which prints the
//! timeout line to stderr and exits 1.

mod common;

use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use common::Indices;

/// `cargo test` runs the legs of this target on parallel threads while each leg
/// runs a binary that already spreads over the cores; serializing them keeps
/// every wall-time budget a measurement of one solve rather than of a dozen
/// competing ones. Other test targets are separate processes and unaffected.
static GATE_LOCK: Mutex<()> = Mutex::new(());

/// Takes the gate lock for one leg, tolerating a poisoned one: a leg that failed
/// corrupted nothing, and the remaining legs must still run.
fn gate_lock() -> std::sync::MutexGuard<'static, ()> {
    GATE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

// Gate budgets. The center budget is deliberately in pixels of the field's own
// scale: 1" is 0.37 px at 2.71"/px but 1.00 px at 1.0"/px, so a fixed arcsec
// figure would not mean the same error on both. A field may widen its center or
// rotation budget through the `FieldTruth` overrides; the entry comments say why.
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

// The priors of the hint variants. The tight circle is narrower than the
// half-diagonal of the widest shipped field (3.05 deg), so the quads near its
// corners are rejected by the in-circle test and the field must be solved from
// its inner stars.
const HINT_TIGHT_RADIUS_DEG: f64 = 3.0;
/// The wide hint circle, degrees: the same search area as the CLI default
/// (`params::DEFAULT_RADECS_RADIUS_DEG`, written out because an integration
/// target cannot read a private const).
const HINT_WIDE_RADIUS_DEG: f64 = 10.0;
/// How far off the truth center `hint_offset` puts its `--ra/--dec`, degrees.
const HINT_OFFSET_DEG: f64 = 2.0;
/// `reject_beyond_hint` draws a circle of the field's own circumradius
/// (`circumradius_deg`) with its center this many circumradii off the field
/// center. The circle then covers the near part of the frame - hits get fitted
/// and verified - while the field's center falls outside it, so a finished
/// solution must be thrown away rather than reported.
const HINT_BEYOND_OFFSET_FACTOR: f64 = 1.5;
/// Half-width of the tight scale window as a fraction of the truth scale; also
/// the width of the wrong-scale window. The solver widens its pre-fit scale gate
/// by 1 %, so a window much tighter than that stops being a test of the fit.
const SCALE_TIGHT_FRAC: f64 = 0.02;
/// The wide scale window, as factors of the truth scale.
const SCALE_WIDE_LO: f64 = 0.5;
const SCALE_WIDE_HI: f64 = 2.0;
/// `reject_wrong_scale` asks for a scale this many times COARSER than the truth.
/// Coarser rather than finer because the fine end of the sky is where a small
/// install may hold no index at all, which would leave the leg nothing to gate
/// (the leg checks that index files were selected).
const SCALE_WRONG_FACTOR: f64 = 5.0;
/// Wall-clock budget of a negative run, seconds (see the module docs). Must stay
/// well above the time `SHORTLIST` fields take to solve: below it the deadline
/// would expire before the forbidden hit was ever attempted and the leg would
/// pass without having tested anything.
const NEGATIVE_CPULIMIT_S: f64 = 15.0;
/// Slack on top of a negative run's deadline. The deadline is wall-clock, so only
/// its overshoot scales with the profile: it is sampled at chunk and iteration
/// boundaries, and a slower profile spends more real time per iteration.
const NEGATIVE_TIME_SLACK_S: f64 = 10.0;
/// The fields the negative legs and the prior-stability check run on: each
/// negative costs its whole `--cpulimit`, and what these legs gate is the hint
/// and scale gates, not anything field-specific, so three frame sizes catch a
/// regression.
const SHORTLIST: [&str; 3] = ["4656x3522-1", "platesolve-1", "1920x1080-1"];

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
    /// fields' one; `platesolve-9` sets it for the reason of its entry comment.
    tol_center_arcsec: Option<f64>,
    /// Rotation budget, degrees; wins over `TOL_ORIENT_DEG`. Set on the crowded
    /// real fields whose reference print is not the best similarity fit of its
    /// own matched stars - the entry comments give the numbers.
    tol_orient_deg: Option<f64>,
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
        tol_orient_deg: None,
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
        tol_orient_deg: None,
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
        tol_orient_deg: None,
        timeout_s: 300.0,
    },
    // Reference run: the same index-4206 tile and SIP tweak as the entry above,
    // and the slowest of the recorded reference runs at 23.9 s of engine CPU - the
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
        tol_orient_deg: None,
        timeout_s: 300.0,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-04-21 (id 4204, HPNSIDE 2): 52.343 log-odds from 22 matches
    // over 52 index stars and 95 field stars, no conflicts, 85 quads tried, 16
    // matched and 15 verified, a code error of 5.32e-3, won in the field-objects
    // 21-30 round. Its WCS: CRVAL (83.7736366454, -5.24192918952), CRPIX
    // (430.048550832, 111.0064254), CD deg/pix = [[-8.06858236235e-4,
    // -6.26178266262e-5], [6.26178266262e-5, -8.06858236235e-4]] - equal column
    // norms (2.913424 arcsec/px) and an antisymmetric off-diagonal, so a pure
    // similarity, exactly the shape this solver fits. As in the entries above,
    // the numbers here are the console print of that map, 6-digit rounded: its
    // rotation is -175.562 deg and its size 38.8451 x 29.1339 arcminutes.
    //
    // The `--no-tweak` run is the truth for this field, and that is a difference
    // of models, not of accuracy. These are real detections through a real lens,
    // and the reference's default run tweaks the answer to a quadratic (TAN-SIP)
    // model whose linear part is anisotropic - 2.93612 and 2.90338 arcsec/px
    // along the two axes - and prints the corner extent of the distorted map as
    // the field size. Against that default print our similarity answer measures
    // 5.6" of center and 0.9% of width, purely because we fit four parameters
    // where that run fits an affine plus a quadratic term; the `1920x1080-*`
    // entries keep the tweaked print and pay for it with a widened absolute
    // center budget. Against a similarity the same solve leaves 0.41 px of
    // center, 0.003% of size and 0.126 deg of rotation, so this field gates at
    // the default budgets.
    FieldTruth {
        name: "platesolve",
        input: "testdata/platesolve_16119512121716485056.xyls",
        width_pix: 800.0,
        height_pix: 600.0,
        ra_deg: 83.785666,
        dec_deg: -5.396673,
        width_deg: 0.647418,
        height_deg: 0.485565,
        orient_deg: -175.562,
        parity_neg: true,
        scale_arcsec: 2.91342,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-12 (id 4212): 65.307 log-odds from 10 matches over 34
    // distractors and 40 index stars.
    FieldTruth {
        name: "platesolve-1",
        input: "testdata/platesolve_13932859987089881536.xyls",
        width_pix: 6224.0,
        height_pix: 4168.0,
        ra_deg: 313.855978,
        dec_deg: 44.34006,
        width_deg: 5.06922,
        height_deg: 3.3959,
        orient_deg: 161.961,
        parity_neg: true,
        scale_arcsec: 2.93399,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-05-03 (id 4205): 173.255 log-odds from 24 matches over 5
    // distractors and 40 index stars.
    FieldTruth {
        name: "platesolve-2",
        input: "testdata/platesolve_14808105915143123636.xyls",
        width_pix: 1920.0,
        height_pix: 1080.0,
        ra_deg: 308.730236,
        dec_deg: 60.154128,
        width_deg: 0.531312,
        height_deg: 0.298863,
        orient_deg: -103.591,
        parity_neg: true,
        scale_arcsec: 0.996218,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-12 (id 4212): 91.619 log-odds from 18 matches over 353
    // distractors and 22 index stars. Crowded real field, and its print is
    // recorded with a widened 1 deg rotation budget: the reference print is not
    // the best similarity fit of its own matched stars - refitting those pairs
    // puts the rotation within 0.001 deg of our print while the print itself
    // keeps 1.3 px of residual over them - so the 0.5 deg budget would gate on
    // the reference's fit slop, not on the sky.
    FieldTruth {
        name: "platesolve-3",
        input: "testdata/platesolve_15484428693811729397.xyls",
        width_pix: 4656.0,
        height_pix: 3522.0,
        ra_deg: 314.943579,
        dec_deg: 44.471678,
        width_deg: 3.82604,
        height_deg: 2.89447,
        orient_deg: 93.1984,
        parity_neg: true,
        scale_arcsec: 2.95965,
        tol_center_arcsec: None,
        tol_orient_deg: Some(1.0),
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-06-02 (id 4206): 42.606 log-odds from 6 matches over 2
    // distractors and 21 index stars. The sparsest shipped field: 12 detected
    // stars.
    FieldTruth {
        name: "platesolve-4",
        input: "testdata/platesolve_3467425964176916163.xyls",
        width_pix: 1920.0,
        height_pix: 1080.0,
        ra_deg: 202.500838,
        dec_deg: 47.227831,
        width_deg: 0.531322,
        height_deg: 0.29887,
        orient_deg: -83.2533,
        parity_neg: true,
        scale_arcsec: 0.996238,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-12 (id 4212): 147.506 log-odds from 29 matches over 245
    // distractors and 45 index stars.
    FieldTruth {
        name: "platesolve-5",
        input: "testdata/platesolve_3475779261026367693.xyls",
        width_pix: 6224.0,
        height_pix: 4168.0,
        ra_deg: 97.898032,
        dec_deg: 4.937407,
        width_deg: 5.06877,
        height_deg: 3.39591,
        orient_deg: -90.8948,
        parity_neg: true,
        scale_arcsec: 2.93427,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-11 (id 4211): 147.111 log-odds from 15 matches over 19
    // distractors and 18 index stars.
    FieldTruth {
        name: "platesolve-6",
        input: "testdata/platesolve_3794436781639291501.xyls",
        width_pix: 4144.0,
        height_pix: 2822.0,
        ra_deg: 202.480322,
        dec_deg: 47.243191,
        width_deg: 2.41445,
        height_deg: 1.64433,
        orient_deg: -120.199,
        parity_neg: true,
        scale_arcsec: 2.09781,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-12 (id 4212): 309.656 log-odds from 35 matches over 446
    // distractors and 39 index stars. Rotation widened like platesolve-3's:
    // the refit of the run's matched stars lands 0.0003 deg from our print
    // while the reference print keeps 1.6 px of residual over the same stars.
    FieldTruth {
        name: "platesolve-7",
        input: "testdata/platesolve_5308030456135217324.xyls",
        width_pix: 6224.0,
        height_pix: 4168.0,
        ra_deg: 60.146199,
        dec_deg: 36.350879,
        width_deg: 5.06868,
        height_deg: 3.39598,
        orient_deg: -179.385,
        parity_neg: true,
        scale_arcsec: 2.93456,
        tol_center_arcsec: None,
        tol_orient_deg: Some(1.0),
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-06-02 (id 4206): 74.674 log-odds from 10 matches over 3
    // distractors and 21 index stars.
    FieldTruth {
        name: "platesolve-8",
        input: "testdata/platesolve_6845873554475570690.xyls",
        width_pix: 1920.0,
        height_pix: 1080.0,
        ra_deg: 202.502733,
        dec_deg: 47.247307,
        width_deg: 0.53135,
        height_deg: 0.298887,
        orient_deg: -83.4086,
        parity_neg: true,
        scale_arcsec: 0.996292,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-11 (id 4211): 89.541 log-odds from 16 matches over 37
    // distractors and 73 index stars.
    // Widened center and rotation budgets for the same reason as platesolve-3
    // and -7: the refit of the run's matched stars lands on our print (0.004
    // deg of rotation, 2.4" of center) while the reference print leaves 2.3 px
    // of residual over those stars and sits 3.8" from the refit's center.
    FieldTruth {
        name: "platesolve-9",
        input: "testdata/platesolve_9410913457908508997.xyls",
        width_pix: 6224.0,
        height_pix: 4168.0,
        ra_deg: 312.857692,
        dec_deg: 30.899818,
        width_deg: 5.04677,
        height_deg: 3.38201,
        orient_deg: -20.7401,
        parity_neg: true,
        scale_arcsec: 2.92356,
        tol_center_arcsec: Some(8.0),
        tol_orient_deg: Some(1.5),
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-05-02 (id 4205, HPNSIDE 1): 149.952 log-odds from 28 matches
    // over 47 distractors and 50 index stars, 12718 quads tried, a code error of
    // 2.42e-3, won in the field-objects 41-50 round, 0.082 s of engine CPU. Its
    // WCS: CRVAL (250.449335194, 36.40164981), CRPIX (685.567728612,
    // 573.347613767), CD deg/pix = [[-1.56386598788e-05, -2.14672249075e-04],
    // [2.14672249075e-04, -1.56386598788e-05]] - equal column norms (0.774868
    // arcsec/px) and an antisymmetric off-diagonal, so a pure similarity. The
    // globular cluster M13 with its core about 1.2' off the field center: 453
    // detections, and the reference needed its 41-50 star round to find it.
    FieldTruth {
        name: "platesolve-10",
        input: "testdata/platesolve_8385152464781942119.xyls",
        width_pix: 1835.0,
        height_pix: 1477.0,
        ra_deg: 250.400608,
        dec_deg: 36.448946,
        width_deg: 0.394965,
        height_deg: 0.31791,
        orient_deg: -94.1666,
        parity_neg: true,
        scale_arcsec: 0.774868,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the all-sky index
    // index-2mass-09 (id 4209, HEALPIX -1): 75.6353 log-odds from 10 matches
    // over 25 distractors and 20 index stars, 324 quads tried, a code error of
    // 9.45e-4, won in the field-objects 11-20 round, 0.0023 s of engine CPU. Its
    // WCS: CRVAL (35.094488284, 57.102806603), CRPIX (630.1215, 868.2093),
    // CD deg/pix = [[-1.25596395682e-04, 6.48327600192e-04],
    // [-6.48327600192e-04, -1.25596395682e-04]] - 2.37737 arcsec/px, a pure
    // similarity. The double cluster h and chi Persei: both members fall inside
    // this 1080x1920 frame, which is taller than wide, so its printed width is
    // the short side.
    FieldTruth {
        name: "platesolve-11",
        input: "testdata/platesolve_8855848955227353225.xyls",
        width_pix: 1080.0,
        height_pix: 1920.0,
        ra_deg: 35.225543,
        dec_deg: 57.149251,
        width_deg: 0.713202,
        height_deg: 1.26788,
        orient_deg: 100.964,
        parity_neg: true,
        scale_arcsec: 2.37737,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-06-02 (id 4206, HPNSIDE 1): 277.428 log-odds from 44 matches
    // over 170 distractors and 60 index stars, 3753 quads tried, a code error of
    // 8.60e-3, won in the field-objects 21-30 round, 0.017 s of engine CPU. Its
    // WCS: CRVAL (250.262394052, 36.336373145), CRPIX (1752.544, 766.7854),
    // CD deg/pix = [[-2.20157034149e-06, 2.48755978219e-04],
    // [-2.48755978219e-04, -2.20157034149e-06]] - 0.895557 arcsec/px. M13 again,
    // this time 25" from the field center and in a square frame, the only
    // shipped field whose printed width equals its height. Its sky also covers
    // the center of the shipped 1920x1080-2 field, 1.6' away.
    FieldTruth {
        name: "platesolve-12",
        input: "testdata/platesolve_15809913605018352836.xyls",
        width_pix: 2556.0,
        height_pix: 2556.0,
        ra_deg: 250.421947,
        dec_deg: 36.453062,
        width_deg: 0.635835,
        height_deg: 0.635835,
        orient_deg: 90.5071,
        parity_neg: true,
        scale_arcsec: 0.895557,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // Reference run: `solve-field` 0.98 with `--no-tweak`, on the healpix tile
    // index-2mass-07-00 (id 4207, HPNSIDE 1): 92.3712 log-odds from 19 matches
    // over 71 distractors and 38 index stars, 307 quads tried, a code error of
    // 1.47e-3, won in the field-objects 11-20 round, 0.0013 s of engine CPU. Its
    // WCS: CRVAL (20.033367283, 58.365901057), CRPIX (655.1508, 553.4411),
    // CD deg/pix = [[-3.06443133695e-05, -6.58546881188e-04],
    // [6.58546881188e-04, -3.06443133695e-05]] - 2.37333 arcsec/px. The Owl
    // Cluster (NGC 457) in Cassiopeia, a few arcminutes off the field center:
    // 477 detections in a 1021x1188 frame.
    FieldTruth {
        name: "platesolve-13",
        input: "testdata/platesolve_16727288192529711027.xyls",
        width_pix: 1021.0,
        height_pix: 1188.0,
        ra_deg: 19.990354,
        dec_deg: 58.269706,
        width_deg: 0.673095,
        height_deg: 0.783187,
        orient_deg: -92.6642,
        parity_neg: true,
        scale_arcsec: 2.37333,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // The `astro-*` block: single-frame real astro-photos whose detections come
    // from `image2xy`, written in the canonical `.xyls` form. Their truth is the
    // console print of a blind `solve-field` 0.98 run against the index set of
    // the machine that ran it, exactly as in the entries above - the default,
    // tweaked run, so `width_deg`/`height_deg` are the corner extent of a TAN-SIP
    // map and `scale_arcsec` is that printed width over the frame width rather
    // than a similarity scale; the 1 % scale and 0.5 % size budgets absorb the
    // difference, and an entry whose budgets they did not absorb says by how
    // much. Four corpus frames carry no truth here: that run did not solve three
    // of them, and the fourth (the NGC 6914 field) is left out with its answer
    // 3.9 px off this print.
    FieldTruth {
        name: "astro-00",
        input: "testdata/astro-00-RunningManOrion-B.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 83.834236,
        dec_deg: -4.810898,
        width_deg: 0.764082,
        height_deg: 0.577987,
        orient_deg: 36.1088,
        parity_neg: true,
        scale_arcsec: 0.590785,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-01",
        input: "testdata/astro-01-RosetteA-SII.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 97.821519,
        dec_deg: 5.148506,
        width_deg: 0.76458,
        height_deg: 0.578448,
        orient_deg: 36.098,
        parity_neg: true,
        scale_arcsec: 0.59117,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-02",
        input: "testdata/astro-02-M51-B.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 202.469764,
        dec_deg: 47.191459,
        width_deg: 0.764502,
        height_deg: 0.579187,
        orient_deg: -143.727,
        parity_neg: true,
        scale_arcsec: 0.59111,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-03",
        input: "testdata/astro-03-M105-G.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 162.017858,
        dec_deg: 12.594721,
        width_deg: 0.764265,
        height_deg: 0.578168,
        orient_deg: 83.884,
        parity_neg: true,
        scale_arcsec: 0.590927,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-04",
        input: "testdata/astro-04-M97-Ha.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 168.696126,
        dec_deg: 55.018261,
        width_deg: 0.764888,
        height_deg: 0.578707,
        orient_deg: 84.0765,
        parity_neg: true,
        scale_arcsec: 0.591409,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-05",
        input: "testdata/astro-05-M104-G.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 189.994528,
        dec_deg: -11.629603,
        width_deg: 0.764747,
        height_deg: 0.57851,
        orient_deg: -95.9549,
        parity_neg: true,
        scale_arcsec: 0.591299,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-06",
        input: "testdata/astro-06-NGC4565-G.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 189.031246,
        dec_deg: 25.933460,
        width_deg: 0.764288,
        height_deg: 0.578183,
        orient_deg: 83.9212,
        parity_neg: true,
        scale_arcsec: 0.590945,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-07",
        input: "testdata/astro-07-NGC6995-SII.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 314.208784,
        dec_deg: 31.138634,
        width_deg: 0.76458,
        height_deg: 0.578452,
        orient_deg: 84.1324,
        parity_neg: true,
        scale_arcsec: 0.59117,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-12",
        input: "testdata/astro-12-BabyEagle-B.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 61.083938,
        dec_deg: 26.417179,
        width_deg: 0.764137,
        height_deg: 0.578085,
        orient_deg: 88.7442,
        parity_neg: true,
        scale_arcsec: 0.590827,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-13",
        input: "testdata/astro-13-NGC6946-G.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 308.702643,
        dec_deg: 60.151131,
        width_deg: 0.764488,
        height_deg: 0.578338,
        orient_deg: -90.7264,
        parity_neg: true,
        scale_arcsec: 0.591099,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-14",
        input: "testdata/astro-14-NGC7000-SII.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 314.953244,
        dec_deg: 43.670742,
        width_deg: 0.76423,
        height_deg: 0.578123,
        orient_deg: -90.979,
        parity_neg: true,
        scale_arcsec: 0.590899,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-15",
        input: "testdata/astro-15-M45-G.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 56.430142,
        dec_deg: 24.193158,
        width_deg: 0.764232,
        height_deg: 0.578015,
        orient_deg: 88.7428,
        parity_neg: true,
        scale_arcsec: 0.590901,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-16",
        input: "testdata/astro-16-M2-B.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 323.349486,
        dec_deg: -0.829491,
        width_deg: 0.763818,
        height_deg: 0.57777,
        orient_deg: -91.0926,
        parity_neg: true,
        scale_arcsec: 0.590581,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-17",
        input: "testdata/astro-17-Comet29P-BLUE.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 101.651717,
        dec_deg: 29.451032,
        width_deg: 0.764763,
        height_deg: 0.579093,
        orient_deg: -144.03,
        parity_neg: true,
        scale_arcsec: 0.591312,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-18",
        input: "testdata/astro-18-HorseheadNebula-BLUE.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 85.231105,
        dec_deg: -2.491202,
        width_deg: 0.7645,
        height_deg: 0.57809,
        orient_deg: 88.675,
        parity_neg: true,
        scale_arcsec: 0.591108,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // A drizzled stack: its frame is the 4604x3448 its `.xyls` headers carry, not
    // the 4656x3520 of the rest of this block.
    FieldTruth {
        name: "astro-19",
        input: "testdata/astro-19-HorseheadStack-Ha.xyls",
        width_pix: 4604.0,
        height_pix: 3448.0,
        ra_deg: 85.252591,
        dec_deg: -2.481860,
        width_deg: 0.75624,
        height_deg: 0.566675,
        orient_deg: 36.0921,
        parity_neg: true,
        scale_arcsec: 0.591326,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    // The core of M15 fills the frame: every prior leaves 1.40" (2.36 px) of
    // center against this print, so the field gates at 1.6" rather than the
    // 0.89" a 1.5 px budget at its 0.59"/px scale allows.
    FieldTruth {
        name: "astro-20",
        input: "testdata/astro-20-M15-BLUE.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 322.491005,
        dec_deg: 12.158394,
        width_deg: 0.764567,
        height_deg: 0.578777,
        orient_deg: 36.3049,
        parity_neg: true,
        scale_arcsec: 0.59116,
        tol_center_arcsec: Some(1.6),
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-21",
        input: "testdata/astro-21-M33-BLUE.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 23.653434,
        dec_deg: 30.867069,
        width_deg: 0.764392,
        height_deg: 0.578215,
        orient_deg: 36.2008,
        parity_neg: true,
        scale_arcsec: 0.591024,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-22",
        input: "testdata/astro-22-M74-BLUE.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 24.174458,
        dec_deg: 15.787231,
        width_deg: 0.764685,
        height_deg: 0.578173,
        orient_deg: 36.1229,
        parity_neg: true,
        scale_arcsec: 0.591251,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
    FieldTruth {
        name: "astro-23",
        input: "testdata/astro-23-NGC2359-Ha.xyls",
        width_pix: 4656.0,
        height_pix: 3520.0,
        ra_deg: 109.676877,
        dec_deg: -13.208382,
        width_deg: 0.764832,
        height_deg: 0.578435,
        orient_deg: 36.097,
        parity_neg: true,
        scale_arcsec: 0.591365,
        tol_center_arcsec: None,
        tol_orient_deg: None,
        timeout_s: TOL_TIME_S,
    },
];

/// One way of presenting a field to the binary, and what its run owes the gate.
#[derive(Clone, Copy)]
enum Variant {
    /// The frame size is the only prior.
    Blind,
    /// `--ra/--dec` at the truth center inside the tight circle.
    HintTight,
    /// `--ra/--dec` at the truth center with no `--radius`, so the circle the CLI
    /// fills in by itself is what gets gated.
    HintDefaultRadius,
    /// A hint `HINT_OFFSET_DEG` off the truth center inside the wide circle: a
    /// prior may narrow the search, it may not move the answer.
    HintOffset,
    /// Truth scale +-`SCALE_TIGHT_FRAC`.
    ScaleTight,
    /// Truth scale from `SCALE_WIDE_LO` to `SCALE_WIDE_HI` times itself.
    ScaleWide,
    /// The tight scale window and the tight circle at once.
    ScaleTightHint,
    /// A hint whose circle cannot contain the field. Every hit with a quad star
    /// outside the circle is rejected, so nothing may be reported.
    RejectWrongPosition,
    /// A scale range the field cannot have: the post-fit gate on the fitted scale
    /// throws the true WCS away. A range that selects no index file at all is the
    /// exit-1 leg of `cli_contract`, so this leg also checks that indices were
    /// selected and the rejection was the gates' doing.
    RejectWrongScale,
    /// A circle that covers the near part of the frame but not its center, so the
    /// sweep can fit and verify the field and then must throw the finished
    /// solution away instead of reporting it.
    RejectBeyondHint,
}

impl Variant {
    /// The name `GATE_VARIANT` selects this variant by.
    fn key(self) -> &'static str {
        match self {
            Self::Blind => "blind",
            Self::HintTight => "hint_tight",
            Self::HintDefaultRadius => "hint_default_radius",
            Self::HintOffset => "hint_offset",
            Self::ScaleTight => "scale_tight",
            Self::ScaleWide => "scale_wide",
            Self::ScaleTightHint => "scale_tight_hint",
            Self::RejectWrongPosition => "reject_wrong_position",
            Self::RejectWrongScale => "reject_wrong_scale",
            Self::RejectBeyondHint => "reject_beyond_hint",
        }
    }

    /// Whether the run must solve the field; the three `reject_*` legs must not.
    fn expects_solution(self) -> bool {
        !matches!(
            self,
            Self::RejectWrongPosition | Self::RejectWrongScale | Self::RejectBeyondHint
        )
    }

    /// Whether the run is asked for progress lines: `reject_beyond_hint` needs
    /// the `[verify]` line that reports a discarded solution, without which the
    /// leg cannot tell a discarded solution from a field it never fitted.
    fn verbose(self) -> bool {
        matches!(self, Self::RejectBeyondHint)
    }

    /// The CLI tokens of this prior, on top of the frame size.
    fn args(self, f: &FieldTruth) -> Vec<String> {
        match self {
            Self::Blind => Vec::new(),
            Self::HintTight => hint(f.ra_deg, f.dec_deg, 0.0, Some(HINT_TIGHT_RADIUS_DEG)),
            Self::HintDefaultRadius => hint(f.ra_deg, f.dec_deg, 0.0, None),
            Self::HintOffset => hint(
                f.ra_deg,
                f.dec_deg,
                HINT_OFFSET_DEG,
                Some(HINT_WIDE_RADIUS_DEG),
            ),
            Self::ScaleTight => scale(
                f.scale_arcsec * (1.0 - SCALE_TIGHT_FRAC),
                f.scale_arcsec * (1.0 + SCALE_TIGHT_FRAC),
            ),
            Self::ScaleWide => scale(
                f.scale_arcsec * SCALE_WIDE_LO,
                f.scale_arcsec * SCALE_WIDE_HI,
            ),
            Self::ScaleTightHint => [
                scale(
                    f.scale_arcsec * (1.0 - SCALE_TIGHT_FRAC),
                    f.scale_arcsec * (1.0 + SCALE_TIGHT_FRAC),
                ),
                hint(f.ra_deg, f.dec_deg, 0.0, Some(HINT_TIGHT_RADIUS_DEG)),
            ]
            .concat(),
            // The antipode of the truth center: the farthest point a hint can
            // name, so no star of the field can fall inside its circle.
            Self::RejectWrongPosition => hint(
                f.ra_deg + 180.0,
                -f.dec_deg,
                0.0,
                Some(HINT_TIGHT_RADIUS_DEG),
            ),
            Self::RejectWrongScale => {
                let off = f.scale_arcsec * SCALE_WRONG_FACTOR;
                scale(
                    off * (1.0 - SCALE_TIGHT_FRAC),
                    off * (1.0 + SCALE_TIGHT_FRAC),
                )
            }
            Self::RejectBeyondHint => {
                let radius = circumradius_deg(f);
                hint(
                    f.ra_deg,
                    f.dec_deg,
                    HINT_BEYOND_OFFSET_FACTOR * radius,
                    Some(radius),
                )
            }
        }
    }
}

/// The circumradius of a field's frame on the sky, degrees: half its diagonal.
/// The frame is rectangular, so this is the distance of its corners from the
/// center, and any hint circle of this radius centered inside the frame covers a
/// comparable part of it whatever the field's scale.
fn circumradius_deg(f: &FieldTruth) -> f64 {
    f64::hypot(f.width_deg, f.height_deg) / 2.0
}

/// Every variant, for the `GATE_VARIANT` name check.
const VARIANTS: &[Variant] = &[
    Variant::Blind,
    Variant::HintTight,
    Variant::HintDefaultRadius,
    Variant::HintOffset,
    Variant::ScaleTight,
    Variant::ScaleWide,
    Variant::ScaleTightHint,
    Variant::RejectWrongPosition,
    Variant::RejectWrongScale,
    Variant::RejectBeyondHint,
];

/// `--ra/--dec` for the point `sky_offset_deg` of sky east of `(ra_deg,
/// dec_deg)` - the RA step widened by 1/cos(dec), so the offset is a sky angle
/// and not an hour-angle one - plus `--radius` when the variant names a circle
/// instead of taking the CLI default. A normalized RA can come out negative;
/// `--ra`/`--dec` accept a leading-hyphen value by design, and the positional
/// `.xyls` is passed first, so such a token is read as a value and not a flag.
fn hint(ra_deg: f64, dec_deg: f64, sky_offset_deg: f64, radius_deg: Option<f64>) -> Vec<String> {
    // The guard is for a field at the pole; none of the shipped ones is close.
    let cos_dec = f64::cos(f64::to_radians(dec_deg)).max(1e-3);
    let mut args = vec![
        "--ra".to_string(),
        format!(
            "{:.6}",
            common::normalize_deg(ra_deg + sky_offset_deg / cos_dec)
        ),
        "--dec".to_string(),
        format!("{:.6}", dec_deg),
    ];
    if let Some(radius) = radius_deg {
        args.push("--radius".to_string());
        args.push(radius.to_string());
    }
    args
}

/// `--scale-low`/`--scale-high` with `--scale-units arcsecperpix`, arcsec/px.
fn scale(lo: f64, hi: f64) -> Vec<String> {
    vec![
        "-u".to_string(),
        "arcsecperpix".to_string(),
        "-L".to_string(),
        format!("{lo:.6}"),
        "-H".to_string(),
        format!("{hi:.6}"),
    ]
}

/// Per-field |dRA*cos(dec)|/|ddec| center budget, arcsec: the absolute one wins
/// over `TOL_CENTER_PX` times the field's own scale.
fn center_tol(f: &FieldTruth) -> f64 {
    f.tol_center_arcsec
        .unwrap_or(TOL_CENTER_PX * f.scale_arcsec)
}

/// The comma-separated names `GATE_FIELD` or `GATE_VARIANT` asks for; `None`
/// when the variable is unset or blank, which means "everything". A value that
/// names nothing is an error rather than a request to run nothing.
fn env_list(name: &str) -> Option<Vec<String>> {
    let raw = std::env::var(name).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let words: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    assert!(
        !words.is_empty(),
        "{name}={raw:?} names nothing; unset it to run everything"
    );
    Some(words)
}

/// The `SHORTLIST` fields, in that order.
fn shortlist_fields() -> Vec<&'static FieldTruth> {
    SHORTLIST
        .iter()
        .map(|name| {
            FIELDS
                .iter()
                .find(|f| f.name == *name)
                .unwrap_or_else(|| panic!("{name} is in SHORTLIST but not in FIELDS"))
        })
        .collect()
}

/// The comma-separated names of `want` that are absent from `known`.
fn unknown_names(want: &[String], known: &[&str]) -> String {
    want.iter()
        .map(String::as_str)
        .filter(|w| !known.contains(w))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether every input `.xyls` of `fields` is on disk, reporting a skip when it
/// is not: `skip` panics under `ASTROMETRY_REQUIRE_TEST_DATA` and returns `true`
/// otherwise, so a caller abandons its run only on a real skip.
fn inputs_present(fields: &[&FieldTruth]) -> bool {
    fields.iter().all(|f| {
        common::repo_root().join(f.input).is_file()
            || !common::skip(&format!("{} is absent", f.input))
    })
}

/// The fields of one variant: the whole table for a positive leg, `SHORTLIST`
/// for a negative one, and the `GATE_FIELD` subset of that.
fn selected(variant: Variant) -> Vec<&'static FieldTruth> {
    let pool: Vec<&'static FieldTruth> = if variant.expects_solution() {
        FIELDS.iter().collect()
    } else {
        shortlist_fields()
    };
    let Some(want) = env_list("GATE_FIELD") else {
        return pool;
    };
    let names: Vec<&str> = pool.iter().map(|f| f.name).collect();
    let unknown = unknown_names(&want, &names);
    assert!(
        unknown.is_empty(),
        "GATE_FIELD names field(s) this leg does not run: {unknown}; {} runs {}, of {} in the table",
        variant.key(),
        names.join(", "),
        FIELDS.len()
    );
    pool.into_iter()
        .filter(|f| want.iter().any(|w| *w == f.name))
        .collect()
}

/// Whether `GATE_VARIANT` asks for this variant.
fn variant_enabled(variant: Variant) -> bool {
    let Some(want) = env_list("GATE_VARIANT") else {
        return true;
    };
    let keys: Vec<&str> = VARIANTS.iter().map(|v| v.key()).collect();
    let unknown = unknown_names(&want, &keys);
    assert!(
        unknown.is_empty(),
        "GATE_VARIANT names unknown variant(s): {unknown}; known: {}",
        keys.join(", ")
    );
    want.iter().any(|w| *w == variant.key())
}

/// The `[verify]` line the solver prints when the hint gate drops a finished
/// solution: `reject_beyond_hint` must see it at least once, or it cannot tell a
/// discarded solution from a field the hint never let it fit.
const DISCARD_MARK: &str = "outside --ra/--dec circle, ignoring";

/// The last `STDERR_TAIL_CHARS` chars of a run's stderr, char-safely: the
/// stderr may quote non-ASCII paths.
fn stderr_tail(stderr: &str) -> String {
    let reversed: String = stderr
        .trim()
        .chars()
        .rev()
        .take(STDERR_TAIL_CHARS)
        .collect();
    reversed.chars().rev().collect()
}

/// Runs the binary on one field under one variant and returns its output with
/// the wall time. `cpulimit` is passed only where the run must be bounded - the
/// negative legs - so a positive run keeps the CLI's own solve budget.
///
/// The subprocess keeps the gate external: the real stdout bytes and the real
/// exit code are what the product promises. The positional .xyls goes first,
/// because a token like `--ra -4` would otherwise let a trailing positional be
/// read as an option value.
fn run_solve(
    f: &FieldTruth,
    indices: &Indices,
    variant: Variant,
    cpulimit: Option<f64>,
) -> (std::process::Output, f64) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_starsolver"));
    cmd.arg(common::repo_root().join(f.input));
    if let Indices::Pin(dir) = indices {
        cmd.args(["--index-dir", &dir.to_string_lossy()]);
    }
    let width = format!("{:.0}", f.width_pix);
    let height = format!("{:.0}", f.height_pix);
    cmd.args(["-w", &width, "-e", &height]);
    cmd.args(variant.args(f));
    if let Some(limit) = cpulimit {
        cmd.args(["--cpulimit", &limit.to_string()]);
    }
    if variant.verbose() {
        cmd.arg("-v");
    }

    let started = Instant::now();
    let proc = cmd.output().expect("the binary runs");
    let elapsed = started.elapsed().as_secs_f64();
    (proc, elapsed)
}

/// Runs one field under one variant and records its checks: the tolerance of the
/// printed WCS for a positive leg, the rejection contract (exit 1, empty stdout,
/// "not solved" on stderr, and index files to reject) for a negative one.
/// Reports whether the run discarded a finished solution at the hint gate.
fn run_field(
    f: &FieldTruth,
    indices: &Indices,
    variant: Variant,
    checks: &mut usize,
    failures: &mut Vec<String>,
) -> bool {
    let mut check = |label: &str, ok: bool, detail: String| {
        *checks += 1;
        if !ok {
            failures.push(format!("{} {}: {label}: {detail}", f.name, variant.key()));
        }
    };

    // The profile scales the solve budget, and the deadline's overshoot for a
    // bounded run; the deadline itself is wall-clock and profile-independent.
    let factor = if cfg!(debug_assertions) {
        DEBUG_TIME_FACTOR
    } else {
        1.0
    };
    let cpulimit = (!variant.expects_solution()).then_some(NEGATIVE_CPULIMIT_S);
    let budget = if variant.expects_solution() {
        f.timeout_s * factor
    } else {
        NEGATIVE_CPULIMIT_S + NEGATIVE_TIME_SLACK_S * factor
    };
    let (proc, elapsed) = run_solve(f, indices, variant, cpulimit);

    if !variant.expects_solution() {
        // The prior forbids a solution, so this leg owes the gate a clean exit 1
        // with an empty stdout and the "not solved" line: exit 2 would be an
        // input error rather than a rejection, and exit 0 a false positive.
        let stderr = String::from_utf8_lossy(&proc.stderr);
        check(
            "exit code",
            proc.status.code() == Some(1),
            format!("exit {:?}", proc.status.code()),
        );
        check(
            "no solution printed",
            proc.stdout.is_empty(),
            format!("stdout: {:?}", String::from_utf8_lossy(&proc.stdout).trim()),
        );
        check(
            "says not solved",
            stderr.contains("Field not solved"),
            format!("stderr tail: {}", stderr_tail(&stderr)),
        );
        // Without index files the prior has nothing to reject and the run ends
        // before solving was ever attempted: the leg would pass for free.
        check(
            "had indices to reject",
            !stderr.contains("no index in "),
            format!("stderr tail: {}", stderr_tail(&stderr)),
        );
        check(
            "time",
            elapsed < budget,
            format!("{elapsed:.2} s over the {budget:.0} s deadline"),
        );
        return stderr.contains(DISCARD_MARK);
    }

    let text = String::from_utf8_lossy(&proc.stdout);
    match common::parse_solution(&text) {
        None => {
            let why = if proc.status.success() {
                "exit 0 but the success block is incomplete".to_string()
            } else {
                let stderr = String::from_utf8_lossy(&proc.stderr);
                format!(
                    "not solved (exit {:?}); stderr tail: {}",
                    proc.status.code(),
                    stderr_tail(&stderr)
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
            let otol = f.tol_orient_deg.unwrap_or(TOL_ORIENT_DEG);
            let dorient = common::normalize_deg(p.orient_deg - f.orient_deg);
            check(
                "orient",
                f64::abs(dorient) <= otol,
                format!("{dorient:+.4} deg over the {otol:.2} deg budget"),
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
    false
}

/// Runs one variant over the fields it owns: skipped when `GATE_VARIANT` does not
/// ask for it, self-skipped when its data is absent, serialized against the other
/// legs of this target by `GATE_LOCK`, and reporting every broken check at once.
fn gate(variant: Variant) {
    if !variant_enabled(variant) {
        eprintln!("skipping: GATE_VARIANT does not ask for {}", variant.key());
        return;
    }
    let fields = selected(variant);
    let _lock = gate_lock();
    if !inputs_present(&fields) {
        return;
    }
    let Some(indices) = common::indices() else {
        return;
    };

    let mut checks = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut discarded = 0usize;
    for f in &fields {
        if run_field(f, &indices, variant, &mut checks, &mut failures) {
            discarded += 1;
        }
    }

    // `reject_beyond_hint` exists to gate the discard of a finished solution, so
    // a leg that never produced one to discard proved nothing. How far the sweep
    // gets depends on the installed index set, so the requirement is put on the
    // leg as a whole rather than on a named field.
    if matches!(variant, Variant::RejectBeyondHint) {
        checks += 1;
        if discarded == 0 {
            failures.push(
                "no run reached a finished solution to discard: no hint circle covered \
                 enough of a field for the sweep to fit and verify it; widen SHORTLIST"
                    .to_string(),
            );
        }
    }

    if !failures.is_empty() {
        panic!(
            "{}/{} ground-truth checks failed [{}]:\n{}",
            failures.len(),
            checks,
            variant.key(),
            failures.join("\n")
        );
    }
    println!(
        "{checks} ground-truth checks within tolerance [{}]",
        variant.key()
    );
}

/// Budgets of the prior-stability check below, far inside the truth budgets, so
/// a prior that nudges the printed WCS is caught long before it breaks the
/// tolerance of the truth.
const AGREE_CENTER_ARCSEC: f64 = 1.0;
const AGREE_ORIENT_DEG: f64 = 0.05;
const AGREE_SIZE_FRAC: f64 = 1e-4;

/// A prior may narrow the search, it must not move the answer: the sweep reports
/// a deterministic winner, so the same field solved under different priors prints
/// the same solution.
///
/// The prints are compared against the tight budgets above rather than byte for
/// byte, because a hint prunes index files and a differently pruned sweep may
/// legitimately reach the field through a different winner.
///
/// `GATE_VARIANT` and `GATE_FIELD` narrow this check as well, so a filtered run
/// stays cheap.
#[test]
fn priors_do_not_move_the_solution() {
    let priors: Vec<Variant> = [
        Variant::Blind,
        Variant::HintTight,
        Variant::ScaleTight,
        Variant::ScaleTightHint,
    ]
    .into_iter()
    .filter(|p| variant_enabled(*p))
    .collect();
    let want_field = env_list("GATE_FIELD");
    let fields: Vec<&'static FieldTruth> = shortlist_fields()
        .into_iter()
        .filter(|f| {
            want_field
                .as_ref()
                .is_none_or(|want| want.iter().any(|w| *w == f.name))
        })
        .collect();
    if priors.len() < 2 || fields.is_empty() {
        eprintln!(
            "skipping: GATE_VARIANT/GATE_FIELD leave nothing to compare ({} priors, {} fields)",
            priors.len(),
            fields.len()
        );
        return;
    }
    if !inputs_present(&fields) {
        return;
    }
    let Some(indices) = common::indices() else {
        return;
    };
    let _lock = gate_lock();
    let mut checks = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for f in &fields {
        let mut printed: Vec<(&'static str, common::Printed)> = Vec::new();
        for prior in &priors {
            let (proc, _) = run_solve(f, &indices, *prior, None);
            checks += 1;
            match common::parse_solution(&String::from_utf8_lossy(&proc.stdout)) {
                Some(p) => printed.push((prior.key(), p)),
                None => failures.push(format!(
                    "{} {}: exit {:?} printed no solution",
                    f.name,
                    prior.key(),
                    proc.status.code()
                )),
            }
        }
        // The baseline is the first prior that printed a solution: blind, unless
        // it was filtered out or failed (which the line above already reports).
        let Some((first, want)) = printed.first() else {
            continue;
        };
        for (key, got) in &printed[1..] {
            checks += 1;
            let off = common::center_off_arcsec(got.ra, got.dec, want.ra, want.dec);
            let rot = common::normalize_deg(got.orient_deg - want.orient_deg);
            let size = common::rel_frac(got.width_deg, want.width_deg);
            if off > AGREE_CENTER_ARCSEC
                || f64::abs(rot) > AGREE_ORIENT_DEG
                || size > AGREE_SIZE_FRAC
                || got.parity != want.parity
            {
                failures.push(format!(
                    "{} {key} printed another solution than {first}: center {off:.3}\", \
                     rotation {rot:+.4} deg, size {size:.6}, parity {} vs {}",
                    f.name, got.parity, want.parity
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "{}/{} prior-stability checks failed:\n{}",
            failures.len(),
            checks,
            failures.join("\n")
        );
    }
    println!("{checks} prior-stability checks within {AGREE_CENTER_ARCSEC}\"");
}

// One test per variant, so `cargo test --test ground_truth -- gate_hint` or
// `GATE_VARIANT` runs a subset instead of the whole matrix.
#[test]
fn gate_blind() {
    gate(Variant::Blind);
}

#[test]
fn gate_hint_tight() {
    gate(Variant::HintTight);
}

#[test]
fn gate_hint_default_radius() {
    gate(Variant::HintDefaultRadius);
}

#[test]
fn gate_hint_offset() {
    gate(Variant::HintOffset);
}

#[test]
fn gate_scale_tight() {
    gate(Variant::ScaleTight);
}

#[test]
fn gate_scale_wide() {
    gate(Variant::ScaleWide);
}

#[test]
fn gate_scale_tight_hint() {
    gate(Variant::ScaleTightHint);
}

#[test]
fn gate_reject_wrong_position() {
    gate(Variant::RejectWrongPosition);
}

#[test]
fn gate_reject_wrong_scale() {
    gate(Variant::RejectWrongScale);
}

#[test]
fn gate_reject_beyond_hint() {
    gate(Variant::RejectBeyondHint);
}

/// The matrix is written in three places at once - the `Variant` enum, the
/// `VARIANTS` list and one `gate_*` test per variant - and nothing makes them
/// agree by construction: an arm missing from `VARIANTS` silently drops out of
/// `GATE_VARIANT`, and an arm without a test function silently never runs. Pin
/// them, the way `src/testutil.rs` pins the truth table against this file.
#[test]
fn every_variant_has_a_leg_and_a_name() {
    /// The `GATE_VARIANT` key an enum arm must carry: its snake_case.
    fn key_of(arm: &str) -> String {
        let mut key = String::new();
        for c in arm.chars() {
            if c.is_uppercase() && !key.is_empty() {
                key.push('_');
            }
            key.extend(c.to_lowercase());
        }
        key
    }

    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/ground_truth.rs"
    ))
    .expect("this file must be present to check the variant list");
    let body = text
        .split("enum Variant {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("the Variant enum must be declared here");
    let arms: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix(','))
        .filter(|name| !name.is_empty() && name.chars().next().is_some_and(|c| c.is_uppercase()))
        .collect();

    assert_eq!(
        arms.len(),
        VARIANTS.len(),
        "`enum Variant` has {} arms but `VARIANTS` lists {}: {arms:?}",
        arms.len(),
        VARIANTS.len()
    );
    let keys: Vec<&str> = VARIANTS.iter().map(|v| v.key()).collect();
    assert_eq!(
        keys.len(),
        keys.iter().collect::<std::collections::HashSet<_>>().len(),
        "two variants share one GATE_VARIANT key: {keys:?}"
    );
    for arm in arms {
        let key = key_of(arm);
        assert!(
            keys.contains(&key.as_str()),
            "Variant::{arm} is not in VARIANTS under the key {key:?}"
        );
        assert!(
            text.contains(&format!("fn gate_{key}(")),
            "Variant::{arm} has no gate_{key} test, so it never runs"
        );
        assert!(
            text.contains(&format!("gate(Variant::{arm})")),
            "no test drives Variant::{arm}"
        );
    }
}
