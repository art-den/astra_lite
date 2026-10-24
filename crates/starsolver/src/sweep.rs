//! The index sweep: the object-count depth ramp, index selection by scale
//! range plus the `--ra/--dec` position gate, and `solve_field` - the round
//! loop that drives `solve_index` and keeps the deterministic winner of the
//! round that solves (the lexicographic-best `(log-odds, quad)` solved hit, so
//! the print does not depend on worker scheduling).

use anyhow::Result;
use std::time::Instant;

use crate::cli::SolveOptions;
use crate::healpix;
use crate::index::{Index, IndexMeta};
use crate::params::MAX_OBJS;
use crate::refine::refine_wcs;
use crate::solver::{FieldSpec, Flow, PositionHint, QuadHit, solve_index};
use crate::verify::{VerifyResult, verify_hit};
use crate::wcs::pixel_center_of_size;
use crate::xyls::Star;

/// A solved field: the values of the success block `output::write_solution`
/// prints (center, sizes, rotation, parity), plus the identity of the hit that
/// produced them (never printed).
///
/// `winner` is what lets the sweep's tests re-run the index that actually
/// solved a field instead of hardcoding an index id: the suite then works with
/// any installed index set, not one specific index file.
#[derive(Debug, Clone)]
pub struct Solution {
    pub ra: f64,
    pub dec: f64,
    pub width_deg: f64,
    pub height_deg: f64,
    pub orient_deg: f64,
    pub parity_neg: bool,
    // Carried for the sweep's tests (which index/quad/round won); the shipped
    // print path never reads it. Kept `pub(crate)` so re-exporting `Solution`
    // at the crate root does not drag `Winner` into the public API; the lint
    // is allowed because only tests read it.
    #[allow(dead_code)]
    pub(crate) winner: Winner,
}

/// Which index and quad the sweep reported as solved, the log-odds the winner
/// was picked by ([`better_hit`] defines that order), and the depth-round window
/// that produced it. The window lets the tests replay the exact winning round
/// (`testutil::FieldRun`) without re-deriving the ramp.
///
/// Every field is consumed only by the tests, not by the shipped print path
/// (see [`Solution::winner`] for why the lint is allowed here).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Winner {
    pub indexid: i64,
    /// `-1` for an all-sky index; the healpix cell number for a sub-region tile.
    pub healpix: i64,
    pub quad: usize,
    pub log_odds: f64,
    /// The winning round's `[obj_start, max_objs)` object window.
    pub obj_start: usize,
    pub max_objs: usize,
}

/// Floor for `--cpulimit` when computing the deadline: `Instant + 0` would
/// expire immediately on the first check, a sub-millisecond budget is
/// meaningless anyway.
pub(crate) const MIN_CPULIMIT_S: f64 = 0.001;

/// Index selection for the sweep: an index is kept when its quad-size band
/// overlaps the one the field can produce (`IndexMeta::fits_scale`) AND - when
/// a `--ra/--dec` hint is given - its sky cell passes the healpix tile position
/// gate ([`tile_in_hint`]). `index::load_metas_in` hands the metas over in
/// sweep order (largest index quad size first) and `filter` preserves it, so
/// the result is still in sweep order. Shared with `testutil::discover`, which
/// runs the production sweep on installed indices.
pub(crate) fn select_indices(metas: &[IndexMeta], opts: &SolveOptions) -> Vec<IndexMeta> {
    metas
        .iter()
        .filter(|m| m.fits_scale(opts.app_min, opts.app_max, opts.width, opts.height))
        .filter(|m| tile_in_hint(m, opts.radecs))
        .cloned()
        .collect()
}

/// The healpix tile position gate: an all-sky index is always in range; a
/// healpix sub-region tile is tried only when its sky cell overlaps the
/// `--ra/--dec` hint circle. With no hint (blind solve) every scale-fitting
/// tile is tried: the gate is a performance filter, never a correctness one
/// (`healpix::tile_overlaps_circle` errs towards keeping).
fn tile_in_hint(meta: &IndexMeta, radecs: Option<PositionHint>) -> bool {
    match (meta.healpix >= 0, radecs) {
        (true, Some(h)) => healpix::tile_overlaps_circle(
            meta.healpix,
            meta.hpnside,
            h.ra_deg,
            h.dec_deg,
            h.radius_deg,
        ),
        _ => true,
    }
}

/// Timeout message for the sweep: prints the effective (floored) limit - the
/// same value `run_into` built the deadline from - not the raw `--cpulimit`
/// argument, so `--cpulimit 0` reads `cpulimit 0.001 s`.
fn timeout_msg(cpulimit: f64) -> String {
    format!(
        "Timeout (cpulimit {} s) reached; field not solved",
        f64::max(cpulimit, MIN_CPULIMIT_S)
    )
}

/// Step of the depth ramp: sweep every selected index over field stars 1-10,
/// then 11-20, 21-30, ..., stopping at the first round + index that verifies a
/// solved hit. The ramp is capped at `MAX_OBJS` (frozen project constant) and
/// at the length of the star list.
///
/// Shallow-first is what makes the ramp useful: a round enumerates the quads
/// whose newest star falls inside its window, and that count grows with the
/// top of the window, so a coarse first round (say 1-100) is deep work on the
/// first index tried, while stepping by 10 gives every index its bright stars
/// before any index starts spending the whole `--cpulimit` on faint ones.
pub(crate) const OBJ_ROUND_STEP: usize = 10;

/// The round's replace rule: the lexicographic-best `(log-odds, quad)` solved
/// hit of a round wins; the quad tie-break keeps even exact float ties
/// deterministic, so the winner never depends on the order in which worker
/// threads report their hits.
fn better_hit(candidate: (f64, usize), current: Option<(f64, usize)>) -> bool {
    match current {
        None => true,
        Some(cur) => candidate > cur,
    }
}

/// Loop-local constants of one index round's capture, so [`RoundCapture`]
/// needs neither the `SolveOptions` nor the loop-local `meta`/round window on
/// every `collect` call.
#[derive(Clone, Copy)]
struct CaptureCtx {
    indexid: i64,
    healpix: i64,
    obj_start: usize,
    max_objs: usize,
    width: f64,
    height: f64,
    verbose: bool,
    radecs: Option<PositionHint>,
}

/// The winner capture for one index's round (`solve_field`'s `on_hit`).
///
/// [`collect`](Self::collect) takes an already-verified hit; `verify_hit` stays
/// in the `solve_field` glue (it needs the `&Index`, but is not part of the
/// capture semantics under test). `collect` runs the refine and the
/// field-center recentring on the worker thread BEFORE the winner lock (CPU
/// work is never serialized across rayon), applies the hint gate on the refined
/// center, and does the lexicographic compare-and-replace as ONE atomic
/// operation under the slot lock. It always returns [`Flow::Continue`]: the
/// round runs to its end, so stdout does not depend on rayon scheduling.
///
/// The glue closure is `Sync` and shares one capture across workers, so the two
/// pieces of mutable state are `Mutex`-guarded; `panic = "abort"` makes
/// poisoning unobservable.
struct RoundCapture<'a> {
    /// The field list: `refine_wcs` matches index stars against it.
    stars: &'a [Star],
    ctx: CaptureCtx,
    /// Running max of `vr.bestlogodds` over EVERY verified hit of the round
    /// (updated before the solved check, so the verbose `best logodds` line is
    /// right even when nothing solves).
    best_logodds: std::sync::Mutex<f64>,
    /// The lexicographic-best `(log-odds, quad)` solved hit captured so far.
    winner: std::sync::Mutex<Option<(f64, usize, Solution)>>,
}

impl RoundCapture<'_> {
    fn new(stars: &[Star], ctx: CaptureCtx) -> RoundCapture<'_> {
        RoundCapture {
            stars,
            ctx,
            best_logodds: std::sync::Mutex::new(f64::NEG_INFINITY),
            winner: std::sync::Mutex::new(None),
        }
    }

    /// Folds one verified hit into the capture: running `best_logodds` for every
    /// hit, then - only for a solved hit - refine + recentre (off-lock), the
    /// hint gate on the refined center, and the lexicographic replace. Always
    /// `Flow::Continue` (the production callback never stops the round).
    fn collect(&self, index: &Index, hit: &QuadHit, vr: VerifyResult) -> Flow {
        {
            let mut best = self.best_logodds.lock().unwrap();
            if vr.bestlogodds > *best {
                *best = vr.bestlogodds;
            }
        }
        if !vr.solved() {
            return Flow::Continue;
        }
        // Post-process on the worker thread, before any slot lock: a failed
        // refine keeps the raw WCS; then report in the field-center tangent
        // frame.
        let (w, h) = (self.ctx.width, self.ctx.height);
        let refined = refine_wcs(index, hit, self.stars, w, h, self.ctx.verbose).unwrap_or(hit.wcs);
        let report = refined.recentered_at([pixel_center_of_size(w), pixel_center_of_size(h)]);
        let (ra, dec) = (report.crval_deg[0], report.crval_deg[1]);
        if let Some(hint) = self.ctx.radecs {
            // The post-fit half of the hint gate: the finished solution, at
            // its refined center, must lie inside the hint circle as well (the
            // pre-fit half is the matched-star test in `solver.rs`).
            if !hint.contains(ra, dec) {
                if self.ctx.verbose {
                    eprintln!(
                        "[verify] solution at ({ra:.4},{dec:.4}) outside --ra/--dec circle, ignoring"
                    );
                }
                return Flow::Continue;
            }
        }
        let scale_deg = report.arcsec_per_pix() / 3600.0;
        let sol = Solution {
            ra,
            dec,
            width_deg: w * scale_deg,
            height_deg: h * scale_deg,
            orient_deg: report.orientation_deg(),
            parity_neg: report.parity_is_neg(),
            winner: Winner {
                indexid: self.ctx.indexid,
                healpix: self.ctx.healpix,
                quad: hit.quad,
                log_odds: vr.bestlogodds,
                obj_start: self.ctx.obj_start,
                max_objs: self.ctx.max_objs,
            },
        };
        // Poison-safe replace under the lock: keep the lexicographic-best
        // (log-odds, quad) solved hit of this index's round.
        let mut slot = self
            .winner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if better_hit(
            (vr.bestlogodds, hit.quad),
            slot.as_ref().map(|(l, q, _)| (*l, *q)),
        ) {
            *slot = Some((vr.bestlogodds, hit.quad, sol));
        }
        Flow::Continue
    }

    /// Post-round take: `(best_logodds, Option<(winner quad, Solution)>)`. The
    /// `[index N] ... SOLVED (quad Q) ... best logodds X` verbose line and the
    /// timeout precedence are rebuilt from exactly this.
    fn into_parts(self) -> (f64, Option<(usize, Solution)>) {
        let best_logodds = *self.best_logodds.lock().unwrap();
        let winner = self
            .winner
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (best_logodds, winner.map(|(_, quad, s)| (quad, s)))
    }
}

/// The index sweep: rounds of field depth outer, one index at a time inside
/// it. `selected` is swept as given, so the caller must hand it over in sweep
/// order ([`select_indices`]: largest index quad size first).
///
/// The first round + index that produces a solved hit stops the sweep, but
/// that round still runs to its end: every hit the winning index produces is
/// verified, and the reported solution is the lexicographic-best
/// (log-odds, quad) solved hit of that index's capture, so stdout does not
/// depend on rayon scheduling - unless `--cpulimit` truncates the round, which
/// is timing-dependent by design.
pub(crate) fn solve_field(
    opts: &SolveOptions,
    stars: &[Star],
    selected: &[IndexMeta],
    deadline: Instant,
) -> Result<Option<Solution>> {
    let (w, h) = (opts.width, opts.height);
    let depth_cap = usize::min(stars.len(), MAX_OBJS);
    let mut lo = 0usize;
    while lo < depth_cap {
        let hi = usize::min(lo + OBJ_ROUND_STEP, depth_cap);
        if opts.verbose {
            eprintln!("[round] field objects {}-{}", lo + 1, hi);
        }
        for meta in selected {
            if Instant::now() >= deadline {
                eprintln!("{}", timeout_msg(opts.cpulimit));
                return Ok(None);
            }
            if opts.verbose {
                eprintln!("[index] trying {}", meta.describe());
            }
            // Loading once per round is intentional: only one index is held in
            // memory at a time (the < 200 MB / 2 GB target).
            let index = Index::load(&meta.path)?;
            let spec = FieldSpec {
                stars,
                width: w,
                height: h,
                app_min: opts.app_min,
                app_max: opts.app_max,
                radecs: opts.radecs,
                max_objs: hi,
                obj_start: lo,
                verbose: opts.verbose,
                deadline: Some(deadline),
            };
            // `solve_index` runs on the rayon pool: the callback is `Sync`
            // and guards its mutable state itself. The winner capture holds
            // the round's mutable state; `verify_hit` stays in this glue (it
            // needs `&index`), the capture composes refine + recentre, the
            // hint gate and the atomic winner replace (see `RoundCapture`).
            let capture = RoundCapture::new(
                stars,
                CaptureCtx {
                    indexid: meta.indexid,
                    healpix: meta.healpix,
                    obj_start: lo,
                    max_objs: hi,
                    width: w,
                    height: h,
                    verbose: opts.verbose,
                    radecs: opts.radecs,
                },
            );
            let stats = solve_index(&index, &spec, &|hit| {
                let vr = verify_hit(&index, hit, stars, w, h);
                if opts.verbose {
                    eprintln!(
                        "[verify] index {} quad {}: logodds {:.3}, {} match, {} ref, {} test{}",
                        meta.indexid,
                        hit.quad,
                        vr.bestlogodds,
                        vr.nmatch,
                        vr.nr,
                        vr.nt,
                        if vr.bailed { ", BAILED" } else { "" }
                    );
                }
                capture.collect(&index, hit, vr)
            });
            let (best_logodds, winner) = capture.into_parts();
            if opts.verbose {
                eprintln!(
                    "[index {}] {}: {} queries, {} code hits, {} verified hits{}",
                    meta.indexid,
                    match &winner {
                        Some((quad, _)) => format!("SOLVED (quad {quad})"),
                        None if stats.timed_out => "timeout".to_string(),
                        None => "no solution".to_string(),
                    },
                    stats.queries,
                    stats.code_hits,
                    stats.hits,
                    if best_logodds.is_finite() {
                        format!(", best logodds {best_logodds:.3}")
                    } else {
                        String::new()
                    }
                );
            }
            let solution = winner.map(|(_, s)| s);
            if stats.timed_out {
                // With parallel workers the deadline can trip while another
                // worker has just captured a solution: report it properly.
                if solution.is_none() {
                    eprintln!("{}", timeout_msg(opts.cpulimit));
                }
                return Ok(solution);
            }
            if solution.is_some() {
                return Ok(solution);
            }
        }
        lo = hi;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ScaleUnits, parse_options};
    use crate::coords::deg2arcsec;
    use crate::index;
    use crate::params::{
        self, CPU_LIMIT_SECS, DEFAULT_RADECS_RADIUS_DEG, MAX_WIDTH_DEG, MIN_WIDTH_DEG,
    };
    use crate::testutil::{FIELD2_TRUTH, NARROW1_TRUTH, NARROW2_TRUTH};
    use crate::wcs::Wcs;
    use std::time::Duration;

    /// Invariant: with the frame size given and no angular size, the scale
    /// half of index selection keeps every installed index. The blind 0.1-180
    /// deg field-width range is a field quad band of 27.2 arcsec to 225.7 deg
    /// of backbone on this 4656x3522 frame (`IndexMeta::fits_scale`), so an
    /// index would need its whole quad band outside that span to be dropped.
    /// An installed set is not ours to enumerate, so the assertion is an
    /// invariant, never a count: nothing dropped, and the sweep order survives
    /// the filter (`metas` arrives in `sweep_cmp` order and filtering keeps it).
    #[test]
    fn blind_scale_range_keeps_every_installed_index() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        // The blind scale range of a 4656x3522 field: no --scale-low/--scale-high.
        let (w, h) = (4656.0, 3522.0);
        let app_min = deg2arcsec(MIN_WIDTH_DEG) / w;
        let app_max = deg2arcsec(MAX_WIDTH_DEG) / w;
        let mut selected: Vec<&IndexMeta> = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        for meta in metas {
            if meta.fits_scale(app_min, app_max, w, h) {
                selected.push(meta);
            } else {
                dropped.push(meta.describe());
            }
        }
        assert_eq!(
            selected.len(),
            metas.len(),
            "the blind range dropped indices: {dropped:?}"
        );
        for pair in selected.windows(2) {
            assert_ne!(
                index::sweep_cmp(pair[0], pair[1]),
                std::cmp::Ordering::Greater,
                "selection broke the sweep order: {} then {}",
                pair[0].describe(),
                pair[1].describe()
            );
        }
    }

    /// A deadline that has already passed must make the sweep report "not
    /// solved" before any index is loaded (the `--cpulimit` deadline check).
    /// Hermetic by design: the metas are header stubs, so no index file is read
    /// at all, and the field file only has to supply a non-empty star list - an
    /// empty one leaves the round loop without ever reaching the deadline check.
    #[test]
    fn solve_field_honors_a_past_deadline() {
        let Some(stars) = crate::testutil::load_field("4656x3522-1.xyls") else {
            return;
        };
        let metas = [
            crate::testutil::meta_stub(4213, -1, 0),
            crate::testutil::meta_stub(4206, 2, 1),
        ];
        let opt = parse_options(&[]).unwrap();
        let solution = solve_field(&opt, &stars, &metas, Instant::now()).unwrap();
        assert!(solution.is_none(), "past deadline must solve nothing");
    }

    // ---- Deterministic round winner -----------------------------------------

    /// The winner of a round is the lexicographic-best `(log-odds, quad)`
    /// solved hit, independent of arrival order; exact float ties resolve to
    /// the larger quad id.
    #[test]
    fn better_hit_is_lexicographic_and_order_independent() {
        // Two solved hits whose log-odds differ: the higher one must win in
        // either arrival order.
        let a = (342.720f64, 88080usize);
        let b = (321.500, 69298);
        for order in [[a, b], [b, a]] {
            let mut best = None;
            for c in order {
                if better_hit(c, best) {
                    best = Some(c);
                }
            }
            assert_eq!(best, Some(a), "arrival order {order:?}");
        }
        let t1 = (100.0f64, 5usize);
        let t2 = (100.0, 9);
        for order in [[t1, t2], [t2, t1]] {
            let mut best = None;
            for c in order {
                if better_hit(c, best) {
                    best = Some(c);
                }
            }
            assert_eq!(best, Some(t2), "exact tie, arrival order {order:?}");
        }
    }

    // ---- RoundCapture: the winner slot, exercised directly ------------------

    /// A synthetic TAN WCS whose field-center sky position is near (10, 0) deg.
    /// Its refine outcome is irrelevant to these assertions: the winner slot
    /// keys on `(log-odds, quad)`, and the far-hint gate is far enough that no
    /// similarity tweak can land the center inside it either way.
    fn synthetic_wcs() -> Wcs {
        let scale = 1.0 / 3600.0; // 1 arcsec/px, deg/px
        Wcs {
            crpix: [50.0, 50.0],
            crval_deg: [10.0, 0.0],
            cd: [[-scale, 0.0], [0.0, scale]],
        }
    }

    fn capture_ctx(radecs: Option<PositionHint>) -> CaptureCtx {
        CaptureCtx {
            indexid: 1,
            healpix: -1,
            obj_start: 0,
            max_objs: 10,
            width: 100.0,
            height: 100.0,
            verbose: false,
            radecs,
        }
    }

    /// A hit whose `field_stars` index the 4-star field below and whose `quad`
    /// is a valid record of the loaded index (the caller keeps `quad < nquads`).
    fn synth_hit(quad: usize) -> QuadHit {
        QuadHit {
            quad,
            code_dist2: 0.0,
            field_stars: [0, 1, 2, 0],
            wcs: synthetic_wcs(),
        }
    }

    /// A `VerifyResult` with the given log-odds; `solved()` keys on the log-odds
    /// alone, so the match counts are placeholders here.
    fn vr_with(logodds: f64) -> VerifyResult {
        VerifyResult {
            bestlogodds: logodds,
            nmatch: 10,
            nr: 100,
            nt: 100,
            bailed: false,
        }
    }

    /// The 4-star field every synthetic hit indexes (its `field_stars` values
    /// are all valid indices into it).
    fn synth_field() -> Vec<Star> {
        [(10.0, 10.0), (50.0, 20.0), (80.0, 60.0), (30.0, 90.0)]
            .iter()
            .map(|&(x, y)| Star { x, y, flux: 1.0 })
            .collect()
    }

    /// (a) The capture's winner is the lexicographic-best `(log-odds, quad)`
    /// solved hit, independent of the arrival order of `collect` (the property
    /// `solve_field` relies on, tested on the capture directly). No hint, so
    /// the gate never fires and the winner is the pure argmax.
    #[test]
    fn round_capture_winner_is_order_independent() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        let hi = params::ODDS_TO_SOLVE_LN + 20.0;
        let lo = params::ODDS_TO_SOLVE_LN + 5.0;
        for order in [[(hi, 0usize), (lo, 1usize)], [(lo, 1), (hi, 0)]] {
            let capture = RoundCapture::new(&stars, capture_ctx(None));
            for &(logodds, quad) in &order {
                assert_eq!(
                    capture.collect(&index, &synth_hit(quad), vr_with(logodds)),
                    Flow::Continue
                );
            }
            let (_best, winner) = capture.into_parts();
            let (wquad, sol) = winner.expect("a solved hit must be captured");
            assert_eq!(
                wquad, 0,
                "the higher-log-odds quad must win, arrival order {order:?}"
            );
            assert_eq!(sol.winner.log_odds, hi);
        }
    }

    /// (b) An exact float tie resolves to the larger quad id (the second half
    /// of the lexicographic key), regardless of arrival order.
    #[test]
    fn round_capture_breaks_float_ties_by_quad() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        let tie = params::ODDS_TO_SOLVE_LN + 10.0;
        for order in [[(tie, 0usize), (tie, 1usize)], [(tie, 1), (tie, 0)]] {
            let capture = RoundCapture::new(&stars, capture_ctx(None));
            for &(logodds, quad) in &order {
                assert_eq!(
                    capture.collect(&index, &synth_hit(quad), vr_with(logodds)),
                    Flow::Continue
                );
            }
            let (wquad, _sol) = capture
                .into_parts()
                .1
                .expect("a solved hit must win on an exact tie");
            assert_eq!(
                wquad, 1,
                "an exact log-odds tie resolves to the larger quad, order {order:?}"
            );
        }
    }

    /// (c) A solved hit whose (refined) center falls outside the hint circle
    /// never enters the winner slot and never stops the round; the identical
    /// hits with no hint do get captured, so the drop is the gate's doing.
    #[test]
    fn round_capture_hint_gate_drops_far_solutions() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        // The synthetic center is near (10, 0) deg; a 1 deg circle at (180, -80)
        // cannot contain it whether refine keeps or replaces the WCS.
        let far = Some(PositionHint {
            ra_deg: 180.0,
            dec_deg: -80.0,
            radius_deg: 1.0,
        });
        let hits = [synth_hit(0), synth_hit(1)];
        let capture = RoundCapture::new(&stars, capture_ctx(far));
        for hit in &hits {
            assert_eq!(
                capture.collect(&index, hit, vr_with(params::ODDS_TO_SOLVE_LN + 50.0)),
                Flow::Continue,
                "a gated hit must not stop the round"
            );
        }
        assert!(
            capture.into_parts().1.is_none(),
            "a solution outside the hint circle must never enter the slot"
        );

        // Control: the same solved hits DO enter with no hint.
        let open = RoundCapture::new(&stars, capture_ctx(None));
        for hit in &hits {
            assert_eq!(
                open.collect(&index, hit, vr_with(params::ODDS_TO_SOLVE_LN + 50.0)),
                Flow::Continue
            );
        }
        assert!(
            open.into_parts().1.is_some(),
            "control: the identical solved hits capture with no hint"
        );
    }

    /// (d) `best_logodds` is a running max over EVERY hit, reached before the
    /// solved check: hits below the solve gate update it (and drive the verbose
    /// `best logodds` line) yet never enter the winner slot.
    #[test]
    fn round_capture_tracks_best_logodds_of_unsolved_hits() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        let capture = RoundCapture::new(&stars, capture_ctx(None));
        let a = params::ODDS_TO_SOLVE_LN - 1.0;
        let b = params::ODDS_TO_SOLVE_LN - 50.0;
        for (logodds, quad) in [(b, 0usize), (a, 1usize)] {
            assert_eq!(
                capture.collect(&index, &synth_hit(quad), vr_with(logodds)),
                Flow::Continue
            );
        }
        let (best, winner) = capture.into_parts();
        assert_eq!(best, a, "best_logodds must reflect unsolved hits");
        assert!(
            winner.is_none(),
            "an unsolved hit must not enter the winner slot"
        );
    }

    /// The round reports the lexicographic-best `(log-odds, quad)` solved hit:
    /// the callback always continues, so the round runs to its end and its hit
    /// set does not depend on scheduling, and the sweep reports exactly that
    /// hit. Replaying the round that wins `4656x3522-2` twice must therefore
    /// give the same winner twice.
    ///
    /// No quad id or log-odds value is pinned here on purpose: which quad wins
    /// is a property of the installed index set, the rule that picks it is ours.
    #[test]
    fn round_winner_is_the_best_solved_hit_of_the_round() {
        let Some(run) = crate::testutil::field_run(FIELD2, 4656.0, 3522.0, None) else {
            return;
        };
        // The hit the production sweep reported.
        let reported = (run.log_odds, run.quad);
        // Two replays of the winning round: each must contain the reported
        // winner again (a replay runs the round with the same parameters, so
        // losing it here would be a real regression) and must pick the same
        // argmax over the round's solved hits, which is also the reported one.
        let mut argmaxes: Vec<(f64, usize)> = Vec::new();
        for _ in 0..2 {
            let hits = crate::testutil::solved_hits(run);
            assert!(
                !hits.is_empty(),
                "the winning round of {} replayed no solved hit",
                run.field
            );
            let winner = hits
                .iter()
                .find(|h| h.hit.quad == run.quad)
                .unwrap_or_else(|| {
                    panic!(
                        "quad {} is not a solved hit when its own round is replayed",
                        run.quad
                    )
                });
            assert_eq!(
                (winner.verify.bestlogodds, winner.hit.quad),
                reported,
                "the winner is not the same hit when its own round is replayed"
            );
            let mut best: Option<(f64, usize)> = None;
            for h in &hits {
                let candidate = (h.verify.bestlogodds, h.hit.quad);
                if better_hit(candidate, best) {
                    best = Some(candidate);
                }
            }
            argmaxes.push(best.unwrap());
        }
        assert_eq!(
            argmaxes[0], argmaxes[1],
            "two replays of the same round picked different winners"
        );
        assert_eq!(
            reported, argmaxes[0],
            "the sweep did not report the better_hit argmax of its round"
        );
        assert!(
            reported.0 >= params::ODDS_TO_SOLVE_LN,
            "the reported winner is below the solve gate: {reported:?}"
        );
    }

    // ---- End-to-end prints: the sky truth ---------------------------------

    /// Field files of the print gates. Their sky truth lives in
    /// `testutil` (`FIELD2_TRUTH`, `NARROW1_TRUTH`, `NARROW2_TRUTH`);
    /// `testutil::Truth::check` gates the print against it with the budgets
    /// `TOL_CENTER_PX`, `TOL_ORIENT_DEG`, `TOL_SCALE_FRAC`, `TOL_SIZE_FRAC`.
    const FIELD2: &str = "4656x3522-2.xyls";
    const NARROW1: &str = "1920x1080-1.xyls";
    const NARROW2: &str = "1920x1080-2.xyls";

    /// The narrow fields' plate scale, as the `--scale-units arcsecperpix
    /// --scale-low 0.95 --scale-high 1.05` hint these runs are swept with: a
    /// band around the 0.9966 and 0.9971 arcsec/px the two truth entries
    /// record.
    const NARROW_SCALE: (f64, f64) = (0.95, 1.05);

    /// Depth cap of the far-hint legs: 40 stars = 4 rounds of the production
    /// ramp ([`OBJ_ROUND_STEP`]). With the full star list an unsolvable hint
    /// sends the ramp to `MAX_OBJS` over every all-sky index, which is a
    /// batch-job cost, not a unit-test one.
    const SHALLOW_OBJS: usize = 40;

    /// Offset of the far-hint legs, in degrees of declination from the field's
    /// published center. Even the widest gated field (3.5 x 2.7 deg) then
    /// has its nearest frame corner 18.7 deg from the hint center: clear of the
    /// default 10 deg `--radius` circle by 8.7 deg, and of the near radius
    /// below by even more.
    const FAR_HINT_OFFSET_DEG: f64 = 20.0;

    /// Near-hint radius for field 2: its farthest frame corner is 2.20 deg from
    /// the field center, so a 2.5 deg circle covers the whole 3.5 x 2.7 deg
    /// field and neither the pre-fit matched-star gate in
    /// `solver.rs::process_hit` nor the post-fit center gate in
    /// `RoundCapture::collect` can drop the solution.
    const NEAR_HINT_RADIUS_DEG: f64 = 2.5;

    /// `SolveOptions` for a field file and frame: `scale` is the
    /// `--scale-units arcsecperpix` low/high pair (`None` is the blind 0.1-180
    /// deg field-width range), `hint` the `--ra/--dec/--radius` position hint.
    fn solve_opts(
        field: &str,
        width: f64,
        height: f64,
        scale: Option<(f64, f64)>,
        hint: Option<PositionHint>,
    ) -> SolveOptions {
        let (app_min, app_max) = match scale {
            Some((lo, hi)) => (lo, hi),
            None => (
                deg2arcsec(MIN_WIDTH_DEG) / width,
                deg2arcsec(MAX_WIDTH_DEG) / width,
            ),
        };
        SolveOptions {
            file: crate::testutil::testdata_path(field),
            width,
            height,
            app_min,
            app_max,
            radecs: hint,
            cpulimit: CPU_LIMIT_SECS,
            verbose: false,
            units: ScaleUnits::DegWidth,
        }
    }

    /// Wall-clock budget of the end-to-end gates: generous against the
    /// seconds these fields take on an installed set.
    fn test_deadline() -> Instant {
        Instant::now() + Duration::from_secs(240)
    }

    /// A `--ra/--dec` circle that does not contain the field must report nothing.
    /// The control leg (no hint) sweeps the installed set and must solve; the far
    /// leg then runs on exactly the index the control won - same index, same
    /// depth, only the hint moved - so what is under test is the gate, and its
    /// cost is one index rather than a sweep over the whole install.
    ///
    /// Both legs are depth-capped (see [`SHALLOW_OBJS`]): uncapped, an unsolvable
    /// hint sends the ramp to `MAX_OBJS` over every all-sky index, which is a
    /// batch-job cost, not a unit-test one.
    fn far_hint_reports_nothing(
        metas: &[IndexMeta],
        stars: &[crate::xyls::Star],
        field: &str,
        width: f64,
        height: f64,
        scale: Option<(f64, f64)>,
        center: (f64, f64),
    ) {
        let shallow = &stars[..usize::min(stars.len(), SHALLOW_OBJS)];
        let control = solve_field(
            &solve_opts(field, width, height, scale, None),
            shallow,
            metas,
            test_deadline(),
        )
        .unwrap();
        let Some(control) = control else {
            panic!(
                "{field} must still solve at depth {} without a hint, or the far-hint \
                 `None` below proves nothing",
                shallow.len()
            );
        };
        // `FAR_HINT_OFFSET_DEG` of declination away with the default radius:
        // the field lies clear of the circle.
        let far = PositionHint {
            ra_deg: center.0,
            dec_deg: center.1 + FAR_HINT_OFFSET_DEG,
            radius_deg: DEFAULT_RADECS_RADIUS_DEG,
        };
        let won: Vec<IndexMeta> = metas
            .iter()
            .filter(|m| m.indexid == control.winner.indexid && m.healpix == control.winner.healpix)
            .cloned()
            .collect();
        assert!(
            !won.is_empty(),
            "the index the control leg won is not in the sweep set: {:?}",
            control.winner
        );
        let sol = solve_field(
            &solve_opts(field, width, height, scale, Some(far)),
            shallow,
            &won,
            test_deadline(),
        )
        .unwrap();
        assert!(
            sol.is_none(),
            "index {} solved {field} under a hint at {far:?}: winner {:?}",
            control.winner.indexid,
            sol.map(|s| s.winner)
        );
    }

    /// End-to-end gate, anchored on the sky: the production sweep run
    /// blind over the whole installed set must print field 2's published
    /// center/size/rotation/parity within the budgets of
    /// `testutil::Truth::check`. Which index wins is whatever the installed set
    /// offers - the print is a property of the sky, not of one index file, and
    /// that is the point: no index id, quad id or per-index fixed point is
    /// pinned here.
    #[test]
    fn blind_field2_print_matches_sky_truth() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(FIELD2) else {
            return;
        };
        let opts = solve_opts(FIELD2, 4656.0, 3522.0, None, None);
        let sol = solve_field(&opts, &stars, metas, test_deadline())
            .unwrap()
            .unwrap_or_else(|| panic!("{FIELD2} must solve against the installed set"));
        FIELD2_TRUTH.check(&sol, 4656.0);
        assert!(
            sol.winner.log_odds >= params::ODDS_TO_SOLVE_LN,
            "the reported winner is below the solve gate: {:?}",
            sol.winner
        );
    }

    /// The hint gate end to end on field 2, with the hint built at runtime from
    /// the field's published center: a circle around it ([`NEAR_HINT_RADIUS_DEG`],
    /// covering the whole field) still reports the solution; a circle
    /// [`FAR_HINT_OFFSET_DEG`] deg away reports nothing.
    #[test]
    fn hint_on_the_true_center_reports_and_a_far_hint_does_not() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(FIELD2) else {
            return;
        };
        let center = (FIELD2_TRUTH.ra_deg, FIELD2_TRUTH.dec_deg);
        let near = Some(PositionHint {
            ra_deg: center.0,
            dec_deg: center.1,
            radius_deg: NEAR_HINT_RADIUS_DEG,
        });
        let sol = solve_field(
            &solve_opts(FIELD2, 4656.0, 3522.0, None, near),
            &stars,
            metas,
            test_deadline(),
        )
        .unwrap()
        .expect("a hint at the true center must keep the solution (final gate)");
        FIELD2_TRUTH.check(&sol, 4656.0);

        far_hint_reports_nothing(metas, &stars, FIELD2, 4656.0, 3522.0, None, center);
    }

    // ---- Healpix tile selection: the hint gate ------------------------------

    /// All-sky metas are never gated; a tile is tried blind, kept when its
    /// cell overlaps the hint circle, and pruned by a far hint. The pixel
    /// numbers are the astrometry.net xy scheme (`healpix.rs`): cell 2 of
    /// nside 1 - the north cap centered at RA 225 deg, dec +41.8 deg - is the
    /// cell that contains both narrow field centers, and `near` below is
    /// `1920x1080-1`'s published center.
    #[test]
    fn tile_in_hint_gates_tiles_not_allsky() {
        let allsky = crate::testutil::meta_stub(4213, -1, 0);
        let tile = crate::testutil::meta_stub(4206, 2, 1);
        let far = Some(PositionHint {
            ra_deg: 20.0,
            dec_deg: -30.0,
            radius_deg: 10.0,
        });
        let near = Some(PositionHint {
            ra_deg: 202.484,
            dec_deg: 47.232,
            radius_deg: 10.0,
        });
        assert!(tile_in_hint(&allsky, far), "all-sky is always in range");
        assert!(tile_in_hint(&allsky, near));
        assert!(tile_in_hint(&tile, None), "blind mode tries every tile");
        assert!(tile_in_hint(&tile, near), "the tile containing the hint");
        assert!(!tile_in_hint(&tile, far), "a far hint prunes the tile");
    }

    /// Gate: both `1920x1080-*` narrow fields solve end to end over the
    /// installed set at their own ~1 arcsec/px scale, and the print lands on the
    /// published sky truth within the budgets of `testutil::Truth::check`. Those
    /// two truth entries trade the pixel center budget for an absolute 2.0
    /// arcsec one, looser than 1.5 px at their scale: the published center is a
    /// plate fit that carries a distortion term, and our refine is
    /// similarity-only, so it cannot follow that term.
    ///
    /// The sweep is run with the scale hint ([`NARROW_SCALE`]), which is what
    /// keeps selection on the fine-scale indices - and what a consumer of a
    /// narrow field actually knows, so it is the honest input.
    ///
    /// Self-skips when the installed set has no `dimquads = 4` index
    /// (`testutil::dim4_index`): which indices a machine has is not ours to
    /// decide, so an install without the 4-star code path skips instead of
    /// failing.
    #[test]
    fn narrow_fields_solve_with_the_scale_hint() {
        if crate::testutil::dim4_index().is_none() {
            return;
        }
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        for (field, truth) in [(NARROW1, &NARROW1_TRUTH), (NARROW2, &NARROW2_TRUTH)] {
            let Some(stars) = crate::testutil::load_field(field) else {
                return;
            };
            let opts = solve_opts(field, 1920.0, 1080.0, Some(NARROW_SCALE), None);
            let sol = solve_field(&opts, &stars, metas, test_deadline())
                .unwrap()
                .unwrap_or_else(|| panic!("{field} must solve at its own scale"));
            truth.check(&sol, 1920.0);
            assert!(
                sol.winner.log_odds >= params::ODDS_TO_SOLVE_LN,
                "{field}: winner below the solve gate: {:?}",
                sol.winner
            );
        }
    }

    /// The hint end to end on a narrow field: a hint at the published center
    /// keeps the tile that contains the field and still solves (print on
    /// truth); a circle [`FAR_HINT_OFFSET_DEG`] deg away leaves the field
    /// unsolved. Run with the same scale hint as
    /// [`narrow_fields_solve_with_the_scale_hint`].
    #[test]
    fn hint_gate_end_to_end_on_the_narrow_field() {
        if crate::testutil::dim4_index().is_none() {
            return;
        }
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(NARROW1) else {
            return;
        };
        let center = (NARROW1_TRUTH.ra_deg, NARROW1_TRUTH.dec_deg);
        let near = Some(PositionHint {
            ra_deg: center.0,
            dec_deg: center.1,
            radius_deg: DEFAULT_RADECS_RADIUS_DEG,
        });
        let opts = solve_opts(NARROW1, 1920.0, 1080.0, Some(NARROW_SCALE), near);
        let sol = solve_field(&opts, &stars, metas, test_deadline())
            .unwrap()
            .expect("a hint at the true center must still solve");
        NARROW1_TRUTH.check(&sol, 1920.0);

        far_hint_reports_nothing(
            metas,
            &stars,
            NARROW1,
            1920.0,
            1080.0,
            Some(NARROW_SCALE),
            center,
        );
    }
}
