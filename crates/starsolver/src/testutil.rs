//! Helpers shared by the data-gated tests. Three kinds of input, gated independently:
//!
//! - **Index files are not part of the repository.** Tests use the system astrometry.net install
//!   (the dirs the binary uses without `--index-dir`: [`IndexDirs`], i.e. `add_path` of
//!   `astrometry.cfg` else the conventional install dirs), or `$ASTROMETRY_TEST_INDEX_DIR` (a path
//!   list) when set. Since that set is not ours to pin, a test must not name an index id: it
//!   selects by header properties ([`smallest_index`], [`dim4_index`]) or takes the index that
//!   actually solved a field ([`field_run`]), asserting invariants, not numbers one file owns.
//! - **Field files** (`testdata/*.xyls`): the small, irreplaceable half of the fixtures.
//! - **Generated fields** need no fixture: `genfield` synthesizes one from the installed indices,
//!   so those gates run without `testdata/` ([`generated_index`], [`generated_field_run`]).
//!
//! Every gate goes through [`skip`]: absent data prints a `skipping:` note and the caller returns
//! early; a present but corrupt file is not a skip - loaders panic, because data that is there is
//! treated as known-good. `ASTROMETRY_REQUIRE_TEST_DATA=1` turns a would-be skip into a panic, so
//! CI can tell an all-skipped run from an all-passed one (`cargo test` hides the note without
//! `--nocapture`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::coords::deg2arcsec;
use crate::fits::Endian;
use crate::index::{Index, IndexMeta, Unusable, load_metas_in, path_list};
use crate::indexdir::IndexDirs;
use crate::params::{MAX_OBJS, MAX_WIDTH_DEG, MIN_WIDTH_DEG};
use crate::solver::{FieldSpec, Flow, PositionHint, QuadHit, solve_index};
use crate::verify::{FieldPre, VerifyResult, verify_hit};
use crate::wcs::Wcs;
use crate::xyls::Star;

use crate::cli::{ScaleUnits, SolveOptions};
use crate::sweep::{OBJ_ROUND_STEP, Solution, select_indices, solve_field};

/// `<crate root>/<dir>/<name>`, from compile-time `CARGO_MANIFEST_DIR` (not the runtime cwd).
pub(crate) fn data_path(dir: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(dir).join(name)
}

/// `testdata/<name>`.
#[must_use]
pub(crate) fn testdata_path(name: &str) -> PathBuf {
    data_path("testdata", name)
}

/// Reports a skipped test: note + `false`; panics if `ASTROMETRY_REQUIRE_TEST_DATA` is truthy.
pub(crate) fn skip(msg: &str) -> bool {
    let strict = matches!(
        std::env::var("ASTROMETRY_REQUIRE_TEST_DATA").as_deref(),
        Ok(v) if !v.is_empty() && v != "0"
    );
    if strict {
        panic!("test data required but absent: {msg}");
    }
    eprintln!("skipping: {msg}");
    false
}

/// Is `<crate root>/<dir>/<name>` present (skip/strict rules as in [`skip`])?
#[must_use]
pub(crate) fn have_file(dir: &str, name: &str) -> bool {
    let path = data_path(dir, name);
    path.is_file() || skip(&format!("no {}", path.display()))
}

/// A header-shaped `IndexMeta` stub for tests that need no index file.
pub(crate) fn meta_stub(indexid: i64, healpix: i64, hpnside: i64) -> IndexMeta {
    IndexMeta {
        path: PathBuf::from(format!("index-{indexid}.fits")),
        indexid,
        dimquads: 4,
        nquads: 1,
        nstars: 2,
        scale_lower_arcsec: 960.0,
        scale_upper_arcsec: 1320.0,
        healpix,
        hpnside,
        allsky: healpix < 0,
        endian: Endian::Little,
        circle: true,
        cx_less_than_dx: false,
        meanx_less_than_half: false,
    }
}

/// Loads `testdata/<name>`, or `None` after a skip when the file is absent.
pub(crate) fn load_field(name: &str) -> Option<Vec<Star>> {
    let path = testdata_path(name);
    if !path.is_file() {
        skip(&format!("no {}", path.display()));
        return None;
    }
    Some(crate::xyls::load(&path).expect("xyls load failed"))
}

// ---- Index discovery: the machine's astrometry.net install ----------------

/// The suite's index dirs: `$ASTROMETRY_TEST_INDEX_DIR` (a path list) verbatim
/// when set, else the system install [`IndexDirs`] finds.
static TEST_DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();

fn test_dirs() -> &'static [PathBuf] {
    TEST_DIRS.get_or_init(|| match std::env::var_os("ASTROMETRY_TEST_INDEX_DIR") {
        Some(list) => std::env::split_paths(&list).collect(),
        None => match IndexDirs::resolve(None) {
            Ok(found) => found.dirs,
            Err(e) => {
                eprintln!("test data: {e:#}");
                Vec::new()
            }
        },
    })
}

/// Metadata of every index the suite can use, in sweep order, scanned once per test binary
/// (headers only). Formats this solver does not read are skipped, not fatal (`Unusable::Skip`);
/// many tiles legitimately share one `INDEXID`, which [`load_metas_in`] warns about once per id.
pub(crate) fn metas() -> Option<&'static [IndexMeta]> {
    static CACHE: OnceLock<Vec<IndexMeta>> = OnceLock::new();
    let metas = CACHE.get_or_init(|| match load_metas_in(test_dirs(), Unusable::Skip) {
        Ok(metas) => metas,
        Err(e) => {
            eprintln!("test data: {e:#}");
            Vec::new()
        }
    });
    if metas.is_empty() {
        return skipped(&format!(
            "no index files in {} (install astrometry-data, or set \
             ASTROMETRY_TEST_INDEX_DIR)",
            path_list(test_dirs())
        ));
    }
    Some(metas)
}

/// [`skip`]s `msg` and returns `None`, so a selector can end with `?`.
#[must_use]
pub(crate) fn skipped<T>(msg: &str) -> Option<T> {
    skip(msg);
    None
}

/// File size, for selector ordering; 0 when it cannot be stat'ed, so the
/// selector prefers that file and [`load`] reports it.
fn file_len(meta: &IndexMeta) -> u64 {
    std::fs::metadata(&meta.path).map(|m| m.len()).unwrap_or(0)
}

/// Deterministic ordering for the "smallest suitable file" selectors.
fn by_size_then_id(a: &&IndexMeta, b: &&IndexMeta) -> std::cmp::Ordering {
    file_len(a)
        .cmp(&file_len(b))
        .then(a.indexid.cmp(&b.indexid))
        .then(a.healpix.cmp(&b.healpix))
}

/// The smallest index file with at least `min_stars` stars: fast real kd-tree
/// data, or a catalog big enough for the star counts a test builds.
pub(crate) fn smallest_index(min_stars: usize) -> Option<&'static IndexMeta> {
    metas()?
        .iter()
        .filter(|m| m.nstars >= min_stars)
        .min_by(by_size_then_id)
        .or_else(|| skipped(&format!("no installed index has >= {min_stars} stars")))
}

/// The `n` smallest installed files in [`by_size_then_id`] order; `None` only via [`metas`].
pub(crate) fn smallest_indices(n: usize) -> Option<Vec<&'static IndexMeta>> {
    let mut files: Vec<&IndexMeta> = metas()?.iter().collect();
    files.sort_by(by_size_then_id);
    files.truncate(n);
    Some(files)
}

/// `n` installed files spread across the size ordering (smallest, middle, largest, ...), so a
/// test standing for "any index" also sees deeper trees and denser code distributions. Distinct
/// while the install holds `n` files; `None` only via [`metas`].
pub(crate) fn spread_indices(n: usize) -> Option<Vec<&'static IndexMeta>> {
    let metas = metas()?;
    let mut files: Vec<&IndexMeta> = metas.iter().collect();
    if files.is_empty() {
        return skipped("no installed index files");
    }
    files.sort_by(by_size_then_id);
    let n = usize::min(n, files.len());
    let last = files.len() - 1;
    let mut picked: Vec<usize> = Vec::with_capacity(n);
    for i in 0..n {
        let pos = usize::min(i * last / usize::max(n - 1, 1), last);
        if !picked.contains(&pos) {
            picked.push(pos);
        }
    }
    Some(picked.iter().map(|&pos| files[pos]).collect())
}

/// The smallest `dimquads == 4` index: the 4-star quad code path and its
/// `CIRCLE`/`CXDX`/`CXDXLT1` filters.
pub(crate) fn dim4_index() -> Option<&'static IndexMeta> {
    metas()?
        .iter()
        .filter(|m| m.dimquads == 4)
        .min_by(by_size_then_id)
        .or_else(|| skipped("no dimquads=4 index installed"))
}

/// Loads a selected index: mmap plus lazy byte views, so the caller decides how
/// long to keep it resident. An unreadable file is a fixture failure: panic.
#[must_use]
pub(crate) fn load(meta: &IndexMeta) -> Index {
    Index::load(&meta.path).expect("index load failed")
}

// ---- Generated fields: an index to synthesize them from -------------------

/// An installed index a synthetic field can be generated from, or a skip: the pick
/// `genfield::default_index` makes, over the already-scanned [`metas`] (so a multi-directory
/// install is searched as a whole, headers parsed once). Cached: several gates ask for it.
#[must_use]
pub(crate) fn generated_index() -> Option<PathBuf> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    let cached = CACHE.get_or_init(|| match metas() {
        Some(metas) => match crate::genfield::default_index_from(metas) {
            Ok(path) => Some(path),
            Err(e) => skipped(&format!(
                "no index a generated field can be built from: {e:#}"
            )),
        },
        None => None,
    });
    cached.clone()
}

/// `Params` of a seeded generated field, or a skip when the installed set cannot produce one at
/// this frame size (a coarse all-sky-only set has no narrow field): an install property, not a
/// code failure.
#[must_use]
pub(crate) fn generated_params(
    seed: u64,
    width: f64,
    height: f64,
) -> Option<crate::genfield::Params> {
    let index = generated_index()?;
    match crate::genfield::random_params(&index, seed, width, height) {
        Ok(p) => Some(p),
        Err(e) => skipped(&format!(
            "no generated field from {}: {e:#}",
            index.display()
        )),
    }
}

// ---- Sky truth of the shipped fields the unit gates replay ----------------

/// Gate budgets, mirrored from `tests/ground_truth.rs` for the unit gates and pinned against it
/// by `gate_budgets_match_the_cargo_gate`. One deliberate asymmetry: where that gate checks each
/// center component separately, [`Truth::check`] gates the center radially (hypot) and is stricter.
pub(crate) const TOL_CENTER_PX: f64 = 1.5;
pub(crate) const TOL_ORIENT_DEG: f64 = 0.5;
pub(crate) const TOL_SCALE_FRAC: f64 = 0.01;
pub(crate) const TOL_SIZE_FRAC: f64 = 0.005;

/// Matched-star floor the data-gated tests demand of a winner: bogus hits match few stars, real
/// winners far more; the exact count belongs to the index set, so it is deliberately not pinned.
pub(crate) const MIN_WINNER_NMATCH: usize = 15;

/// Angle difference normalized to (-180, 180], as the end-to-end gate does.
#[must_use]
pub(crate) fn wrap_deg(a: f64) -> f64 {
    let a = f64::rem_euclid(a + 180.0, 360.0) - 180.0;
    if a == -180.0 { a + 360.0 } else { a }
}

/// Angular offset of two sky positions in pixels of `scale_arcsec`: RA scaled by `cos(dec)` of the
/// first (reported) position as in [`Truth::check`] and `tests/common`, the two combined radially.
#[must_use]
pub(crate) fn center_off_px(ra1: f64, dec1: f64, ra2: f64, dec2: f64, scale_arcsec: f64) -> f64 {
    let dra = wrap_deg(ra1 - ra2) * f64::cos(f64::to_radians(dec1));
    f64::hypot(dra, dec1 - dec2) * 3600.0 / scale_arcsec
}

/// A field's published sky truth (the reference run recorded in `tests/ground_truth.rs`, mirrored
/// here for the unit gates) plus the gate that test applies to the printed solution. Both copies of
/// the numbers are pinned by `truth_consts_match_the_cargo_gate`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Truth {
    pub(crate) ra_deg: f64,
    pub(crate) dec_deg: f64,
    pub(crate) scale_arcsec: f64,
    pub(crate) width_deg: f64,
    pub(crate) height_deg: f64,
    pub(crate) orient_deg: f64,
    pub(crate) parity_neg: bool,
    /// Absolute center budget that overrides the pixel one (real-centroid 1920x1080 fields).
    pub(crate) tol_center_arcsec: Option<f64>,
}

impl Truth {
    /// Checks a printed solution against this truth: center within the budget (radially), rotation
    /// wrapped, scale derived from the printed width, both printed sizes, parity exact.
    pub(crate) fn check(&self, sol: &Solution, width_pix: f64) {
        let tol = self
            .tol_center_arcsec
            .unwrap_or(TOL_CENTER_PX * self.scale_arcsec);
        let dra = wrap_deg(sol.ra - self.ra_deg) * f64::cos(sol.dec.to_radians()) * 3600.0;
        let ddec = (sol.dec - self.dec_deg) * 3600.0;
        assert!(
            f64::hypot(dra, ddec) <= tol,
            "center off by {} arcsec (budget {tol:.3}\")",
            f64::hypot(dra, ddec)
        );
        let drot = wrap_deg(sol.orient_deg - self.orient_deg);
        assert!(
            f64::abs(drot) <= TOL_ORIENT_DEG,
            "rotation off by {drot} deg: {}",
            sol.orient_deg
        );
        let scale = sol.width_deg * 3600.0 / width_pix;
        assert!(
            f64::abs(scale / self.scale_arcsec - 1.0) <= TOL_SCALE_FRAC,
            "scale {scale} arcsec/px vs truth {}",
            self.scale_arcsec
        );
        for (name, got, want) in [
            ("width", sol.width_deg, self.width_deg),
            ("height", sol.height_deg, self.height_deg),
        ] {
            assert!(
                f64::abs(got / want - 1.0) <= TOL_SIZE_FRAC,
                "{name} {got} deg vs truth {want} deg"
            );
        }
        assert_eq!(sol.parity_neg, self.parity_neg, "parity");
    }

    /// Center offset of a WCS report from this truth, in px of the report's own plate scale.
    #[must_use]
    pub(crate) fn center_off_px(&self, w: &Wcs) -> f64 {
        center_off_px(
            w.crval_deg[0],
            w.crval_deg[1],
            self.ra_deg,
            self.dec_deg,
            w.arcsec_per_pix(),
        )
    }
}

/// A generated field's exact truth as a [`Truth`], for [`Truth::center_off_px`]. Not for
/// [`Truth::check`]: the shipped budgets are several times looser than any generated-field gate. No
/// absolute center budget: at these scales the pixel one is the tighter of the two.
#[must_use]
pub(crate) fn exact_truth(t: &crate::genfield::Truth) -> Truth {
    Truth {
        ra_deg: t.ra_deg,
        dec_deg: t.dec_deg,
        scale_arcsec: t.scale_arcsec,
        width_deg: t.width_deg,
        height_deg: t.height_deg,
        orient_deg: t.orient_deg,
        parity_neg: t.parity_neg,
        tol_center_arcsec: None,
    }
}

/// The shipped fields' sky truth, the same numbers the end-to-end gate carries (its comments record
/// the reference runs). Only the fields a unit gate replays are mirrored here.
pub(crate) const FIELD1_TRUTH: Truth = Truth {
    ra_deg: 60.031038,
    dec_deg: 36.375976,
    scale_arcsec: 2.71247,
    width_deg: 3.50687,
    height_deg: 2.65305,
    orient_deg: -178.949,
    parity_neg: true,
    tol_center_arcsec: None,
};
pub(crate) const FIELD2_TRUTH: Truth = Truth {
    ra_deg: 43.67983,
    dec_deg: 60.425902,
    scale_arcsec: 2.71151,
    width_deg: 3.50552,
    height_deg: 2.65188,
    orient_deg: -177.609,
    parity_neg: true,
    tol_center_arcsec: None,
};
pub(crate) const NARROW1_TRUTH: Truth = Truth {
    ra_deg: 202.484305,
    dec_deg: 47.232352,
    scale_arcsec: 0.996643,
    width_deg: 0.531127,
    height_deg: 0.298915,
    orient_deg: -74.729,
    parity_neg: true,
    tol_center_arcsec: Some(2.0),
};
pub(crate) const NARROW2_TRUTH: Truth = Truth {
    ra_deg: 250.423162,
    dec_deg: 36.479129,
    scale_arcsec: 0.997071,
    width_deg: 0.531737,
    height_deg: 0.299092,
    orient_deg: 106.362,
    parity_neg: true,
    tol_center_arcsec: Some(2.0),
};
/// `testdata/platesolve_13932859987089881536.xyls`, for the hinted fanout leg of `sweep.rs` (it
/// needs a circle at the field's true center). That field reaches the fifth depth round before it
/// solves, which is what makes the leg run the hint structures a sweep builds only from round 2 on.
pub(crate) const PLATESOLVE1_TRUTH: Truth = Truth {
    ra_deg: 313.855978,
    dec_deg: 44.34006,
    scale_arcsec: 2.93399,
    width_deg: 5.06922,
    height_deg: 3.3959,
    orient_deg: 161.961,
    parity_neg: true,
    tol_center_arcsec: None,
};

// ---- Field runs: the index and round that solve a field --------------------

/// Everything a replayed round needs: the index and depth round that solved the field, the hit that
/// round reported, the field stars in pipeline order. From [`field_run`] / [`generated_field_run`].
pub(crate) struct FieldRun {
    pub(crate) meta: &'static IndexMeta,
    pub(crate) field: String,
    pub(crate) width: f64,
    pub(crate) height: f64,
    /// Scale range the sweep ran with: the blind one in [`field_run`], the
    /// generator's window in [`generated_field_run`].
    pub(crate) app_min: f64,
    pub(crate) app_max: f64,
    pub(crate) radecs: Option<PositionHint>,
    /// The winning round, as the `[round] field objects a-b` line numbers it.
    pub(crate) obj_start: usize,
    pub(crate) max_objs: usize,
    /// The field, already reordered the way `augment::preprocess` does.
    pub(crate) stars: Vec<Star>,
    /// The round's reported winner: lexicographic best (log-odds, quad).
    pub(crate) quad: usize,
    pub(crate) log_odds: f64,
}

/// Cache key: field file, frame size and hint; floats as bits, so `Eq + Hash`.
type RunKey = (String, u64, u64, Option<(u64, u64, u64)>);

fn run_key(field: &str, width: f64, height: f64, radecs: Option<PositionHint>) -> RunKey {
    (
        field.to_string(),
        width.to_bits(),
        height.to_bits(),
        radecs.map(|h| {
            (
                h.ra_deg.to_bits(),
                h.dec_deg.to_bits(),
                h.radius_deg.to_bits(),
            )
        }),
    )
}

/// One [`field_run`] cache slot: its `OnceLock` makes concurrent callers of the
/// same key wait on the in-flight discovery instead of each repeating it.
type RunSlot = OnceLock<Option<&'static FieldRun>>;
type RunCache = OnceLock<Mutex<HashMap<RunKey, Arc<RunSlot>>>>;

/// The index and round that solve `field`: [`discover`] over the installed set, cached (negative
/// results too) per field + frame + hint, because the search costs what a real solve does - a few
/// seconds for the wide fields, the only expensive step of the suite; tests then re-run just the
/// winning index or round. Several tests share one run at suite start, hence the per-key slot.
///
/// A field the installed set cannot solve is a skip, not a failure: which indices a machine has is
/// not ours to decide. Under `ASTROMETRY_REQUIRE_TEST_DATA` it is a panic, as everywhere else here.
pub(crate) fn field_run(
    field: &str,
    width: f64,
    height: f64,
    radecs: Option<PositionHint>,
) -> Option<&'static FieldRun> {
    static CACHE: RunCache = OnceLock::new();
    let key = run_key(field, width, height, radecs);
    let slot = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(key)
        .or_insert_with(|| Arc::new(OnceLock::new()))
        .clone();
    let run = *slot.get_or_init(|| discover(field, width, height, radecs));
    run.or_else(|| {
        skipped(&format!(
            "{field} {width}x{height}: no installed index solves it"
        ))
    })
}

/// CPU budget of a [`discover`] sweep: unlimited for a unit test, yet a `Duration` deadline.
const DISCOVER_CPULIMIT_S: f64 = 3600.0;

/// Solves the field the way the binary does - [`solve_field`] over [`metas`] pre-filtered by the
/// same [`select_indices`] gate, at the blind scale range - minus `run_into`'s brightest-first cap
/// (this path hands the sweep the uncapped [`xyls::load`] order). The record is therefore what the
/// binary would have reported but for that cap - index, round window, winner quad, log-odds - for
/// any hint, which is what lets tests re-run the winning index instead of hardcoding an index id.
///
/// The blind scale range matters: the index whose nominal band contains the field width is often
/// not the one that solves it (a quad diameter is smaller than the field it is found in).
fn discover(
    field: &str,
    width: f64,
    height: f64,
    radecs: Option<PositionHint>,
) -> Option<&'static FieldRun> {
    let metas = metas()?;
    let stars = load_field(field)?;
    let app_min = deg2arcsec(MIN_WIDTH_DEG) / width;
    let app_max = deg2arcsec(MAX_WIDTH_DEG) / width;
    let opts = SolveOptions {
        file: testdata_path(field),
        width,
        height,
        app_min,
        app_max,
        radecs,
        cpulimit: DISCOVER_CPULIMIT_S,
        verbose: false,
        units: ScaleUnits::DegWidth,
    };
    let run = solve_into_run(metas, stars, &opts, field)?;
    // One leaked record per (field, frame, hint): a handful per suite at most,
    // and `&'static` keeps every helper here free of lifetimes.
    Some(Box::leak(Box::new(run)))
}

/// Solves `stars` the way the binary would ([`solve_field`] over the [`select_indices`] pre-filter
/// of `metas`, at `opts`' frame, scale range and hint; the list as handed, without the `run_into`
/// cap) and records the run the binary would have reported.
///
/// Shared by [`discover`] and [`generated_field_run`] so the two cannot drift on how a run is
/// recorded; `None` (nothing solved, no meta matched the winner, sweep error) is the caller's skip.
fn solve_into_run(
    metas: &'static [IndexMeta],
    stars: Vec<Star>,
    opts: &SolveOptions,
    field: &str,
) -> Option<FieldRun> {
    let selected = select_indices(metas, opts);
    let deadline = Instant::now() + Duration::from_secs_f64(DISCOVER_CPULIMIT_S);
    let sol = match solve_field(opts, &stars, &selected, deadline) {
        Ok(Some(sol)) => sol,
        // Both are the caller's skip to phrase; a sweep error is a data problem,
        // so it is reported here too.
        Ok(None) => return None,
        Err(e) => {
            eprintln!("test data: {e:#}");
            return None;
        }
    };
    // Map the winner back to its meta by (indexid, healpix), the identity `run()`'s verbose
    // selection uses. Two files sharing both (same indexid, different HPNSIDE) resolve to the
    // first in sweep order - one that just solved the field, so the replays still hold.
    let meta = metas
        .iter()
        .find(|m| m.indexid == sol.winner.indexid && m.healpix == sol.winner.healpix)?;
    Some(FieldRun {
        meta,
        field: field.to_string(),
        width: opts.width,
        height: opts.height,
        app_min: opts.app_min,
        app_max: opts.app_max,
        radecs: opts.radecs,
        obj_start: sol.winner.obj_start,
        max_objs: sol.winner.max_objs,
        stars,
        quad: sol.winner.quad,
        log_odds: sol.winner.log_odds,
    })
}

/// The same [`FieldRun`] for a generated field, plus the exact truth it was built from: for the
/// gate that replays the winning round (refine) on a field whose answer is known by construction
/// rather than taken from a reference run.
///
/// The sweep runs with the generator's own scale window, which is what [`replay_round`] replays
/// with, so the recorded round is the one that really won. A field the set cannot produce or solve
/// is a skip, as elsewhere.
pub(crate) fn generated_field_run(
    seed: u64,
    width: f64,
    height: f64,
) -> Option<(FieldRun, crate::genfield::Truth)> {
    let metas = metas()?;
    let p = generated_params(seed, width, height)?;
    let (stars, truth) = match crate::genfield::generate_stars(&p) {
        Ok(v) => v,
        Err(e) => return skipped(&format!("generated field at seed {seed}: {e:#}")),
    };
    let (app_min, app_max) = crate::genfield::scale_window(p.scale_arcsec);
    let opts = SolveOptions {
        // Names the run in messages only; `solve_field` never opens it.
        file: PathBuf::from(format!("generated-{seed}.xyls")),
        width,
        height,
        app_min,
        app_max,
        radecs: None,
        cpulimit: DISCOVER_CPULIMIT_S,
        verbose: false,
        units: ScaleUnits::DegWidth,
    };
    let run = solve_into_run(metas, stars, &opts, &format!("generated:{seed}")).or_else(|| {
        skipped(&format!(
            "generated field at seed {seed} ({app_min:.3}..{app_max:.3} arcsec/pixel) produced no usable run (no solve, or a sweep error above)"
        ))
    })?;
    Some((run, truth))
}

/// A hit of a replayed depth round together with its verification outcome.
pub(crate) struct RoundHit {
    pub(crate) hit: QuadHit,
    pub(crate) verify: VerifyResult,
}

/// Replays the depth round `obj_start..max_objs` on the index that solved the field, returning
/// every code hit with its verify result in rayon scheduling order (callers sort on what they
/// compare).
#[must_use]
pub(crate) fn replay_round(run: &FieldRun, obj_start: usize, max_objs: usize) -> Vec<RoundHit> {
    let index = load(run.meta);
    let spec = FieldSpec {
        stars: &run.stars,
        width: run.width,
        height: run.height,
        app_min: run.app_min,
        app_max: run.app_max,
        radecs: run.radecs,
        // Replays search the index's own codes tree: there is no hint tree here, and they
        // must see the hits the first round of the original sweep saw.
        hint_codes: None,
        max_objs,
        obj_start,
        verbose: false,
        deadline: None,
        cancel: None,
    };
    let hits: Mutex<Vec<RoundHit>> = Mutex::new(Vec::new());
    let pre = FieldPre::new(&run.stars);
    solve_index(&index, &spec, &|hit| {
        let vr = verify_hit(&index, hit, &run.stars, &pre, run.width, run.height);
        hits.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RoundHit {
                hit: hit.clone(),
                verify: vr,
            });
        Flow::Continue
    });
    hits.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Hits of `run`'s round that verify as solved, in quad order (rayon delivers them in scheduling
/// order). This replaces pinning a quad id: the refine and racing-hit tests take real verifies.
#[must_use]
pub(crate) fn solved_hits(run: &FieldRun) -> Vec<RoundHit> {
    let mut hits: Vec<RoundHit> = replay_round(run, run.obj_start, run.max_objs)
        .into_iter()
        .filter(|h| h.verify.solved())
        .collect();
    hits.sort_by_key(|h| h.hit.quad);
    hits
}

/// Depth rounds [`solved_hits_with_peers`] may run past the winning round before giving up.
const PEER_ROUNDS_MAX: usize = 3;

/// The winning round's solved hits in quad order; when it gives fewer than `min_want` (a round's
/// next-best hit is often junk below the solve gate), extended with the solved hits of up to
/// `PEER_ROUNDS_MAX` following depth rounds of the same index - fits over different field-star
/// subsets, what a racing-hit comparison needs when one round's raw fits disagree. Panics when even
/// those rounds cannot supply `min_want`: the caller needs peers.
#[must_use]
pub(crate) fn solved_hits_with_peers(run: &FieldRun, min_want: usize) -> Vec<RoundHit> {
    let mut hits = solved_hits(run);
    let cap = usize::min(run.stars.len(), MAX_OBJS);
    let mut lo = run.max_objs;
    let mut replays = 0;
    while hits.len() < min_want && replays < PEER_ROUNDS_MAX && lo < cap {
        let hi = usize::min(lo + OBJ_ROUND_STEP, cap);
        let more: Vec<RoundHit> = replay_round(run, lo, hi)
            .into_iter()
            .filter(|h| h.verify.solved() && !hits.iter().any(|s| s.hit.quad == h.hit.quad))
            .collect();
        hits.extend(more);
        lo = hi;
        replays += 1;
    }
    assert!(
        hits.len() >= min_want,
        "neither round {}-{} nor the next {PEER_ROUNDS_MAX} depth rounds of the winning index \
         produced {min_want} solved hits to compare against",
        run.obj_start + 1,
        run.max_objs
    );
    hits.sort_by_key(|h| h.hit.quad);
    hits
}

/// The round's reported winner: [`FieldRun::quad`] with its verify result.
///
/// # Panics
/// If the recorded winner is not among the replayed hits (same round, same params): a regression.
#[must_use]
pub(crate) fn winning_hit(run: &FieldRun) -> RoundHit {
    solved_hits(run)
        .into_iter()
        .find(|h| h.hit.quad == run.quad)
        .unwrap_or_else(|| panic!("quad {} is not a solved hit of its own round", run.quad))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The absent-data path self-skips (`None`) rather than panicking - the whole point of the
    /// helpers, exercised without data. Under `ASTROMETRY_REQUIRE_TEST_DATA` it must fail loudly.
    #[test]
    fn absent_field_file_self_skips() {
        let missing = "no-such-field-file-xyz.xyls";
        assert!(
            !testdata_path(missing).is_file(),
            "{missing} must not exist"
        );
        let strict = matches!(
            std::env::var("ASTROMETRY_REQUIRE_TEST_DATA").as_deref(),
            Ok(v) if !v.is_empty() && v != "0"
        );
        if strict {
            let caught = std::panic::catch_unwind(|| load_field(missing));
            assert!(caught.is_err(), "strict mode must fail on absent data");
        } else {
            assert!(
                load_field(missing).is_none(),
                "an absent field file must self-skip, not panic"
            );
        }
    }

    /// The gate budgets exist twice: the constants above and `tests/ground_truth.rs`, nothing
    /// deriving one from the other. Pin them: on drift `cargo test` would keep accepting solutions
    /// the end-to-end gate rejects.
    #[test]
    fn gate_budgets_match_the_cargo_gate() {
        let rs = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ground_truth.rs"
        ))
        .expect("tests/ground_truth.rs must be present to check the budgets");
        let gate = |name: &str| -> f64 {
            let prefix = format!("const {name}: f64 = ");
            let raw = rs
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap_or_else(|| panic!("{name} const not found in tests/ground_truth.rs"));
            raw.trim()
                .trim_end_matches(';')
                .parse()
                .unwrap_or_else(|e| panic!("cannot parse the cargo gate's {name}: {e}"))
        };
        for (name, here) in [
            ("TOL_CENTER_PX", TOL_CENTER_PX),
            ("TOL_ORIENT_DEG", TOL_ORIENT_DEG),
            ("TOL_SCALE_FRAC", TOL_SCALE_FRAC),
            ("TOL_SIZE_FRAC", TOL_SIZE_FRAC),
        ] {
            assert_eq!(
                gate(name),
                here,
                "{name} drifted out of sync with tests/ground_truth.rs"
            );
        }
    }

    /// The sky truth is written twice: the `Truth` consts above and the `FIELDS` table of
    /// `tests/ground_truth.rs`, nothing deriving one from the other, so pin them number by number.
    /// A gate entry no unit gate replays (`platesolve`) has no const here, so is not in the list.
    #[test]
    fn truth_consts_match_the_cargo_gate() {
        /// The `FieldTruth { .. }` literal that declares one field.
        fn field_block(rs: &str, name: &str) -> String {
            let marker = format!("name: \"{name}\",");
            let at = rs
                .find(&marker)
                .unwrap_or_else(|| panic!("{name} is not declared in tests/ground_truth.rs"));
            let open = rs[..at]
                .rfind("FieldTruth {")
                .unwrap_or_else(|| panic!("no literal opens the {name} entry"));
            let close = at
                + rs[at..]
                    .find("},")
                    .unwrap_or_else(|| panic!("no literal closes the {name} entry"));
            rs[open..close].to_string()
        }

        /// The token written for one key of a `FieldTruth` literal.
        fn value(block: &str, key: &str) -> String {
            let prefix = format!("{key}: ");
            block
                .lines()
                .find_map(|line| line.trim().strip_prefix(&prefix))
                .unwrap_or_else(|| panic!("the gate's field literal has no {key}"))
                .trim()
                .trim_end_matches(',')
                .to_string()
        }

        fn number(block: &str, key: &str) -> f64 {
            let raw = value(block, key);
            raw.parse()
                .unwrap_or_else(|e| panic!("cannot parse the gate's {key} {raw:?}: {e}"))
        }

        fn optional(block: &str, key: &str) -> Option<f64> {
            let raw = value(block, key);
            if let Some(inner) = raw.strip_prefix("Some(").and_then(|s| s.strip_suffix(')')) {
                return Some(
                    inner
                        .parse()
                        .unwrap_or_else(|e| panic!("cannot parse the gate's {key} {raw:?}: {e}")),
                );
            }
            assert_eq!(raw, "None", "the gate's {key} must be None or Some(..)");
            None
        }

        fn flag(block: &str, key: &str) -> bool {
            match value(block, key).as_str() {
                "true" => true,
                "false" => false,
                raw => panic!("the gate's {key} must be true or false, got {raw:?}"),
            }
        }

        let rs = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ground_truth.rs"
        ))
        .expect("tests/ground_truth.rs must be present to check the truth table");

        for (name, truth) in [
            ("4656x3522-1", FIELD1_TRUTH),
            ("4656x3522-2", FIELD2_TRUTH),
            ("1920x1080-1", NARROW1_TRUTH),
            ("1920x1080-2", NARROW2_TRUTH),
            ("platesolve-1", PLATESOLVE1_TRUTH),
        ] {
            let b = field_block(&rs, name);
            assert_eq!(number(&b, "ra_deg"), truth.ra_deg, "{name} ra drifted");
            assert_eq!(number(&b, "dec_deg"), truth.dec_deg, "{name} dec drifted");
            assert_eq!(
                number(&b, "scale_arcsec"),
                truth.scale_arcsec,
                "{name} scale drifted"
            );
            assert_eq!(
                number(&b, "width_deg"),
                truth.width_deg,
                "{name} width drifted"
            );
            assert_eq!(
                number(&b, "height_deg"),
                truth.height_deg,
                "{name} height drifted"
            );
            assert_eq!(
                number(&b, "orient_deg"),
                truth.orient_deg,
                "{name} rotation drifted"
            );
            assert_eq!(
                flag(&b, "parity_neg"),
                truth.parity_neg,
                "{name} parity drifted"
            );
            assert_eq!(
                optional(&b, "tol_center_arcsec"),
                truth.tol_center_arcsec,
                "{name} center budget drifted"
            );
        }
    }
}
