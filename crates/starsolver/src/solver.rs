//! Solve loop: field quad codes -> codes kd-tree search -> hit filtering -> WCS
//! fit; verification/odds in `verify.rs`, the depth-ramp sweep in `sweep.rs`,
//! stdout printing in `output.rs`.
//!
//! Enumeration is the minimal sufficient set ([`Searcher::triple`],
//! [`Searcher::quad`]): every choice of star C (`dimquads=3`) or ordered pair
//! (C, D) (`dimquads=4`), each x 2 parities - an index declares its
//! canonicalization in the header (`CXDX`, `CXDXLT1`) but has no key for parity,
//! so codes of either handedness occur - dropping the variants that index's
//! filters reject ([`Searcher::variants`], [`canonical_ok`]).
//!
//! A query's hits are processed in ascending code distance, so the callback sees
//! the closest code first. Enumeration is incremental: step `imax` covers exactly
//! the quads whose newest slot is `imax`, so a pass from `obj_start = 0` has
//! enumerated every quad formable from stars `0..=i` once step `i` is done (most
//! die at the |AB| or circle filters, never reaching the tree) - what lets a
//! shallow round solve a field whose quad is made of early stars, and what a
//! [`Flow::Stop`] callback cuts off.
//!
//! Within a depth step the enumeration runs on the rayon pool, one task per
//! backbone `(A, B)`; first-solution semantics is best-effort - after
//! [`Flow::Stop`] workers abandon their not-yet-started work.

use crate::codes::{canonical_ok, in_circle, parity_flip, swap_backbone};
use crate::coords::{arcsec2rad, chord2_to_rad, deg2distsq, distsq, radec_to_xyz};
use crate::index::{HintCodes, Index};
use crate::kdtree::Hit;
use crate::params::{
    CODE_TOL, CXDX_MARGIN_FACTOR, DEADLINE_CHECK_PERIOD, DEADLINE_EVENT_BATCH, MAX_OBJS, QSF_HI,
    QSF_LO,
};
use crate::wcs::{Wcs, fit_tan_wcs, pixel_center_of_size};
use crate::xyls::Star;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Callback flow for [`solve_index`]. Under the rayon pool `Stop` is
/// best-effort: it prevents not-yet-started work, but workers already inside a
/// hit may still call the callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "honor the callback flow: a discarded Stop can miss first-solution semantics"]
pub enum Flow {
    Continue,
    Stop,
}

/// The `--ra/--dec/--radius` position hint: a circle on the sky (`--radius`
/// falls back to `params::DEFAULT_RADECS_RADIUS_DEG`); the single source of the
/// chord²-vs-`deg2distsq` test behind the pre-fit solver-side gate and the
/// post-fit check of a finished solution.
///
/// [`contains`](Self::contains) is boundary-inclusive and the two sites that run
/// this test differ on NaN deliberately (see their docs). No `Eq`/`Hash` (the
/// `f64` fields implement neither on this toolchain); a caller that needs a hash
/// key takes their `to_bits` itself (`testutil::run_key`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionHint {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub radius_deg: f64,
}

impl PositionHint {
    /// `(hint center xyz, squared-chord radius)` - the ONE source of the
    /// chord²-vs-`deg2distsq` geometry, precomputed per searcher by
    /// `Searcher::new`. NaN handling is the CALLER's: the pre-fit gate rejects a
    /// NaN star distance.
    #[must_use]
    pub fn precompute(&self) -> ([f64; 3], f64) {
        (
            radec_to_xyz(self.ra_deg, self.dec_deg),
            deg2distsq(self.radius_deg),
        )
    }

    /// Post-fit solution gate: is a sky point inside the hint circle? The test is
    /// the negated rejection `!(distsq > deg2distsq(radius))`, so it is boundary
    /// inclusive and - crucially - a NaN distance is NOT rejected: a NaN center
    /// passes. The pre-fit gate rejects NaN instead (`Searcher::process_hit`); the
    /// two keep their NaN behavior on purpose and must not be merged across the
    /// xyz/radec boundary. That negation is why the partially-ordered-negation
    /// lint is allowed.
    #[must_use]
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    pub fn contains(&self, ra_deg: f64, dec_deg: f64) -> bool {
        !(distsq(
            &radec_to_xyz(ra_deg, dec_deg),
            &radec_to_xyz(self.ra_deg, self.dec_deg),
        ) > deg2distsq(self.radius_deg))
    }
}

/// Pre-fit hint test for ONE matched index star: is its unit direction outside
/// the circle of a precomputed hint (`PositionHint::precompute`)? A NaN distance
/// (a corrupt index star) counts as outside, which the bare `d > r2` comparison
/// would let through; [`PositionHint::contains`] accepts NaN on purpose, so the
/// two sites are not interchangeable. The per-hit gate (`Searcher::process_hit`)
/// and the per-file reachability scan (`index.rs::Index::hint_index`) both run
/// this one predicate, so a scan verdict can never disagree with the gate it
/// stands in for.
#[inline]
#[must_use]
pub(crate) fn star_outside_hint(xyz: &[f64; 3], hint: &([f64; 3], f64)) -> bool {
    let d = distsq(xyz, &hint.0);
    d > hint.1 || d.is_nan()
}

/// Everything about the field the solver sees.
pub struct FieldSpec<'a> {
    /// Field stars in the order the caller hands them over: on the CLI path the
    /// brightest `params::TOP_OBJS` descending by flux (`xyls::brightest_first`);
    /// callers that enter here directly (`testutil`, the unit tests) may still
    /// pass the uncapped `xyls::load` order, which is NOT flux-sorted. The
    /// enumeration sees only list position: slots are ordered by list rank, a
    /// quad's largest index is its "newest" star, and the depth rounds are windows
    /// over this order (`Star::flux` is never read here).
    pub stars: &'a [Star],
    pub width: f64,
    pub height: f64,
    /// Allowed image scale, arcsec/px: `--scale-low`/`--scale-high`, or the
    /// blind default (the frame width spanning 0.1 to 180 degrees).
    pub app_min: f64,
    pub app_max: f64,
    /// `--ra/--dec/--radius` hint, degrees.
    pub radecs: Option<PositionHint>,
    /// The sweep's compact hint codes tree for this `(index, hint)` pair
    /// (`index.rs`): the index's own codes tree restricted to the quads whose
    /// stars all lie inside the hint circle. Searching it instead of the full tree
    /// cannot change the hits that reach [`process_hit`], because the `radecs`
    /// gate there stays in force and rejects exactly the records the tree leaves
    /// out. `None` for a blind pass and for a hinted pass's first round, which has
    /// no tree yet.
    pub hint_codes: Option<&'a HintCodes>,
    /// Cap on the number of field stars this pass may use; `solve_index` clamps
    /// it further to `params::MAX_OBJS` (the emergency brake that keeps an
    /// unsolvable field from walking the whole list).
    pub max_objs: usize,
    /// First field star of the depth round: the sweep drives one pass per window
    /// of `OBJ_ROUND_STEP` stars (objs 1-10, then 11-20, ...). Only quads whose
    /// newest (largest-index) slot is >= `obj_start` are enumerated; older stars
    /// stay valid A/C/D slots, which makes consecutive windows an exact partition
    /// of the quad set (`tasks_for_imax`).
    pub obj_start: usize,
    pub verbose: bool,
    /// Cooperative `--cpulimit` deadline: the clock is sampled once per
    /// `DEADLINE_CHECK_PERIOD` globally counted enumeration iterations; once the
    /// flag trips only work already in flight runs, so the overshoot is the
    /// sampling lag plus those tasks.
    pub deadline: Option<std::time::Instant>,
    /// Cooperative cancel of a concurrent index attempt (`sweep.rs`): raised once
    /// some earlier index of the same round has captured a solution. Read as one
    /// relaxed load per [`Searcher::should_stop`] call and reported as
    /// [`SolveStats::cancelled`] - never as a timeout or a callback stop.
    pub cancel: Option<&'a AtomicBool>,
}

/// A code hit that survived the scale checks and got a WCS fit.
#[derive(Debug, Clone)]
pub struct QuadHit {
    /// Index quad number: the codes tree holds exactly `NQUADS` records (a
    /// mismatch is rejected when the index loads) and record `q` is quad `q`'s
    /// code, so a hit's record index addresses the `quads` table directly.
    pub quad: usize,
    /// Squared code-space distance to the field code.
    #[allow(dead_code)] // carried for external consumers; the verbose `[hit]`
    // line prints the square root of the `process_hit` parameter, not this field
    pub code_dist2: f64,
    /// Field star for each quad slot `(A, B, C[, D])`; for `dimquads=3` the 4th
    /// slot is an unused sentinel `0` (a valid star #0) that consumers never read.
    pub field_stars: [usize; 4],
    pub wcs: Wcs,
}

impl QuadHit {
    #[must_use]
    pub fn arcsec_per_pix(&self) -> f64 {
        self.wcs.arcsec_per_pix()
    }

    /// Residuals (pixels) of the matched quad's index stars against the field
    /// stars they came from. Needs the field list, hence a method here.
    #[cfg(test)]
    #[must_use]
    pub fn residuals(&self, index: &Index, stars: &[Star]) -> Vec<f64> {
        let d = index.meta.dimquads;
        index.quad(self.quad)[..d]
            .iter()
            .zip(self.field_stars[..d].iter())
            .map(|(&sid, &fid)| {
                let (ra, dec) = crate::coords::xyz_to_radec(index.star_xyz(sid as usize));
                let (px, py) = self.wcs.radec_to_pix(ra, dec);
                f64::hypot(px - stars[fid].x, py - stars[fid].y)
            })
            .collect()
    }
}

/// Counters describing one `solve_index` pass (verbose logging / tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct SolveStats {
    /// Code-vector ball searches issued.
    pub queries: usize,
    /// Raw codes-tree hits.
    pub code_hits: usize,
    pub radec_skipped: usize,
    pub abscale_skipped: usize,
    pub fit_skipped: usize,
    pub scale_skipped: usize,
    /// Hits that reached the callback.
    pub hits: usize,
    /// Set when the callback stopped the search.
    pub stopped_early: bool,
    /// Set when the `FieldSpec::deadline` passed mid-enumeration.
    pub timed_out: bool,
    /// Set when the `FieldSpec::cancel` flag was raised mid-enumeration.
    pub cancelled: bool,
}

impl SolveStats {
    /// Merge of two partial counters (per-worker accumulation).
    fn add(self, o: Self) -> Self {
        Self {
            queries: self.queries + o.queries,
            code_hits: self.code_hits + o.code_hits,
            radec_skipped: self.radec_skipped + o.radec_skipped,
            abscale_skipped: self.abscale_skipped + o.abscale_skipped,
            fit_skipped: self.fit_skipped + o.fit_skipped,
            scale_skipped: self.scale_skipped + o.scale_skipped,
            hits: self.hits + o.hits,
            stopped_early: self.stopped_early || o.stopped_early,
            timed_out: self.timed_out || o.timed_out,
            cancelled: self.cancelled || o.cancelled,
        }
    }
}

/// Cross-worker state of one `solve_index` pass.
#[derive(Default)]
struct Shared {
    /// The callback returned [`Flow::Stop`]: wind down all workers.
    stop: AtomicBool,
    /// The deadline passed (checked once per `DEADLINE_CHECK_PERIOD` events).
    timed_out: AtomicBool,
    /// Global search-event counter driving the periodic deadline check.
    checks: AtomicUsize,
}

/// Runs one index over the field on the rayon pool, one depth step at a time.
/// Every code hit that passes the ra/dec hint, the pre-fit |AB| scale check and
/// the post-fit scale check reaches `on_hit` with the fitted WCS; return
/// [`Flow::Stop`] to abandon the remaining depth steps (first-solution
/// semantics, for the tests and other first-solution callers). Production
/// `solve_field` never returns `Stop`: the capture of the attempt that solves has
/// to see every hit that attempt enumerates to keep the lexicographic-best solved
/// one, so the printed solution does not depend on worker scheduling
/// (`sweep.rs`).
///
/// `on_hit` may be called concurrently from several worker threads: it must stay
/// `Sync` and guard its own mutable state, and must not spawn rayon work (an
/// attempt runs inside a rayon task). After it returns `Stop`, other workers may
/// still finish the hits they already started.
pub fn solve_index(
    index: &Index,
    spec: &FieldSpec<'_>,
    on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync),
) -> SolveStats {
    debug_assert!(
        DEADLINE_CHECK_PERIOD.is_multiple_of(DEADLINE_EVENT_BATCH),
        "the batched deadline counter requires the batch to divide the period"
    );
    let shared = Shared::default();
    let n = usize::min(usize::min(spec.stars.len(), spec.max_objs), MAX_OBJS);
    let mut total = SolveStats::default();
    for imax in spec.obj_start..n {
        // One task per backbone; workers share `index`/`spec` immutably.
        let tasks = tasks_for_imax(imax);
        // No `with_min_len`: coarse chunks defeat `tasks_for_imax`'s heavy/light
        // interleaving.
        let part: SolveStats = tasks
            .par_iter()
            // One Searcher per fold split: its hit buffer and backbone
            // memoization are reused across that split's tasks.
            .fold(
                || Searcher::new(index, spec, &shared),
                |mut srch, task| {
                    srch.run_task(*task, on_hit);
                    srch
                },
            )
            .map(|srch| srch.stats)
            .reduce(SolveStats::default, SolveStats::add);
        total = total.add(part);
        // Deliberate precedence: `timed_out` is tested first, so a round in which
        // both flags are set reports a timeout and not a callback stop
        // (`past_deadline_stops_the_search` pins that a lone timeout never sets
        // `stopped_early`).
        if shared.timed_out.load(Ordering::Relaxed) {
            total.timed_out = true;
            break;
        }
        if shared.stop.load(Ordering::Relaxed) {
            total.stopped_early = true;
            break;
        }
        // Same tail, one rank lower: a cancel is neither a deadline nor a
        // callback `Stop`, and the sweep's fold needs to tell the three apart.
        // Only tested at a depth-step boundary, so an attempt cancelled during
        // its LAST `imax` step finishes that step and reports `Nothing`/`Solved`,
        // not `cancelled` - harmless for stdout, because the fold reports the
        // earlier solver that raised the cancel either way.
        if spec.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            total.cancelled = true;
            break;
        }
    }
    total
}

/// A unit of parallel work: all quads of one depth step `imax` that share a
/// backbone `(A, B)`, so its `cos/sin` stay memoized per worker.
#[derive(Clone, Copy)]
enum Task {
    /// The newest star occupies backbone slot B: `B = imax`, C/D run over the
    /// older stars.
    BackboneNewest { a: usize, b: usize },
    /// The backbone is made of older stars; the newest star takes the C or D
    /// slot.
    NewestSlot { a: usize, b: usize, imax: usize },
}

/// Enumerates the quads whose newest (largest-index) slot is `imax` (the
/// incremental step of the depth ramp), split by backbone.
///
/// The two families are INTERLEAVED per `a` so rayon's contiguous task chunks
/// stay balanced despite the cost gap: for `dimquads=4` `BackboneNewest` is
/// O(imax^2) and `NewestSlot` O(imax), for 3 they are O(imax) and O(1).
fn tasks_for_imax(imax: usize) -> Vec<Task> {
    let mut tasks = Vec::with_capacity(imax + imax * imax.saturating_sub(1) / 2);
    for a in 0..imax {
        tasks.push(Task::BackboneNewest { a, b: imax });
        for b in (a + 1)..imax {
            tasks.push(Task::NewestSlot { a, b, imax });
        }
    }
    tasks
}

struct Searcher<'a> {
    index: &'a Index,
    spec: &'a FieldSpec<'a>,
    shared: &'a Shared,
    stats: SolveStats,
    qmin2: f64,
    qmax2: f64,
    /// Slack allowed on the index's canonicalization filters:
    /// `CXDX_MARGIN_FACTOR * CODE_TOL` = 1.5 * 0.01 = 0.015, wide enough that a
    /// CODE_TOL ball query is never pruned away from a stored code.
    margin: f64,
    /// Pre-fit |AB| scale bounds, rad/px: `app_min`/`app_max` converted from
    /// arcsec/px and widened by a relative CODE_TOL (1 %) slack, because this
    /// gate runs before the fit and only has the A-B pair to go by.
    scale_lo: f64,
    scale_hi: f64,
    /// Hint center as unit vector + squared chord radius.
    hint: Option<([f64; 3], f64)>,
    dimquads: usize,
    /// The compact codes tree of the current hint, when the sweep has one; see
    /// [`FieldSpec::hint_codes`].
    hint_codes: Option<&'a HintCodes>,
    /// `--cpulimit` deadline, checked via the global counter, see
    /// [`Searcher::should_stop`].
    deadline: Option<std::time::Instant>,
    /// Sibling-cancel flag of a concurrent index attempt, see
    /// [`FieldSpec::cancel`].
    cancel: Option<&'a AtomicBool>,
    /// Iterations accumulated locally before one atomic add to the global
    /// counter (avoids per-iteration cache-line ping-pong).
    deadline_events: usize,
    /// Reused hit buffer of the codes ball search (debug builds would otherwise
    /// allocate per query here).
    hitbuf: Vec<Hit>,
    /// Memoized backbone of the last `triple`/`quad` call: the enumeration holds
    /// `(A, B)` fixed while `C` runs, so `cos/sin` and the |AB| range gate are
    /// computed once per backbone.
    bb: Option<Backbone>,
}

/// Cached per-backbone `(A, B)` quantities: the two coefficients of the code
/// formula plus the verdict of the |AB| quad-size gate.
#[derive(Clone, Copy)]
struct Backbone {
    a: usize,
    b: usize,
    cos: f64,
    sin: f64,
    /// `qmin2 <= |AB|^2 <= qmax2`.
    within: bool,
}

impl<'a> Searcher<'a> {
    fn new(index: &'a Index, spec: &'a FieldSpec<'a>, shared: &'a Shared) -> Self {
        let qmin = QSF_LO * f64::min(spec.width, spec.height);
        let qmax = QSF_HI * f64::hypot(spec.width, spec.height);
        let hint = spec.radecs.map(|h| h.precompute());
        Self {
            index,
            spec,
            shared,
            stats: SolveStats::default(),
            qmin2: qmin * qmin,
            qmax2: qmax * qmax,
            margin: CXDX_MARGIN_FACTOR * CODE_TOL,
            scale_lo: arcsec2rad(spec.app_min) * (1.0 - CODE_TOL),
            scale_hi: arcsec2rad(spec.app_max) * (1.0 + CODE_TOL),
            hint,
            dimquads: index.meta.dimquads,
            hint_codes: spec.hint_codes,
            deadline: spec.deadline,
            cancel: spec.cancel,
            deadline_events: 0,
            hitbuf: Vec::new(),
            bb: None,
        }
    }

    /// Runs one backbone task: the inner C/D loops of [`Task`]. Non-backbone
    /// slots never reuse A or B; coverage of the full quad set is pinned by
    /// `depth_rounds_partition_the_hits` and the serial-vs-parallel multiset
    /// equality in `hit_multiset_is_schedule_independent`.
    fn run_task(&mut self, task: Task, on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync)) {
        match task {
            Task::BackboneNewest { a, b } => {
                if self.dimquads == 3 {
                    for c in 0..b {
                        if self.should_stop() {
                            return;
                        }
                        if c != a && self.triple(a, b, c, on_hit) == Flow::Stop {
                            return;
                        }
                    }
                } else {
                    for c in 0..b {
                        // C and D never reuse a backbone star: the code of A is
                        // (0,0) and of B is (1,1), both on the circle filter, so
                        // only the enumeration can drop the repeat.
                        if c == a {
                            continue;
                        }
                        for dd in 0..b {
                            if self.should_stop() {
                                return;
                            }
                            if dd != a && c != dd && self.quad(a, b, c, dd, on_hit) == Flow::Stop {
                                return;
                            }
                        }
                    }
                }
            }
            Task::NewestSlot { a, b, imax } => {
                if self.dimquads == 3 {
                    if !self.should_stop() {
                        // `Stop` is stored globally in `shared.stop` and seen by
                        // every other loop head/query, so dropping the local
                        // Flow here is intentional.
                        let _ = self.triple(a, b, imax, on_hit);
                    }
                } else {
                    for s in 0..imax {
                        if self.should_stop() {
                            return;
                        }
                        if s == a || s == b {
                            continue;
                        }
                        if self.quad(a, b, imax, s, on_hit) == Flow::Stop {
                            return;
                        }
                        if self.quad(a, b, s, imax, on_hit) == Flow::Stop {
                            return;
                        }
                    }
                }
            }
        }
    }

    fn star(&self, i: usize) -> &'a Star {
        &self.spec.stars[i]
    }

    /// `cos/sin` of the code formula for the backbone `(A, B)` plus the |AB|
    /// quad-size gate, memoized across the inner `C` loops.
    fn backbone(&mut self, a: usize, b: usize) -> (f64, f64) {
        if let Some(bb) = self.bb
            && bb.a == a
            && bb.b == b
        {
            return (bb.cos, bb.sin);
        }
        let (sa, sb) = (self.star(a), self.star(b));
        let dx = sb.x - sa.x;
        let dy = sb.y - sa.y;
        let s = dx * dx + dy * dy;
        // Quad-size gate on the backbone length, from the frame alone: |AB| must
        // lie between QSF_LO * min(W, H) and QSF_HI * the frame diagonal. There is
        // no second gate from the index's own quad-diameter range, and omitting
        // such a pre-filter can only add ball searches: every query must still land
        // inside the CODE_TOL ball and pass both scale gates to reach the callback.
        // `s > 0` belongs to the gate (a zero-length backbone would divide by
        // zero); `inv` stays 0 when it fails, so cos/sin stay finite and the caller
        // bails on `within`.
        let within = s >= self.qmin2 && s <= self.qmax2 && s > 0.0;
        let inv = if within { 1.0 / s } else { 0.0 };
        let bb = Backbone {
            a,
            b,
            cos: (dy + dx) * inv,
            sin: (dy - dx) * inv,
            within,
        };
        self.bb = Some(bb);
        (bb.cos, bb.sin)
    }

    /// One `dimquads=3` assignment: backbone `A=a, B=b` (`a < b` by list rank) and
    /// star `C=c`. Minimal sufficient enumeration: 2 parities per choice of C, minus
    /// what the index's canonicalization filters reject (see the module header).
    fn triple(
        &mut self,
        a: usize,
        b: usize,
        c: usize,
        on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync),
    ) -> Flow {
        let (cos, sin) = self.backbone(a, b);
        if !self.bb.unwrap().within {
            return Flow::Continue;
        }
        let (sa, sc) = (self.star(a), self.star(c));
        let (ux, uy) = (sc.x - sa.x, sc.y - sa.y);
        // Same formula as `codes::star_code`, backbone terms precomputed.
        let base = [ux * cos + uy * sin, -ux * sin + uy * cos];
        if !in_circle(base[0], base[1], CODE_TOL) {
            return Flow::Continue;
        }
        self.variants(&base, [a, b, c, 0], on_hit)
    }

    /// One `dimquads=4` assignment: backbone `A=a, B=b` (`a < b`), stars
    /// `C=c, D=d` (ordered - both orders of C,D are tried by the caller).
    fn quad(
        &mut self,
        a: usize,
        b: usize,
        c: usize,
        dd: usize,
        on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync),
    ) -> Flow {
        let (cos, sin) = self.backbone(a, b);
        if !self.bb.unwrap().within {
            return Flow::Continue;
        }
        let (sa, sc, sd) = (self.star(a), self.star(c), self.star(dd));
        let cc = {
            let (ux, uy) = (sc.x - sa.x, sc.y - sa.y);
            [ux * cos + uy * sin, -ux * sin + uy * cos]
        };
        // Every non-backbone star must pass the circle filter, not just C (see
        // `codes::in_circle`); an index that does not declare the CIRCLE convention
        // is refused at load (`index.rs`), so running this filter unconditionally
        // cannot drop a code that could have matched.
        if !in_circle(cc[0], cc[1], CODE_TOL) {
            return Flow::Continue;
        }
        let dc = {
            let (ux, uy) = (sd.x - sa.x, sd.y - sa.y);
            [ux * cos + uy * sin, -ux * sin + uy * cos]
        };
        if !in_circle(dc[0], dc[1], CODE_TOL) {
            return Flow::Continue;
        }
        let base = [cc[0], cc[1], dc[0], dc[1]];
        self.variants(&base, [a, b, c, dd], on_hit)
    }

    /// Queries the four orbit variants `{c, 1-c, flip(c), 1-flip(c)}` of a base
    /// code, keeping those `codes::canonical_ok` accepts for the flags this index
    /// declares (both handednesses are always offered; the filter does the pruning).
    /// The pruning is sound: a `CXDX`+`CXDXLT1` index stores only codes of mean
    /// x <= 0.5 and a ball search returns only codes within `CODE_TOL < margin` of
    /// the query, so a dropped variant is at most a code too far from every stored
    /// code to ever match.
    ///
    /// Because field backbones are enumerated in list-rank order rather than
    /// canonicalized, for about half of the field codes the `1 - code` variant is
    /// the only one that can match a stored code.
    fn variants(
        &mut self,
        base: &[f64],
        corr: [usize; 4],
        on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync),
    ) -> Flow {
        let cxdx = self.index.meta.cx_less_than_dx;
        let cxdxlt1 = self.index.meta.cx_less_than_dx && self.index.meta.meanx_less_than_half;
        let ndim = base.len();
        let flip = parity_flip(base);
        let rot = swap_backbone(base);
        let rotflip = swap_backbone(&flip[..ndim]);
        for (code, swapped) in [
            (base, false),
            (&flip[..ndim], false),
            (&rot[..ndim], true),
            (&rotflip[..ndim], true),
        ] {
            if !canonical_ok(code, cxdx, cxdxlt1, self.margin) {
                continue;
            }
            // The `1 - code` variants are the same quad with the backbone read
            // the other way round, so the field stars of slots A and B swap to
            // stay aligned with the index quad's A and B.
            let corr = if swapped {
                [corr[1], corr[0], corr[2], corr[3]]
            } else {
                corr
            };
            if self.query(code, corr, on_hit) == Flow::Stop {
                return Flow::Stop;
            }
        }
        Flow::Continue
    }

    /// Cooperative stop for all workers: the callback stopped, a sibling of a
    /// concurrent chunk cancelled this attempt, or the `--cpulimit` deadline
    /// passed. With a deadline set each call counts as one search event (a call
    /// that already sees a stop returns before counting); events are counted
    /// globally, accumulated per worker in batches of `DEADLINE_EVENT_BATCH` (one
    /// atomic RMW per batch), and the clock is read only when the global count
    /// crosses a `DEADLINE_CHECK_PERIOD` boundary, however many workers run. The
    /// cancel flag is tested BEFORE that counter, so an attempt that sees a cancel
    /// never also sees the deadline and `solve_index` reports it `cancelled`, not
    /// `timed_out`.
    fn should_stop(&mut self) -> bool {
        if self.shared.stop.load(Ordering::Relaxed) || self.shared.timed_out.load(Ordering::Relaxed)
        {
            return true;
        }
        // The cancel flag belongs to this attempt alone and is written once per
        // chunk, so the line stays shared and the load costs an L1 hit.
        if let Some(flag) = self.cancel
            && flag.load(Ordering::Relaxed)
        {
            return true;
        }
        if let Some(deadline) = self.deadline {
            self.deadline_events += 1;
            if self.deadline_events >= DEADLINE_EVENT_BATCH {
                self.deadline_events = 0;
                let prev = self
                    .shared
                    .checks
                    .fetch_add(DEADLINE_EVENT_BATCH, Ordering::Relaxed);
                let crossed = prev / DEADLINE_CHECK_PERIOD
                    != (prev + DEADLINE_EVENT_BATCH) / DEADLINE_CHECK_PERIOD;
                if crossed && std::time::Instant::now() >= deadline {
                    self.shared.timed_out.store(true, Ordering::Relaxed);
                    return true;
                }
            }
        }
        false
    }

    /// Ball search of one code vector at the CODE_TOL radius; the hits are
    /// processed in ascending code-distance order, so a query offers its closest
    /// code to the callback first.
    ///
    /// Under a hint the sweep hands over a tree of only the reachable records
    /// ([`FieldSpec::hint_codes`]), searched in place of the index's own: a 3 deg
    /// circle covers a fraction of a percent of a wide index, so nearly every query
    /// would otherwise descend through records the hint gate throws away. A hit's
    /// record is mapped back to the quad it is the code of before processing.
    fn query(
        &mut self,
        code: &[f64],
        corr: [usize; 4],
        on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync),
    ) -> Flow {
        self.stats.queries += 1;
        if self.should_stop() {
            return Flow::Stop;
        }
        // Reuse the hit buffer across queries (hot loop).
        let mut buf = std::mem::take(&mut self.hitbuf);
        buf.clear();
        match self.hint_codes {
            Some(hint) => hint.tree.ball_search_into(code, CODE_TOL, &mut buf),
            None => self.index.codes.ball_search_into(code, CODE_TOL, &mut buf),
        }
        buf.sort_by(|a, b| f64::total_cmp(&a.dist2, &b.dist2));
        let mut flow = Flow::Continue;
        for hit in buf.iter() {
            self.stats.code_hits += 1;
            // Hits (not queries) dominate the runtime on wide scale ranges: every
            // hit runs a fit, so the stop check runs here too.
            if self.should_stop() {
                flow = Flow::Stop;
                break;
            }
            let quad_no = match self.hint_codes {
                Some(hint) => hint.quad_of[hit.index] as usize,
                None => hit.index,
            };
            if self.process_hit(quad_no, hit.dist2, corr, on_hit) == Flow::Stop {
                flow = Flow::Stop;
                break;
            }
        }
        self.hitbuf = buf;
        flow
    }

    /// Turns one code hit into a candidate solution: hint gate on the matched stars
    /// -> pre-fit |AB| scale gate -> TAN fit -> post-fit scale gate -> `on_hit`.
    /// Verification and odds are the caller's (`verify.rs`).
    ///
    /// This star-by-star gate is the ONLY accept test a hinted hit faces: the hint
    /// structures of `index.rs` decide which records a query can return, never
    /// whether a returned hit is accepted, so an attempt that searched the index's
    /// own codes tree (a blind round, or a file outside the sweep's tree budget)
    /// and one that searched the filtered tree accept exactly the same hits. A
    /// per-hit quad mask would only re-test what this loop already tests on its way
    /// to the fit.
    fn process_hit(
        &mut self,
        quad_no: usize,
        code_dist2: f64,
        corr: [usize; 4],
        on_hit: &(dyn Fn(&QuadHit) -> Flow + Sync),
    ) -> Flow {
        let d = self.dimquads;
        let stars = self.index.quad(quad_no);
        let mut starxyz = [[0.0f64; 3]; 4];
        let mut fieldxy = [[0.0f64; 2]; 4];
        for i in 0..d {
            starxyz[i] = self.index.star_xyz(stars[i] as usize);
            if let Some(hint) = self.hint {
                // Any matched star outside the hint circle rejects the hit
                // before any fitting, NaN distance (corrupt index star) too; the
                // post-fit gate accepts NaN on purpose, so the two sites are not
                // interchangeable (see `star_outside_hint`).
                if star_outside_hint(&starxyz[i], &hint) {
                    self.stats.radec_skipped += 1;
                    return Flow::Continue;
                }
            }
            let s = self.star(corr[i]);
            fieldxy[i] = [s.x, s.y];
        }

        // Quick scale estimate from the A-B pair, BEFORE the fit: the index stars'
        // angular separation (chord^2 -> radians) over their field distance in
        // pixels gives an implied scale in rad/px, compared against the requested
        // range widened by the CODE_TOL slack (`Searcher::new`).
        let ab_chord2 = distsq(&starxyz[0], &starxyz[1]);
        let ab_pix = f64::hypot(fieldxy[1][0] - fieldxy[0][0], fieldxy[1][1] - fieldxy[0][1]);
        let implied = chord2_to_rad(ab_chord2) / ab_pix;
        // Explicit-accept range gate: `contains` is false for NaN - here only the
        // pixel side can make one, the 0/0 of coincident field and index A-B
        // pairs - where `implied < lo || implied > hi` would fail open.
        if !(self.scale_lo..=self.scale_hi).contains(&implied) {
            self.stats.abscale_skipped += 1;
            return Flow::Continue;
        }

        let Some(wcs) = fit_tan_wcs(&starxyz[..d], &fieldxy[..d]) else {
            self.stats.fit_skipped += 1;
            return Flow::Continue;
        };
        let arcsec = wcs.arcsec_per_pix();
        // Post-fit gate on the fitted scale, in arcsec/px and with NO slack
        // (`spec.app_min..=spec.app_max`); the same explicit-accept form throws
        // out a NaN scale from a degenerate fit.
        if !(self.spec.app_min..=self.spec.app_max).contains(&arcsec) {
            self.stats.scale_skipped += 1;
            return Flow::Continue;
        }

        self.stats.hits += 1;
        // Verbose line: code distance in code units (the square root of the
        // squared L2), the fitted scale, and where the WCS points at the frame's
        // center pixel.
        if self.spec.verbose {
            let (ra, dec) = wcs.pix_to_radec(
                pixel_center_of_size(self.spec.width),
                pixel_center_of_size(self.spec.height),
            );
            eprintln!(
                "[hit] index {} quad {} field=[{}] dist={:.5} scale={:.5}\"/px center=({:.4},{:.4})",
                self.index.meta.indexid,
                quad_no,
                corr[..d]
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                f64::sqrt(code_dist2),
                arcsec,
                ra,
                dec
            );
        }
        let flow = on_hit(&QuadHit {
            quad: quad_no,
            code_dist2,
            field_stars: corr,
            wcs,
        });
        if flow == Flow::Stop {
            self.shared.stop.store(true, Ordering::Relaxed);
        }
        flow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use crate::wcs::test_data::*;

    /// With the uncapped reader order that this test hands the solver directly
    /// (`xyls.rs` -> `augment.rs`; on the CLI path `run_into` would re-sort it
    /// brightest-first), the narrow field `1920x1080-2` already verifies a solved
    /// hit on the index that wins it inside the FIRST TWO depth rounds
    /// (objs 1-20); handed the 20 highest-FLUX detections it only made it at the
    /// objs 41-50 round.
    ///
    /// Which index wins is a property of the installed set, so it is discovered at
    /// runtime and no index id or exact log-odds is pinned. The strength of the
    /// match stays the invariant (a marginal bogus hit could not match many
    /// stars).
    #[test]
    fn narrow_field2_verifies_true_match_in_first_rounds() {
        let Some(run) = testutil::field_run("1920x1080-2.xyls", 1920.0, 1080.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let spec = FieldSpec {
            stars: &run.stars,
            width: 1920.0,
            height: 1080.0,
            // A tight window around 1 "/px that holds the field's published
            // scale (0.997 "/px).
            app_min: 0.95,
            app_max: 1.05,
            radecs: None,
            hint_codes: None,
            // Two rounds of the sweep's depth ramp: objs 1-20.
            max_objs: 20,
            obj_start: 0,
            verbose: false,
            deadline: None,
            cancel: None,
        };
        let best = std::sync::Mutex::new((f64::NEG_INFINITY, 0usize));
        let pre = crate::verify::FieldPre::new(&run.stars);
        solve_index(&index, &spec, &|hit| {
            let vr = crate::verify::verify_hit(&index, hit, &run.stars, &pre, 1920.0, 1080.0);
            let mut b = best.lock().unwrap();
            if vr.bestlogodds > b.0 {
                *b = (vr.bestlogodds, vr.nmatch);
            }
            Flow::Continue
        });
        let (logodds, nmatch) = *best.lock().unwrap();
        assert!(
            logodds >= crate::params::ODDS_TO_SOLVE_LN,
            "objs 1-20 must already solve on the field's winning index ({}), got logodds {logodds}",
            run.meta.indexid
        );
        assert!(
            nmatch >= testutil::MIN_WINNER_NMATCH,
            "the true match must hold many matched stars, got {nmatch} (logodds {logodds})"
        );
    }

    fn spec<'a>(stars: &'a [Star], radecs: Option<PositionHint>) -> FieldSpec<'a> {
        FieldSpec {
            stars,
            width: 4656.0,
            height: 3522.0,
            // The blind-mode "no scale hint" range: the frame width spanning 0.1
            // to 180 degrees over the frame width in pixels (what `cli` produces
            // when neither `--scale-*` is given).
            app_min: 0.1 * 3600.0 / 4656.0,
            app_max: 180.0 * 3600.0 / 4656.0,
            radecs,
            // The compact hint tree is the sweep's business; these passes
            // exercise the gate and the index's own codes tree.
            hint_codes: None,
            max_objs: 15,
            obj_start: 0,
            verbose: false,
            deadline: None,
            cancel: None,
        }
    }

    /// Mechanics fixture for the field-1 tests: the installed index that actually
    /// solves `4656x3522-1`, loaded. Its winning round is guaranteed to produce at
    /// least one code hit, which is all the mechanics tests below need;
    /// `field_run` is cached for the whole test binary.
    fn field1_winner_index() -> Option<(&'static testutil::FieldRun, Index)> {
        let run = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None)?;
        Some((run, testutil::load(run.meta)))
    }

    /// The hit multiset `(quad, field_stars)` of one pass, sorted.
    fn collect_hits(index: &Index, stars: &[Star], max_objs: usize) -> Vec<(usize, [usize; 4])> {
        let mut s = spec(stars, None);
        s.max_objs = max_objs;
        let out = std::sync::Mutex::new(Vec::new());
        solve_index(index, &s, &|hit| {
            out.lock().unwrap().push((hit.quad, hit.field_stars));
            Flow::Continue
        });
        let mut out = out.into_inner().unwrap();
        out.sort();
        out
    }

    /// Runs the enumeration twice, once on the ambient rayon pool and once on
    /// a single-thread pool, and requires the same non-empty hit multiset.
    fn assert_parallel_equals_serial(index: &Index, stars: &[Star], max_objs: usize) {
        let parallel = collect_hits(index, stars, max_objs);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let serial = pool.install(|| collect_hits(index, stars, max_objs));
        assert!(
            !parallel.is_empty(),
            "the enumeration produced no hits at depth {max_objs}"
        );
        assert_eq!(
            serial, parallel,
            "serial enumeration differs from the parallel one"
        );
    }

    /// Task-split algebra of one depth step, independent of index data: `imax`
    /// backbone-newest tasks + one newest-slot task per older pair.
    #[test]
    fn tasks_for_imax_counts() {
        let counts = |imax: usize| {
            let t = tasks_for_imax(imax);
            let bn = t
                .iter()
                .filter(|task| matches!(task, Task::BackboneNewest { b, .. } if *b == imax))
                .count();
            (t.len(), bn)
        };
        assert_eq!(counts(0), (0, 0));
        assert_eq!(counts(1), (1, 1));
        assert_eq!(counts(4), (4 + 6, 4));
        let imax = 12;
        assert_eq!(counts(imax), (imax + imax * (imax - 1) / 2, imax));
    }

    /// The round winner on `4656x3522-1.xyls` fits the field's published sky
    /// truth: scale within 0.05" of 2.71247 "/px, the field-center pixel pointing
    /// within 1 deg of (60.031, 36.376), matched index quad within 1 px of its
    /// field stars.
    ///
    /// The window invariant on its slots: distinct stars below `max_objs`, and
    /// the NEWEST slot (the maximum - the star whose step enumerated this quad)
    /// inside the round window; older slots may predate `obj_start`. No quad id is
    /// pinned: which quad wins is a property of the installed index set.
    #[test]
    fn field1_winning_hit_matches_sky_truth() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let h = testutil::winning_hit(run);
        let d = index.meta.dimquads;
        let slots = &h.hit.field_stars[..d];
        for (i, a) in slots.iter().enumerate() {
            assert!(
                *a < run.max_objs,
                "slot {a} beyond the round cap {}",
                run.max_objs
            );
            for (j, b) in slots.iter().enumerate() {
                assert!(i == j || a != b, "reused field star in hit {:?}", h.hit);
            }
        }
        let newest = slots.iter().max().copied().unwrap();
        assert!(
            (run.obj_start..run.max_objs).contains(&newest),
            "newest slot {newest} outside the round window {}..{}",
            run.obj_start,
            run.max_objs
        );
        assert!(
            f64::abs(h.hit.arcsec_per_pix() - FIELD1_SCALE_ARCSEC) < 0.05,
            "scale {}",
            h.hit.arcsec_per_pix()
        );
        let (ra, dec) = h.hit.wcs.pix_to_radec(
            pixel_center_of_size(run.width),
            pixel_center_of_size(run.height),
        );
        assert!(
            f64::abs(ra - FIELD1_CENTER.0) < 1.0 && f64::abs(dec - FIELD1_CENTER.1) < 1.0,
            "center {ra},{dec}"
        );
        let res = h.hit.residuals(&index, &run.stars);
        assert!(res.iter().all(|&r| r < 1.0), "residuals {res:?}");
    }

    /// The same gate for field 2, on the installed index that wins it: scale and
    /// center against `testutil::FIELD2_TRUTH`, distinct slots inside the round
    /// window, sub-pixel residuals; as in field 1, no quad or index id is pinned.
    #[test]
    fn field2_winning_hit_matches_sky_truth() {
        let Some(run) = testutil::field_run("4656x3522-2.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let h = testutil::winning_hit(run);
        let d = index.meta.dimquads;
        let slots = &h.hit.field_stars[..d];
        for (i, a) in slots.iter().enumerate() {
            assert!(
                *a < run.max_objs,
                "slot {a} beyond the round cap {}",
                run.max_objs
            );
            for (j, b) in slots.iter().enumerate() {
                assert!(i == j || a != b, "reused field star in hit {:?}", h.hit);
            }
        }
        let newest = slots.iter().max().copied().unwrap();
        assert!(
            (run.obj_start..run.max_objs).contains(&newest),
            "newest slot {newest} outside the round window {}..{}",
            run.obj_start,
            run.max_objs
        );
        assert!(
            f64::abs(h.hit.arcsec_per_pix() - testutil::FIELD2_TRUTH.scale_arcsec) < 0.05,
            "scale {}",
            h.hit.arcsec_per_pix()
        );
        let (ra, dec) = h.hit.wcs.pix_to_radec(
            pixel_center_of_size(run.width),
            pixel_center_of_size(run.height),
        );
        assert!(
            f64::abs(ra - testutil::FIELD2_TRUTH.ra_deg) < 1.0
                && f64::abs(dec - testutil::FIELD2_TRUTH.dec_deg) < 1.0,
            "center {ra},{dec}"
        );
        let res = h.hit.residuals(&index, &run.stars);
        assert!(res.iter().all(|&r| r < 1.0), "residuals {res:?}");
    }

    /// The ra/dec hint rejects hits away from the hint circle: the far-hint pass
    /// still queries the tree, every code hit is skipped by the hint and none
    /// reaches the callback.
    #[test]
    fn radec_hint_rejects_far_field() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let mut s = spec(
            &run.stars,
            Some(PositionHint {
                ra_deg: 12.0,
                dec_deg: -45.0,
                radius_deg: 1.0,
            }),
        );
        s.max_objs = run.max_objs;
        let stats = solve_index(&index, &s, &|_| Flow::Continue);
        assert!(stats.queries > 0, "enumeration produced no queries");
        assert!(
            stats.radec_skipped > 0 && stats.hits == 0,
            "hint did not filter: {stats:?}"
        );
    }

    /// Every slot of a hit holds a distinct field star: a repeat gives a
    /// degenerate code (`codes::star_code` returns `(0, 0)` for a star in the A
    /// slot and `(1, 1)` for one in the B slot, both on the circle filter, so the
    /// code checks would not catch it) and a degenerate fit. Hence the enumeration
    /// itself skips A and B when it fills the C and D slots.
    #[test]
    fn hits_never_reuse_a_field_star() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let mut s = spec(&run.stars, None);
        s.max_objs = run.max_objs;
        let checked = std::sync::atomic::AtomicUsize::new(0);
        let stats = solve_index(&index, &s, &|hit| {
            let slots = &hit.field_stars[..index.meta.dimquads];
            for (i, a) in slots.iter().enumerate() {
                for (j, b) in slots.iter().enumerate() {
                    assert!(i == j || a != b, "reused field star in hit {hit:?}");
                }
            }
            checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Flow::Continue
        });
        assert!(stats.queries > 0, "enumeration produced no queries");
        assert!(
            checked.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "no hits were produced to check; {stats:?}"
        );
    }

    /// Consecutive depth windows must partition the enumeration exactly: every
    /// quad is enumerated once, at `imax = max(its field slots)`, so the hits of
    /// `[0,split)` and `[split,n)` together make the hits of `[0,n)` without
    /// overlap (the depth rounds of `sweep.rs` rely on this).
    #[test]
    fn depth_rounds_partition_the_hits() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let d = index.meta.dimquads;
        // Split the winning round's window in half: it is known to hold at least
        // the round's solving quad.
        let (split, n) = (run.max_objs / 2, run.max_objs);

        let collect = |obj_start: usize, max_objs: usize| -> Vec<(usize, [usize; 4])> {
            let mut s = spec(&run.stars, None);
            s.obj_start = obj_start;
            s.max_objs = max_objs;
            let out = std::sync::Mutex::new(Vec::new());
            solve_index(&index, &s, &|hit| {
                out.lock().unwrap().push((hit.quad, hit.field_stars));
                Flow::Continue
            });
            out.into_inner().unwrap()
        };

        let all = collect(0, n);
        assert!(!all.is_empty(), "no hits at all; raise `n`");
        let lo = collect(0, split);
        let hi = collect(split, n);

        // Each round only touches its own window, always using its largest slot
        // as the newest star `imax`.
        for (quad, fs) in &lo {
            let m = fs[..d].iter().max().copied().unwrap();
            assert!(
                m < split,
                "round [0,{split}) emitted imax {m} (quad {quad})"
            );
        }
        for (quad, fs) in &hi {
            let m = fs[..d].iter().max().copied().unwrap();
            assert!(
                (split..n).contains(&m),
                "round [{split},{n}) emitted imax {m} (quad {quad})"
            );
        }

        // Multiset equality (no dedup): the rounds are disjoint AND cover every
        // hit exactly once.
        let mut merged = lo;
        merged.extend(hi);
        merged.sort();
        let mut all = all;
        all.sort();
        assert_eq!(merged, all, "depth rounds did not partition the hits");
    }

    /// `Flow::Stop` must be reflected in `stopped_early` and actually cut the
    /// remaining work short (parallel best-effort: the stopped pass may finish
    /// in-flight hits, but cannot do the whole sweep). The strict query
    /// inequality is the substance; the hit inequality says the stopped pass
    /// cannot have emitted the full round's multiset.
    #[test]
    fn stop_early_truncates_work() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let mut s = spec(&run.stars, None);
        s.max_objs = run.max_objs;
        let full = solve_index(&index, &s, &|_| Flow::Continue);
        let stopped = solve_index(&index, &s, &|_| Flow::Stop);
        assert!(stopped.stopped_early, "Stop not reported in stats");
        assert!(!full.stopped_early, "full pass unexpectedly stopped");
        assert!(
            stopped.queries < full.queries && stopped.hits < full.hits,
            "stop did not truncate: {stopped:?} vs {full:?}"
        );
    }

    /// The rayon task decomposition must emit the exact same multiset of
    /// `(quad, field_stars)` hits as a serial run of the same search: one pass on
    /// a single-thread rayon pool, the other on the ambient pool, sorted multisets
    /// equal and non-empty.
    ///
    /// The multiset - not the order it arrives in - is the invariant: the sweep
    /// only gets a deterministic round winner once the set of hits a round
    /// produces is independent of how its tasks were scheduled (`sweep.rs`). Both
    /// a `dimquads=3` field and a `dimquads=4` synthetic quad are exercised.
    #[test]
    fn hit_multiset_is_schedule_independent() {
        // dimquads=3: real field 1 against its winning index, winning round.
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        assert_parallel_equals_serial(&index, &run.stars, run.max_objs);

        // dimquads=4: the synthetic projected quad, depth 4.
        let Some(meta) = testutil::dim4_index() else {
            return;
        };
        let index4 = testutil::load(meta);
        let (_q, px) = pick_dim4_quad(&index4).expect("no usable dim4 quad found");
        let field: Vec<Star> = px
            .iter()
            .enumerate()
            .map(|(i, [x, y])| Star {
                x: *x,
                y: *y,
                flux: 1000.0 - i as f64,
            })
            .collect();
        assert_parallel_equals_serial(&index4, &field, 4);
    }

    /// A `--cpulimit` deadline already in the past must stop the enumeration and
    /// report `timed_out`.
    #[test]
    fn past_deadline_stops_the_search() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let mut s = spec(&run.stars, None);
        s.max_objs = run.max_objs;
        s.deadline = Some(std::time::Instant::now());
        let stats = solve_index(&index, &s, &|_| Flow::Continue);
        assert!(stats.timed_out, "past deadline did not trip the timeout");
        assert!(
            !stats.stopped_early,
            "a timeout must not be reported as a callback stop: {stats:?}"
        );
        assert!(
            stats.queries > 0,
            "the search never even started: {stats:?}"
        );
    }

    /// The other half of the tail's precedence: an attempt that times out while a
    /// cancel flag is present (but never raised) is still reported as a timeout,
    /// so the sweep's fold prints the timeout line instead of treating the attempt
    /// as speculative work to be dropped.
    #[test]
    fn a_timeout_is_not_reported_as_a_cancel() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let mut s = spec(&run.stars, None);
        s.max_objs = run.max_objs;
        s.deadline = Some(std::time::Instant::now());
        let cancelled = AtomicBool::new(false);
        s.cancel = Some(&cancelled);
        let stats = solve_index(&index, &s, &|_| Flow::Continue);
        assert!(
            stats.timed_out,
            "the deadline must still be reported: {stats:?}"
        );
        assert!(
            !stats.cancelled,
            "a timeout must not be reported as a cancel: {stats:?}"
        );
        assert!(
            !cancelled.load(Ordering::Relaxed),
            "the deadline path must not raise the cancel flag: {stats:?}"
        );
    }

    /// A cancel raised by a sibling is reported as `cancelled` - never as a
    /// timeout, never as a callback stop - and winds the walk down instead of
    /// running the round to its end.
    #[test]
    fn cancel_stops_the_search_and_reports_cancelled() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };
        let mut s = spec(&run.stars, None);
        s.max_objs = run.max_objs;
        let full = solve_index(&index, &s, &|_| Flow::Continue);
        assert!(full.hits > 1, "the round must offer more than one hit");
        assert!(
            !full.cancelled && !full.timed_out && !full.stopped_early,
            "the uncancelled round reported a reason to stop: {full:?}"
        );

        // Cancelled before the first query: every worker stops at its first
        // `should_stop`, so nothing reaches the callback.
        let mut s = spec(&run.stars, None);
        s.max_objs = run.max_objs;
        let flag = AtomicBool::new(true);
        s.cancel = Some(&flag);
        let stats = solve_index(&index, &s, &|_| Flow::Continue);
        assert!(
            stats.cancelled,
            "the cancel flag never reached the stats: {stats:?}"
        );
        assert!(!stats.timed_out, "a cancel is not a timeout: {stats:?}");
        assert!(
            !stats.stopped_early,
            "a cancel must not be reported as a callback stop: {stats:?}"
        );
        assert_eq!(stats.hits, 0, "a pre-cancelled walk must deliver nothing");
        assert_eq!(
            stats.queries, 0,
            "a pre-cancelled walk must not even reach a code search: {stats:?}"
        );

        // Cancelled mid-round (the callback stands for the sibling that just
        // solved): the walk stops within a few hits of the flag, not at the end.
        let flag = AtomicBool::new(false);
        s.cancel = Some(&flag);
        let stats = solve_index(&index, &s, &|_| {
            flag.store(true, Ordering::Relaxed);
            Flow::Continue
        });
        assert!(
            stats.cancelled,
            "a cancel raised mid-round never reached the stats: {stats:?}"
        );
        assert!(!stats.timed_out && !stats.stopped_early, "{stats:?}");
        assert!(
            stats.hits < full.hits,
            "the cancelled walk ran the whole round: {stats:?} vs {full:?}"
        );
    }

    /// Both scale gates fire: the pre-fit |AB| check rejects when the allowed
    /// scale range is far off, the post-fit check when the range differs from the
    /// truth by less than the pre-fit slack (unrelated bogus quads may still land
    /// inside such a narrow window, so only the counters are asserted).
    #[test]
    fn scale_bounds_filter_hits() {
        let Some((run, index)) = field1_winner_index() else {
            return;
        };

        // Far from the true 2.7125 "/px: the pre-fit |AB| gate rejects.
        let mut far = spec(&run.stars, None);
        far.max_objs = run.max_objs;
        far.app_min = 10.0;
        far.app_max = 20.0;
        let stats = solve_index(&index, &far, &|_| Flow::Continue);
        assert!(
            stats.abscale_skipped > 0 && stats.hits == 0,
            "pre-fit gate did not fire: {stats:?}"
        );

        // Just outside the true scale but inside the `1 +/- CODE_TOL` pre-fit
        // slack: only the post-fit gate can reject it.
        let mut tight = spec(&run.stars, None);
        tight.max_objs = run.max_objs;
        tight.app_min = FIELD1_SCALE_ARCSEC * 1.005;
        tight.app_max = FIELD1_SCALE_ARCSEC * 1.02;
        let stats = solve_index(&index, &tight, &|_| Flow::Continue);
        assert!(
            stats.scale_skipped > 0,
            "post-fit gate did not fire: {stats:?}"
        );
    }

    /// First quad of the installed dim4 index that projects cleanly into a
    /// synthetic field frame, as `(quad, projected pixels)`; shared by the dim4
    /// tests.
    ///
    /// The projection runs through a tangent WCS at the QUAD'S OWN centroid with
    /// `FIELD1_WCS`'s CD, so the synthetic plate scale is `FIELD1_SCALE_ARCSEC` and
    /// the parity convention matches the real fields while any dim4 tile works no
    /// matter where it sits (through `FIELD1_WCS` itself it would have to overlap
    /// field 1). Guards: every pair of projected pixels at least 50 px apart, `|AB|`
    /// inside 500..5000 px (inside the backbone window the tests' 4656x3522 frame
    /// gives: 352 px up to the 5838 px diagonal), and C's height over AB at least
    /// 5 % of the triangle's longest side, so `fit_tan_wcs` stays well-conditioned
    /// (a collinear C can sit dead-center of the code circle, so the index's CIRCLE
    /// filter does not exclude it).
    fn pick_dim4_quad(index: &Index) -> Option<(usize, Vec<[f64; 2]>)> {
        for q in 0..index.meta.nquads {
            let quad = index.quad(q);
            let mut xyz = [[0.0f64; 3]; 4];
            let mut cm = [0.0f64; 3];
            for j in 0..4 {
                xyz[j] = index.star_xyz(quad[j] as usize);
                for k in 0..3 {
                    cm[k] += xyz[j][k];
                }
            }
            let norm = f64::sqrt(cm[0] * cm[0] + cm[1] * cm[1] + cm[2] * cm[2]);
            if norm < 0.5 {
                continue; // stars spread over the sphere: no local tangent frame
            }
            let (cra, cdec) =
                crate::coords::xyz_to_radec([cm[0] / norm, cm[1] / norm, cm[2] / norm]);
            let w = Wcs {
                crpix: [0.0, 0.0],
                crval_deg: [cra, cdec],
                cd: FIELD1_WCS.cd,
            };
            let px: Vec<[f64; 2]> = (0..4)
                .map(|j| {
                    let (ra, dec) = crate::coords::xyz_to_radec(xyz[j]);
                    let (x, y) = w.radec_to_pix(ra, dec);
                    [x, y]
                })
                .collect();
            if !(500.0..5000.0).contains(&f64::hypot(px[1][0] - px[0][0], px[1][1] - px[0][1])) {
                continue;
            }
            let mut separated = true;
            for i in 0..4 {
                for j in (i + 1)..4 {
                    if f64::hypot(px[j][0] - px[i][0], px[j][1] - px[i][1]) < 50.0 {
                        separated = false;
                    }
                }
            }
            if !separated {
                continue;
            }
            let ab = f64::hypot(px[1][0] - px[0][0], px[1][1] - px[0][1]);
            let cross = f64::abs(
                (px[1][0] - px[0][0]) * (px[2][1] - px[0][1])
                    - (px[1][1] - px[0][1]) * (px[2][0] - px[0][0]),
            );
            let longest = f64::max(
                ab,
                f64::max(
                    f64::hypot(px[2][0] - px[0][0], px[2][1] - px[0][1]),
                    f64::hypot(px[2][0] - px[1][0], px[2][1] - px[1][1]),
                ),
            );
            if cross / ab < 0.05 * longest {
                continue;
            }
            return Some((q, px));
        }
        None
    }

    /// dimquads=4 (any installed 4-star index that declares `CXDX`+`CXDXLT1` -
    /// the flags this code path needs, which the test asserts right after the
    /// load): a synthetic field built by projecting a real quad's stars through a
    /// tangent WCS at the quad's own centroid (see [`pick_dim4_quad`]) must be
    /// solved back to that quad through the C/D permutation + CXDX/CXDXLT1 path,
    /// with sub-pixel residuals and the synthetic plate scale recovered.
    #[test]
    fn dimquads4_synthetic_quad_is_found() {
        let Some(meta) = testutil::dim4_index() else {
            return;
        };
        let index = testutil::load(meta);
        assert_eq!(index.meta.dimquads, 4);
        assert!(index.meta.cx_less_than_dx && index.meta.meanx_less_than_half);

        let (q, px) = pick_dim4_quad(&index).expect("no usable dim4 quad found");

        // Field stars = the projected quad in slot order, so the enumeration's
        // `a < b` backbone is (0, 1); the decreasing flux is fixture detail.
        let field: Vec<Star> = px
            .iter()
            .enumerate()
            .map(|(i, [x, y])| Star {
                x: *x,
                y: *y,
                flux: 1000.0 - i as f64,
            })
            .collect();
        let mut s = spec(&field, None);
        s.max_objs = 4;

        let found: std::sync::Mutex<Option<QuadHit>> = std::sync::Mutex::new(None);
        let stats = solve_index(&index, &s, &|hit| {
            if hit.quad == q {
                *found.lock().unwrap() = Some(hit.clone());
                return Flow::Stop;
            }
            Flow::Continue
        });
        let hit = found
            .into_inner()
            .unwrap()
            .unwrap_or_else(|| panic!("dim4 quad {q} not found; {stats:?}"));
        let res = hit.residuals(&index, &field);
        assert!(res.iter().all(|&r| r < 1.0), "residuals {res:?}");
        assert!(f64::abs(hit.arcsec_per_pix() - FIELD1_SCALE_ARCSEC) < 0.05);
    }

    /// The `process_hit` scale gates use explicit-accept range checks, so a NaN
    /// scale is rejected (the open-range form `v < lo || v > hi` fails open);
    /// finite values must behave identically.
    #[test]
    fn scale_gate_rejects_nan() {
        let range = 1.0..=10.0;
        let old_rejects = |v: f64| v < 1.0 || v > 10.0;
        let new_rejects = |v: f64| !(range).contains(&v);
        assert!(!old_rejects(f64::NAN), "old open-range form failed open");
        assert!(new_rejects(f64::NAN), "explicit-accept form must reject");
        for &v in &[f64::MIN, -1.0, 0.0, 1.0, 5.0, 10.0, 100.0, f64::INFINITY] {
            assert_eq!(old_rejects(v), new_rejects(v), "mismatch at {v}");
        }
    }

    /// The post-fit solution gate (`PositionHint::contains`) is inclusive of the
    /// boundary and - unlike the pre-fit gate, which rejects NaN
    /// (`Searcher::process_hit`) - a NaN sky center PASSES it, because the
    /// comparison is a negation. Nothing else exercises that divergence
    /// (hermetic: no index or field data).
    #[test]
    fn position_hint_contains_boundary_and_nan() {
        let hint = PositionHint {
            ra_deg: 10.0,
            dec_deg: 20.0,
            radius_deg: 5.0,
        };
        // The hint center itself (zero distance) is inside.
        assert!(hint.contains(10.0, 20.0));
        // A point clearly outside the 5 deg circle is rejected.
        assert!(!hint.contains(180.0, -80.0));
        // A NaN center is NOT rejected (a NaN `distsq > r2` comparison is false,
        // so `!(..)` accepts) - deliberately divergent from the pre-fit gate.
        assert!(hint.contains(f64::NAN, 0.0), "NaN ra must pass");
        assert!(hint.contains(10.0, f64::NAN), "NaN dec must pass");
    }

    /// The shipped hot path (`Searcher::triple`/`quad`) inlines the quad-code
    /// formula instead of calling `codes::star_code`, so the unit-test guard in
    /// `codes.rs` would otherwise validate a copy the binary never runs. This
    /// data-free test recomputes the inlined `base` exactly as `backbone` +
    /// `triple`/`quad` build it (including the `* (1.0/s)` reciprocal form) and
    /// pins it against `codes::star_code` to ~1e-12, for a triple and a quad.
    #[test]
    fn inlined_star_code_matches_codes_star_code() {
        // Hand-built backbone A,B and non-backbone stars C,D (pixel coords).
        let (ax, ay) = (120.5, -40.25);
        let (bx, by) = (980.75, 640.0);
        let (cx, cy) = (500.0, 300.5);
        let (ex, ey) = (700.25, 120.0);

        // Inlined exactly as `Searcher::backbone` computes cos/sin, then as
        // `triple`/`quad` combine them with the star offset.
        let dx = bx - ax;
        let dy = by - ay;
        let s = dx * dx + dy * dy;
        let inv = 1.0 / s;
        let cos = (dy + dx) * inv;
        let sin = (dy - dx) * inv;
        let code = |px: f64, py: f64| -> [f64; 2] {
            let (ux, uy) = (px - ax, py - ay);
            [ux * cos + uy * sin, -ux * sin + uy * cos]
        };

        let close = |a: &[f64], b: &[f64]| {
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b) {
                assert!(f64::abs(x - y) < 1e-12, "{x} vs {y}");
            }
        };

        // dimquads=3: base = C's code (as `triple` builds it).
        let triple_base = code(cx, cy);
        let want3 = crate::codes::star_code(ax, ay, bx, by, cx, cy);
        close(&triple_base, &want3);

        // dimquads=4: base = [C's code, D's code] (as `quad` builds it).
        let cc = code(cx, cy);
        let dc = code(ex, ey);
        let quad_base = [cc[0], cc[1], dc[0], dc[1]];
        let w3 = crate::codes::star_code(ax, ay, bx, by, cx, cy);
        let w4 = crate::codes::star_code(ax, ay, bx, by, ex, ey);
        let want4 = [w3[0], w3[1], w4[0], w4[1]];
        close(&quad_base, &want4);
    }
}
