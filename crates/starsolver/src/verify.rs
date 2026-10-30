//! Verification and odds: project the index stars of one fitted code hit into
//! the frame and accumulate the log-odds that the match over the field stars is
//! real.
//!
//! The odds model is the minimal two-term one: each field star contributes
//! `max(match term, distractor term)` against a uniform background over the whole
//! frame (`effA = W*H`), nothing else. Matching is first-come with no conflict
//! resolution - deliberate, and it biases the odds slightly upward, since where a
//! conflict-resolving matcher can deny a star whose reference is already taken,
//! here a fresh full match term is banked - so the frozen ln(1e9) solve gate
//! ([`crate::params::ODDS_TO_SOLVE_LN`]) and this matcher must not be tuned
//! independently. The test list is scanned as delivered (the
//! [`crate::augment::preprocess`] order, capped brightest-first on the CLI path)
//! and never re-shuffled here, so `bestlogodds` is a maximum over its prefixes.
//!
//! Flow:
//! 0. [`verify_probe`] may drop the hit outright: the brightest non-quad
//!    detections are counted against index stars, and a count that cannot be told
//!    apart from chance never reaches step 1. It decides which hits are verified at
//!    all, so it must stay a pure function of `(index, hit, stars, frame)`, never
//!    of rayon scheduling.
//! 1. [`sigma2_base`]: the base per-star variance, px^2; every variance below is a
//!    multiple of it.
//! 2. [`collect_refs`]: index stars from a 3D chord-ball search about the
//!    frame-center pixel, projected and kept only when `0 <= x <= W` and
//!    `0 <= y <= H`, inclusive.
//! 3. The quad's own index stars stay out of the reference list (and out of `nr`):
//!    the WCS was just fitted from them, so matching one reuses evidence. Their
//!    field stars leave the test list only after deduplication.
//! 4. Each field star takes the nearest still-unused reference within
//!    `NN_RADIUS_SIGMA * sigma_i`, `sigma2_i = sigma2 * (1 + R_i^2 / Q^2)`, `R_i`
//!    its distance to the midpoint of the quad backbone `AB`, `Q = |AB|/2`.
//! 5. [`odds_scan_inner`] adds `term - logbg` per star, `term = max(logfg, logd)`,
//!    abandons the hit below `BAIL_THRESHOLD_LN` and stops above `stop_ln` - which
//!    [`verify_hit`] sets to infinity, so a production scan visits the whole list.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::ops::ControlFlow;

use crate::coords::{DEG2ARCSEC, arcsec2rad, distsq, radec_to_xyz};
use crate::index::Index;
use crate::kdtree::KdTree;
use crate::params::{
    BAIL_THRESHOLD_LN, DISTRACTOR_RATIO, INDEX_JITTER_ARCSEC, NN_RADIUS_SIGMA, ODDS_TO_SOLVE_LN,
    PROBE_BALL_MAX_STARS, PROBE_MIN_CAT_STARS, PROBE_MIN_MATCHES, PROBE_MIN_PROBES,
    PROBE_RADIUS_SIGMA, PROBE_RANDOM_FACTOR, PROBE_STARS, VERIFY_PIX,
};
use crate::solver::QuadHit;
use crate::xyls::Star;

/// Outcome of verifying one code hit.
#[derive(Debug, Clone, Copy)]
pub struct VerifyResult {
    /// Peak running log-odds over the prefixes of the scan - not the last
    /// total: the terms are not all positive and the prefix that trips the bail
    /// is never offered.
    pub bestlogodds: f64,
    /// Matches banked up to that best prefix.
    pub nmatch: usize,
    /// Reference stars kept in the frame, quad stars removed. `nr` divides both
    /// odds terms, so it prices a match and a miss.
    pub nr: usize,
    /// Test stars reaching the scan: after deduplication and after the quad's
    /// field stars are dropped.
    pub nt: usize,
    /// Set when the running total fell below `BAIL_THRESHOLD_LN` and the rest of
    /// the list was abandoned.
    pub bailed: bool,
}

impl VerifyResult {
    #[must_use]
    pub fn solved(&self) -> bool {
        self.bestlogodds >= ODDS_TO_SOLVE_LN
    }
}

/// Odds accumulator over the test stars in the order they are handed in (on the
/// CLI path the capped brightest-first list of `xyls::brightest_first`), against
/// the projected references. First come wins: a matched reference star is no
/// longer offered to later test stars.
///
/// Each star contributes `ln(P(observed | solved) / P(observed | unsolved))`, both
/// sides per square pixel: the unsolved side the uniform background `1 / eff_area`
/// (the distractor term), the solved side a Gaussian of variance `testsigma2[i]`
/// at the nearest still-unused reference (the match term). The larger is banked,
/// so a star whose counterpart is missing or far costs odds instead of paying for
/// a match - which makes the traversal order part of the result.
///
/// Returns `(bestlogodds, nmatch_at_best, bailed)`; the `used` bitmap and the
/// x-ordered lookup (`refxs` ascending, `reford` the matching `refxy` indices) are
/// handed in as reusable buffers.
///
/// `logd` depends on the scan state only through `mu`, so it is cached until a
/// match moves `mu`, with the same ln calls the uncached loop made.
///
/// The nearest-free search scans only the references whose x lies within
/// `sqrt(s2 * nnsig2)`: a bigger dx means `d2 > s2 * nnsig2` = the loop's initial
/// `bd2`, which only shrinks, so a full ascending scan could never have picked such
/// a reference. The full scan's winner rule (`d2 <= bd2`, highest index wins an
/// exact tie) is reproduced explicitly below.
// The scratch buffers are threaded through to keep the hot loop allocation-free;
// bundling them would only add a wrapper with no other caller.
#[allow(clippy::too_many_arguments)]
fn odds_scan_inner(
    testxy: &[[f64; 2]],
    testsigma2: &[f64],
    refxy: &[[f64; 2]],
    refxs: &[f64],
    reford: &[u32],
    eff_area: f64,
    bail_ln: f64,
    stop_ln: f64,
    used: &mut Vec<bool>,
) -> (f64, usize, bool) {
    let delta = DISTRACTOR_RATIO;
    let nr = refxy.len();
    if nr == 0 || testxy.is_empty() || eff_area <= 0.0 {
        return (f64::NEG_INFINITY, 0, false);
    }
    let nr = nr as f64;
    // The background: a source placed at random in the frame lands per square
    // pixel with probability 1 / eff_area. Both odds terms below are densities
    // in the same units, so one subtraction of `logbg` turns a term into a
    // likelihood ratio.
    let logbg = f64::ln(1.0 / eff_area);

    used.clear();
    used.resize(refxy.len(), false);
    let mut logodds = 0.0f64;
    let mut mu = 0usize;
    let mut logd = f64::ln(delta + (1.0 - delta) * f64::from(mu as u32) / nr) + logbg;
    let mut bestlogodds = f64::NEG_INFINITY;
    let mut best_mu = 0usize;
    let mut bailed = false;

    let nnsig2 = NN_RADIUS_SIGMA * NN_RADIUS_SIGMA;
    for (i, tp) in testxy.iter().enumerate() {
        let s2 = testsigma2[i];
        // Distractor density. A fraction `delta` of detections is junk outright;
        // the rest are real stars, and a share `mu / nr` of the references is
        // already consumed, so a real star has that chance of finding no free
        // counterpart and adding only noise. A miss therefore costs
        // ln(delta + (1 - delta) * mu / nr): -1.39 nats at the start, rising toward
        // 0 as the scan uses up the references, so the first misses drive a bail.
        // `mu` never exceeds `nr` (one-to-one matches), so the `u32` step into
        // `f64::from` is lossless. Cached on `mu` (see the fn docs).

        // The nearest reference still free within NN_RADIUS_SIGMA (5) sigmas: `bd2`
        // starts as the squared search radius and only shrinks, so the loop returns
        // the nearest one rather than any one. The x-window is a sound restriction
        // (see the fn docs); a NaN sigma or test coordinate empties it, matching the
        // `d2 <= bd2` comparisons never firing in the full scan.
        let r = f64::sqrt(s2 * nnsig2);
        let lo = refxs.partition_point(|&x| x < tp[0] - r);
        let hi = refxs.partition_point(|&x| x <= tp[0] + r);
        let mut bd2 = s2 * nnsig2;
        let mut bi = usize::MAX;
        for &k in &reford[lo..hi] {
            let j = k as usize;
            if used[j] {
                continue;
            }
            let rp = refxy[j];
            let dx = tp[0] - rp[0];
            let dy = tp[1] - rp[1];
            let d2 = dx * dx + dy * dy;
            if d2 < bd2 || (d2 == bd2 && (bi == usize::MAX || j > bi)) {
                bd2 = d2;
                bi = j;
            }
        }

        let term = if bi == usize::MAX {
            logd
        } else {
            // The match density: the peak of a 2D Gaussian of variance `s2`
            // (1 / 2pi*s2) scaled by the prior (1 - delta) / nr that this
            // detection is the counterpart of one particular reference star,
            // evaluated at the offset found (`bd2` is 0 for an exact overlap,
            // where the term is largest).
            let loggmax = f64::ln((1.0 - delta) / (2.0 * PI * s2 * nr));
            let logfg = loggmax - bd2 / (2.0 * s2);
            if logfg > logd {
                used[bi] = true;
                mu += 1;
                logd = f64::ln(delta + (1.0 - delta) * f64::from(mu as u32) / nr) + logbg;
                logfg
            } else {
                // Too weak to beat the distractor term: the star contributes
                // noise and the reference stays free for later stars.
                logd
            }
        };

        logodds += term - logbg;
        // The bail check runs before the best-prefix bookkeeping, so the prefix
        // that tripped the floor never counts as this hit's best.
        if logodds < bail_ln {
            bailed = true;
            break;
        }
        if logodds > bestlogodds {
            bestlogodds = logodds;
            best_mu = mu;
        }
        if logodds > stop_ln {
            break;
        }
    }
    (bestlogodds, best_mu, bailed)
}

/// Reusable per-worker buffers of [`verify_hit`] and [`verify_probe`]. Every field
/// is cleared before use, so a call only ever reads what it wrote; reusing them
/// across the ~10^6 hits of a sweep removes the per-hit allocations without
/// touching the arithmetic. Each pool worker (not each hit) keeps their high-water
/// capacity for the rest of the run - a few tens of KB at shipped-field sizes.
///
/// `verify_hit` holds this `RefCell` borrow for its whole call, so no rayon work
/// may be spawned from inside an attempt.
#[derive(Default)]
struct Scratch {
    /// The in-frame survivors of the star ball search, `(dist2, [x, y])`,
    /// ascending `dist2`; [`verify_hit`] copies the coordinates into `refxy`.
    refs: Vec<(f64, [f64; 2])>,
    refxy: Vec<[f64; 2]>,
    refxs: Vec<f64>,
    reford: Vec<u32>,
    sigma2s: Vec<f64>,
    keep: Vec<bool>,
    testxy: Vec<[f64; 2]>,
    testsigma2: Vec<f64>,
    used: Vec<bool>,
    /// The star coordinates inside the footprint ball of [`verify_probe`]'s
    /// batched lookup. Its collector clears it on entry to every collection, and
    /// [`verify_probe`] clears it on entry to every call (the abstain path
    /// included), so no hit can read another hit's ball.
    probe: Vec<[f64; 3]>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = const {
        RefCell::new(Scratch {
            refs: Vec::new(),
            refxy: Vec::new(),
            refxs: Vec::new(),
            reford: Vec::new(),
            sigma2s: Vec::new(),
            keep: Vec::new(),
            testxy: Vec::new(),
            testsigma2: Vec::new(),
            used: Vec::new(),
            probe: Vec::new(),
        })
    };
}

/// The NN lookup order of [`odds_scan_inner`]: `reford` = reference indices
/// ascending by x (stable), `refxs` the same x values beside them for
/// `partition_point`.
fn x_order(refxy: &[[f64; 2]], reford: &mut Vec<u32>, refxs: &mut Vec<f64>) {
    reford.clear();
    reford.extend(0..refxy.len() as u32);
    reford.sort_by(|&a, &b| f64::total_cmp(&refxy[a as usize][0], &refxy[b as usize][0]));
    refxs.clear();
    refxs.extend(reford.iter().map(|&i| refxy[i as usize][0]));
}

/// The per-field precomputations of [`verify_hit`]: the field indices sorted by x
/// and their x values in that order. They depend on the star list only, never on a
/// hit, so the sweep builds them once per field; the arrays are exactly the ones
/// the per-hit code used to build, so the dedup outcome is bit-identical.
pub struct FieldPre {
    byx: Vec<u32>,
    xs: Vec<f64>,
}

impl FieldPre {
    #[must_use]
    pub fn new(stars: &[Star]) -> Self {
        let mut byx: Vec<u32> = (0..stars.len() as u32).collect();
        byx.sort_by(|&a, &b| f64::total_cmp(&stars[a as usize].x, &stars[b as usize].x));
        let xs: Vec<f64> = byx.iter().map(|&i| stars[i as usize].x).collect();
        FieldPre { byx, xs }
    }
}

/// Reference stars of one fitted hit: the star-ball hits that are not the quad's
/// own index stars and project inside the frame, ordered by ascending ball
/// distance - the order `ball_search` applies on its way out, part of the
/// first-come matching semantics, so it is frozen.
///
/// The candidate cap is a ball in 3D chord space about the sky position of the
/// frame-center pixel `(W/2, H/2)`, radius = the chord to the `(0, 0)` corner - a
/// pre-filter, not a strict bound: the fitted WCS puts its tangent point at the
/// matched quad's pixel centroid (see `fit_tan_wcs`), so chord distance from the
/// center pixel is not monotone in pixel distance and `(0, 0)` is not generally
/// the frame's farthest point on the sky; on the shipped field-1 WCS (tangent
/// point ~650 px off the frame center) that corner pokes 4.8 arcsec (about 1.8 px)
/// beyond the ball.
/// Stars in the frame but outside the ball are lost here; sky the ball reaches
/// outside the frame is cut by the pixel test below, inclusive at both ends in
/// 0-based pixels - one pixel looser at the top than the 1-based FITS convention,
/// and frozen because `nr` divides both odds terms.
///
/// Survivors are collected by [`KdTree::ball_search_visit`] rather than
/// materialized: a garbage fit's ball can reach the whole star table while almost
/// none of it lands in the frame. Sorting only the survivors still equals sorting
/// every hit and then filtering: `sort_by` is stable, so equal keys keep their
/// traversal order either way and the survivor subsequence [`odds_scan_inner`]
/// matches against is identical element for element.
fn collect_refs(
    index: &Index,
    hit: &QuadHit,
    width: f64,
    height: f64,
    refs: &mut Vec<(f64, [f64; 2])>,
) {
    let center = {
        let (ra, dec) = hit.wcs.pix_to_radec(width / 2.0, height / 2.0);
        radec_to_xyz(ra, dec)
    };
    let corner = {
        let (ra, dec) = hit.wcs.pix_to_radec(0.0, 0.0);
        radec_to_xyz(ra, dec)
    };
    let radius = f64::sqrt(distsq(&center, &corner));
    // A quad row holds star-table row numbers, which is what the star tree
    // reports as a hit index, so the comparison is direct.
    let quad_ids = index.quad(hit.quad);
    let dimquads = index.meta.dimquads;
    // Trig-free projection: the map folds the tangent frame and the CD inverse
    // into dot products, agreeing with `radec_to_pix(xyz_to_radec(..))` to
    // rounding (pinned by `wcs::tests::xyz_to_pix_map_matches_radec_to_pix`);
    // the LSB drift stays far below any margin of the solve gate.
    let proj = hit.wcs.xyz_to_pix_map();
    refs.clear();
    index
        .stars
        .ball_search_visit(&center, radius, |rec, dist2| {
            if quad_ids[..dimquads].contains(&(rec as u32)) {
                return;
            }
            let (x, y) = proj.project(index.star_xyz(rec));
            if (0.0..=width).contains(&x) && (0.0..=height).contains(&y) {
                refs.push((dist2, [x, y]));
            }
        });
    refs.sort_by(|a, b| f64::total_cmp(&a.0, &b.0));
}

/// Verifies a fitted code hit against the index star list.
///
/// `pre` must be a [`FieldPre::new`] of the very `stars` slice handed in here:
/// the dedup window is looked up in its pre-sorted copy of the same list.
#[must_use]
pub fn verify_hit(
    index: &Index,
    hit: &QuadHit,
    stars: &[Star],
    pre: &FieldPre,
    width: f64,
    height: f64,
) -> VerifyResult {
    debug_assert_eq!(
        pre.xs.len(),
        stars.len(),
        "FieldPre built from a different star list"
    );
    let arcsec = hit.arcsec_per_pix();
    // px^2, the base of every per-star variance below.
    let sigma2 = sigma2_base(arcsec);

    SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        let Scratch {
            refs,
            refxy,
            refxs,
            reford,
            sigma2s,
            keep,
            testxy,
            testsigma2,
            used,
            // The probe's ball buffer: `verify_probe` owns and clears it, and
            // this call neither reads nor writes it.
            probe: _,
        } = &mut *scratch;
        // Ball hits that project inside the frame, minus the quad's own index
        // stars (see `collect_refs`).
        collect_refs(index, hit, width, height, refs);
        refxy.clear();
        refxy.extend(refs.iter().map(|&(_, p)| p));
        let nr = refxy.len();
        // The NN lookup order of `odds_scan_inner` (see `x_order`).
        x_order(refxy, reford, refxs);

        // Per-test-star variance sigma2_i = sigma2 * (1 + R_i^2 / Q2), Q2 the
        // squared half-backbone AB (midpoint-to-A): stars far from the ones the fit
        // was pinned to project less accurately, so a star one half-backbone from
        // the midpoint already carries twice the variance and its match Gaussian is
        // lower and wider.
        let qa = &stars[hit.field_stars[0]];
        let qb = &stars[hit.field_stars[1]];
        let qc = [(qa.x + qb.x) / 2.0, (qa.y + qb.y) / 2.0];
        let q2 = {
            let dx = qa.x - qc[0];
            let dy = qa.y - qc[1];
            dx * dx + dy * dy
        };
        let inv_q2 = if q2 > 0.0 { 1.0 / q2 } else { 0.0 };
        sigma2s.clear();
        sigma2s.extend(stars.iter().map(|s| {
            let r2 = f64::powi(s.x - qc[0], 2) + f64::powi(s.y - qc[1], 2);
            sigma2 * (1.0 + r2 * inv_q2)
        }));

        // Deduplication: a star within one sigma of an earlier kept star is
        // dropped, at that earlier star's own sigma2 as the radius. "Earlier" is the
        // incoming list order, so the kept set depends on that order.
        //
        // Neighbours come from a copy of the indices sorted by x, so the two
        // partition_point calls hand back the x-window [xi-r, xi+r] and only those
        // stars get the real 2D test; the radii are a pixel or two near the quad
        // (sigma2 grows away from it), so the window stays short.
        let nstars = stars.len();
        keep.clear();
        keep.resize(nstars, true);
        if nstars > 1 {
            let byx = &pre.byx;
            let xs = &pre.xs;
            for i in 0..nstars {
                if !keep[i] {
                    continue;
                }
                let r = f64::sqrt(sigma2s[i]);
                let (xi, yi) = (stars[i].x, stars[i].y);
                let lo = xs.partition_point(|&x| x < xi - r);
                let hi = xs.partition_point(|&x| x <= xi + r);
                for &j32 in &byx[lo..hi] {
                    let j = j32 as usize;
                    // Only later entries can be dropped, and only by a star still
                    // kept when its turn comes (the outer loop skips the rest), so
                    // a dropped star never removes anything in turn.
                    if j <= i || !keep[j] {
                        continue;
                    }
                    let dx = xi - stars[j].x;
                    let dy = yi - stars[j].y;
                    if dx * dx + dy * dy <= sigma2s[i] {
                        keep[j] = false;
                    }
                }
            }
        }
        // The quad's field stars leave the test list only now, AFTER dedup: while
        // dedup runs they can still suppress a nearby detection, and that
        // suppression is part of the odds a run reproduces. Slicing to dimquads
        // matters - for a 3-star index the unused 4th slot holds the 0 sentinel,
        // which would silently drop star 0.
        for &slot in &hit.field_stars[..index.meta.dimquads] {
            keep[slot] = false;
        }

        testxy.clear();
        testsigma2.clear();
        for (i, s) in stars.iter().enumerate() {
            if keep[i] {
                testxy.push([s.x, s.y]);
                testsigma2.push(sigma2s[i]);
            }
        }
        let nt = testxy.len();

        // No early stop: stop_ln is +infinity, so the scan runs the whole list and
        // the solve decision is the caller's job - the sweep's capture takes hits
        // whose log-odds reach ODDS_TO_SOLVE_LN (see VerifyResult::solved) and keeps
        // the lexicographic best of the attempt, and it deliberately never
        // interrupts the attempt that solves, so stdout does not depend on the order
        // in which parallel workers reach their hits.
        //
        // Traversal order is part of the odds definition: bestlogodds is a maximum
        // over the prefixes of the incoming list order - the preprocessed .xyls
        // order from augment::preprocess - and nothing re-sorts it here.
        let (bestlogodds, nmatch, bailed) = odds_scan_inner(
            testxy,
            testsigma2,
            refxy,
            refxs,
            reford,
            width * height,
            BAIL_THRESHOLD_LN,
            f64::INFINITY,
            used,
        );
        VerifyResult {
            bestlogodds,
            nmatch,
            nr,
            nt,
            bailed,
        }
    })
}

/// Base per-star variance of the verification model, px^2: the field centroid
/// uncertainty [`VERIFY_PIX`] in quadrature with the index position uncertainty
/// [`INDEX_JITTER_ARCSEC`] converted to pixels by the fitted scale. [`verify_hit`]
/// and the probe read it here so the two cannot drift; the probe uses it UNWIDENED
/// while `verify_hit` widens it per star, deliberately, see `PROBE_RADIUS_SIGMA`.
#[must_use]
pub(crate) fn sigma2_base(arcsec_per_pix: f64) -> f64 {
    VERIFY_PIX * VERIFY_PIX + f64::powi(INDEX_JITTER_ARCSEC / arcsec_per_pix, 2)
}

/// The `CAP` best candidates by descending `key`, in `out`, their count as the
/// return value. `key` returning `None` rejects a star.
fn top_k<const CAP: usize>(
    stars: &[Star],
    mut key: impl FnMut(&Star) -> Option<f64>,
    out: &mut [usize; CAP],
) -> usize {
    let mut keys = [-f64::INFINITY; CAP];
    let mut len = 0usize;
    for (i, s) in stars.iter().enumerate() {
        let Some(k) = key(s) else { continue };
        // `xyls::load` guarantees a finite flux, so this only has to hold for
        // some future key: it keeps a NaN from silently ordering its way into
        // the probe set.
        if !k.is_finite() || (len == CAP && k <= keys[CAP - 1]) {
            continue;
        }
        // Insertion into the descending top-K; rejects above keep the shifts
        // rare for a long list.
        let mut j = usize::min(len, CAP - 1);
        while j > 0 && keys[j - 1] < k {
            keys[j] = keys[j - 1];
            out[j] = out[j - 1];
            j -= 1;
        }
        keys[j] = k;
        out[j] = i;
        len = usize::min(len + 1, CAP);
    }
    len
}

/// Cheap sanity check of a fitted hit, run before [`verify_hit`]: do the brightest
/// non-quad field stars land on index stars at all?
///
/// Returns `false` only to say "this similarity is garbage, do not spend a
/// verification on it": survivors go to `verify_hit` untouched, so a `true` here
/// never commits anything. The answer is a pure function of
/// `(index, hit, stars, cands, width, height)` - no clock, no thread, no
/// `--ra/--dec` - so which hits get verified cannot depend on how the rayon pool
/// scheduled them.
///
/// Most code hits come from a code match whose tolerance is loose next to the
/// fitted scale and land nowhere near the truth: their odds scan runs to the end of
/// the star list (the bail never fires) for zero matches. The probe buys that scan
/// back by counting how many probe stars land on a catalog star at all, so its own
/// setup must stay small: candidates from [`probe_stars`] once per attempt, the
/// pixel -> sky map once per hit, and the lookups folded into one ball search over
/// the fitted footprint whenever the frame is sparse enough (see
/// [`PROBE_BALL_MAX_STARS`]) instead of one tree descent per probe.
pub fn verify_probe(
    index: &Index,
    hit: &QuadHit,
    stars: &[Star],
    cands: &ProbeStars,
    width: f64,
    height: f64,
) -> bool {
    // The abstain path returns before the collector runs, so the shared scratch
    // is emptied here too: a hit that only abstains must not leave the previous
    // hit's ball behind. The batched path is safe without this (its collector
    // clears `probe` on entry), so this covers exactly the paths that never reach
    // the collector.
    SCRATCH.with(|cell| cell.borrow_mut().probe.clear());
    let Some(plan) = probe_setup(index, hit, stars, cands, width, height) else {
        return true;
    };
    // Borrowed only for the collection, not across `probe_setup`. The batched
    // path answers exactly what the per-probe searches answer (see
    // [`probe_via_ball`]); the guard only picks the cheaper path, and
    // `probe_via_ball` aborts on its own when the ball is bigger than the guard
    // estimated.
    let batched = SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        probe_batched(&index.stars, &plan, &mut scratch.probe)
    });
    if let Some(verdict) = batched {
        return verdict;
    }
    probe_via_searches(&index.stars, &plan)
}

/// The batched verdict for a planned hit: `None` when the frame's estimated
/// catalog content is above [`PROBE_BALL_MAX_STARS`] (the guard keeping dense
/// frames off the batched path) or when [`probe_via_ball`] found the ball
/// unusable - both mean "use [`probe_via_searches`]".
fn probe_batched(stars: &KdTree, plan: &ProbePlan, buf: &mut Vec<[f64; 3]>) -> Option<bool> {
    if plan.cat_in_frame > PROBE_BALL_MAX_STARS as f64 {
        return None;
    }
    probe_via_ball(stars, plan, buf)
}

/// Test hook: the verdicts of the two probe paths for one hit as
/// `(per-probe searches, batched ball, cat_in_frame)`. `None` is the probe
/// abstaining; a `None` ball is the batched path declining the hit, which is
/// exactly when [`verify_probe`] runs the searches.
#[cfg(test)]
pub(crate) fn probe_paths(
    index: &Index,
    hit: &QuadHit,
    stars: &[Star],
    cands: &ProbeStars,
    width: f64,
    height: f64,
) -> Option<(bool, Option<bool>, f64)> {
    let plan = probe_setup(index, hit, stars, cands, width, height)?;
    let searches = probe_via_searches(&index.stars, &plan);
    let mut buf = Vec::new();
    Some((
        searches,
        probe_batched(&index.stars, &plan, &mut buf),
        plan.cat_in_frame,
    ))
}

/// Everything both probe paths need from one hit, computed once. Every live probe
/// is projected up front - the batched ball radius scans all of `qs`, so the
/// projections cannot be shared lazily - which costs an accepting hit that lands on
/// the fallback all of its projections, at most `PROBE_STARS` of them.
struct ProbePlan {
    /// Catalog stars under the frame from the index's tile-mean density: the
    /// estimate the batched lookup is gated on.
    cat_in_frame: f64,
    /// The probe detections projected through the fitted WCS, in probe order. A
    /// garbage fit hands back non-finite vectors, which never count as matches.
    qs: [[f64; 3]; PROBE_STARS],
    /// How many entries of `qs` are live (at most [`PROBE_STARS`]).
    probes: usize,
    /// Squared chord radius of the probe lookup, in the star tree's own units.
    cutoff2: f64,
    /// Matches that put the count well above what chance would bank.
    need: usize,
    /// Sky position of the frame-center pixel: the center of the batched ball.
    center: [f64; 3],
    /// Radius of the ball about `center` that holds every probe's lookup ball,
    /// or `f64::NAN` when no such ball can be computed and the batched path must
    /// be skipped.
    radius: f64,
}

/// The probe's shared setup: the abstain gates, the probe list, the lookup
/// cutoff, the acceptance count and the footprint ball. `None` is the probe
/// abstaining - the hit goes to [`verify_hit`] unjudged.
fn probe_setup(
    index: &Index,
    hit: &QuadHit,
    stars: &[Star],
    cands: &ProbeStars,
    width: f64,
    height: f64,
) -> Option<ProbePlan> {
    // A catalog of unknown surface density cannot price a chance coincidence, and
    // a frame too small to hold enough catalog stars for a count to mean anything
    // cannot be judged: both abstain.
    let density = index.meta.stars_per_deg2()?;
    let arcsec = hit.arcsec_per_pix();
    let deg_per_pix = arcsec / DEG2ARCSEC;
    let cat_in_frame = density * width * height * deg_per_pix * deg_per_pix;
    if !(cat_in_frame.is_finite() && cat_in_frame >= PROBE_MIN_CAT_STARS) {
        return None;
    }

    // The quad's own field stars leave the probe set: the fit was pinned to
    // them, so they match by construction and carry no evidence.
    let quad = &hit.field_stars[..index.meta.dimquads];
    let mut sel = [0usize; PROBE_STARS];
    let mut probes = 0usize;
    for &i in &cands.idx {
        if quad.contains(&i) {
            continue;
        }
        sel[probes] = i;
        probes += 1;
        if probes == PROBE_STARS {
            break;
        }
    }
    if probes < PROBE_MIN_PROBES {
        return None;
    }

    // Probe radius in the star tree's own units: chords on the unit sphere,
    // taken from the UNWIDENED sigma (see PROBE_RADIUS_SIGMA for why).
    let sigma = f64::sqrt(sigma2_base(arcsec));
    let theta = arcsec2rad(PROBE_RADIUS_SIGMA * sigma * arcsec);
    let chord = 2.0 * f64::sin(theta / 2.0);
    let cutoff2 = chord * chord;

    // What a WCS of the right scale but a wrong pointing would bank by luck over
    // these probes, and the count that puts a hit well above it.
    // Density-normalized: the chance rate per probe varies by four orders of
    // magnitude between index families.
    let probe_area_deg2 = PI * f64::powi(PROBE_RADIUS_SIGMA * sigma * deg_per_pix, 2);
    let e_random = probes as f64 * density * probe_area_deg2;
    let need = usize::max(
        PROBE_MIN_MATCHES,
        f64::ceil(PROBE_RANDOM_FACTOR * e_random) as usize,
    );
    // A frame so sparse that even all its probes together could not reach `need`
    // cannot be judged: a good fit there would be rejected for lack of catalog
    // stars to land on, so the probe abstains.
    if need > probes {
        return None;
    }

    let proj = hit.wcs.pix_to_xyz_map();
    let mut qs = [[0.0f64; 3]; PROBE_STARS];
    for (i, &star) in sel[..probes].iter().enumerate() {
        qs[i] = proj.project(stars[star].x, stars[star].y);
    }
    let center = proj.project(width / 2.0, height / 2.0);
    // The batched ball: centered on the frame, large enough to hold every
    // probe's own lookup ball, so one traversal replaces the per-probe searches.
    let radius = probe_ball_radius(&center, &qs[..probes], chord);
    Some(ProbePlan {
        cat_in_frame,
        qs,
        probes,
        cutoff2,
        need,
        center,
        radius,
    })
}

/// Slack on the batched ball's radius. Its parts come from different code paths,
/// so the ball must be a few ulps roomier than the exact triangle-inequality
/// bound: with too little slack a witness lands outside the collected ball and
/// the probe's verdict flips. `1e-12` relative swamps the ~1e-15 accumulated
/// rounding and stays negligible against the collection cap.
const BALL_MARGIN: f64 = 1.0 + 1e-12;

/// The radius of the smallest ball about `center` that contains every probe's
/// lookup ball of radius `chord`, or `f64::NAN` when a non-finite projection makes
/// that question unanswerable (and the batched path must be skipped).
///
/// Both radii are chords - the Euclidean metric of the tree's xyz space - so the
/// superset property is the triangle inequality and `sqrt` is monotone; the extra
/// room is [`BALL_MARGIN`].
fn probe_ball_radius(center: &[f64; 3], qs: &[[f64; 3]], chord: f64) -> f64 {
    let mut far2 = 0.0;
    let mut usable = all_finite(center);
    for q in qs {
        usable &= all_finite(q);
        far2 = f64::max(far2, distsq(q, center));
    }
    if usable {
        (f64::sqrt(far2) + chord) * BALL_MARGIN
    } else {
        f64::NAN
    }
}

/// A projected unit vector with no NaN or infinity in it.
fn all_finite(v: &[f64; 3]) -> bool {
    v[0].is_finite() && v[1].is_finite() && v[2].is_finite()
}

/// Collects the dequantized coordinates of the stars inside the ball
/// `center`/`radius` into `buf`, answering `false` and emptying `buf` again once
/// they would pass `cap` (the caller's own ceiling).
///
/// The collector owns the buffer's state - it empties `buf` on entry and on the
/// abort path - so no caller can leak an earlier ball into it. The abort bounds the
/// cost: the caller's estimate of the ball's content is a tile mean, which bounds
/// neither the traversal nor the population inside it. The coordinates come from
/// [`KdTree::coord3`], which is what [`Index::star_xyz`] returns, so they are the
/// ones the per-probe searches measure.
fn probe_ball_coords(
    tree: &KdTree,
    center: &[f64; 3],
    radius: f64,
    cap: usize,
    buf: &mut Vec<[f64; 3]>,
) -> bool {
    buf.clear();
    debug_assert!(
        buf.is_empty(),
        "the probe ball buffer is cleared by its collector"
    );
    let collected = tree.try_ball_search_visit(center, radius, |rec, _d2| {
        buf.push(tree.coord3(rec));
        if buf.len() > cap {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    if collected {
        return true;
    }
    buf.clear();
    false
}

/// The probe count from one ball search over the fitted footprint: collect the
/// ball's catalog stars once, then ask each probe whether any of them lies inside
/// its own lookup ball.
///
/// `None` says the ball was not usable and the caller must fall back to
/// [`probe_via_searches`]; the buffer comes back empty for that answer.
///
/// The verdict is identical to `probe_via_searches`' by construction:
///
/// - the ball is a superset of every probe's lookup ball: a star `s` that
///   `nearest_within` could report for probe `q` satisfies `|s - q| <= chord`
///   (boundary included, up to the ulp [`BALL_MARGIN`] covers), so
///   `|s - center| <= chord + |q - center| <= chord + sqrt(far2) <= radius` - and,
///   like the descent's own pruning, this assumes a leaf's records respect its
///   split planes;
/// - [`within_chord2`] decides a pair by the same squared chord `dist2_cutoff`
///   accumulates: same dimensions, same order, same inclusive `<= cutoff2`;
/// - the counting is `probe_via_searches`' loop line for line, so the order the
///   probes are walked in stays irrelevant.
fn probe_via_ball(tree: &KdTree, plan: &ProbePlan, buf: &mut Vec<[f64; 3]>) -> Option<bool> {
    // A non-finite lookup is not answerable by collection: `nearest_within`
    // refuses a non-finite cutoff and reports no match, while `<= +inf` would
    // call every collected star a match. A NaN radius already means a non-finite
    // projection (see `probe_setup`).
    if !plan.radius.is_finite() || !plan.cutoff2.is_finite() {
        return None;
    }
    if !probe_ball_coords(tree, &plan.center, plan.radius, PROBE_BALL_MAX_STARS, buf) {
        return None;
    }
    let mut matches = 0usize;
    for (done, q) in plan.qs[..plan.probes].iter().enumerate() {
        if buf.iter().any(|s| within_chord2(q, s, plan.cutoff2)) {
            matches += 1;
            if matches >= plan.need {
                return Some(true);
            }
        }
        // Even every remaining probe cannot reach `need`: reject now. Exact -
        // the loop above cannot have returned `true` from here on.
        if matches + (plan.probes - done - 1) < plan.need {
            return Some(false);
        }
    }
    Some(false)
}

/// The probe count from one [`KdTree::nearest_within`] existence test per probe:
/// the reference the batched path is checked against, and the path a hit takes
/// whenever the ball is not usable. Its loop is deliberately a separate copy of
/// the counting: the two-paths-agree pins prove nothing if the batched path
/// shared the code it is compared against.
fn probe_via_searches(tree: &KdTree, plan: &ProbePlan) -> bool {
    let mut matches = 0usize;
    for (done, q) in plan.qs[..plan.probes].iter().enumerate() {
        let on_index_star = all_finite(q) && tree.nearest_within(q, plan.cutoff2).is_some();
        if on_index_star {
            matches += 1;
            if matches >= plan.need {
                return true;
            }
        }
        // Even every remaining probe cannot reach `need`: reject now. Exact -
        // the loop above cannot have returned `true` from here on.
        if matches + (plan.probes - done - 1) < plan.need {
            return false;
        }
    }
    false
}

/// `distsq(q, s) <= cutoff2` in `dist2_cutoff`'s accumulate-and-bail order: a
/// partial sum past `cutoff2` cannot come back down, so the dimensions after the
/// bail are not worth reading and dropping them cannot change the answer. A
/// non-finite component makes every comparison false, the same "no match" the
/// full sum gives.
#[inline]
fn within_chord2(q: &[f64; 3], s: &[f64; 3], cutoff2: f64) -> bool {
    let dx = q[0] - s[0];
    let mut d2 = dx * dx;
    if d2 > cutoff2 {
        return false;
    }
    let dy = q[1] - s[1];
    d2 += dy * dy;
    if d2 > cutoff2 {
        return false;
    }
    let dz = q[2] - s[2];
    d2 += dz * dz;
    d2 <= cutoff2
}

/// The batched lookup's internals, reachable from `kdtree`'s differential pin
/// (`kdtree::tests::batched_probe_ball_matches_nearest_within`) under `cfg(test)`
/// only. Production keeps these private; the pin drives the very functions the
/// batched path runs, so the seam forwards instead of mirroring them.
#[cfg(test)]
pub(crate) mod probe_pin {
    use super::KdTree;

    pub(crate) fn probe_ball_radius(center: &[f64; 3], qs: &[[f64; 3]], chord: f64) -> f64 {
        super::probe_ball_radius(center, qs, chord)
    }

    pub(crate) fn probe_ball_coords(
        tree: &KdTree,
        center: &[f64; 3],
        radius: f64,
        cap: usize,
        buf: &mut Vec<[f64; 3]>,
    ) -> bool {
        super::probe_ball_coords(tree, center, radius, cap, buf)
    }

    pub(crate) fn within_chord2(q: &[f64; 3], s: &[f64; 3], cutoff2: f64) -> bool {
        super::within_chord2(q, s, cutoff2)
    }
}

/// How far past [`PROBE_STARS`] the candidate list of [`probe_stars`] reaches:
/// slack for the quad's own stars, which only become known per hit and are
/// dropped there. Index files use 3 or 4 stars per quad; a larger `dimquads`
/// just leaves the probe with fewer probes to count.
const PROBE_SLACK: usize = 4;

/// The probe's candidate detections of one field star list, ordered once per
/// field next to [`FieldPre`] rather than once per hit: the brightest
/// [`PROBE_STARS`] + [`PROBE_SLACK`] entries by descending flux.
pub struct ProbeStars {
    idx: Vec<usize>,
}

/// [`ProbeStars`] for `stars`. When the FLUX column is absent every flux is
/// `0.0`, so `top_k`'s stable insertion order degenerates to list order and the
/// probe reads the first detections of the list.
#[must_use]
pub fn probe_stars(stars: &[Star]) -> ProbeStars {
    let mut buf = [0usize; PROBE_STARS + PROBE_SLACK];
    let n = top_k(stars, |s| Some(s.flux), &mut buf);
    ProbeStars {
        idx: buf[..n].to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use crate::wcs::{Wcs, test_data::FIELD1_SCALE_ARCSEC};

    /// The pre-`collect_refs` `verify_hit`, kept as the frozen reference copy of the
    /// algorithm it replaced: the ball is materialized with
    /// [`KdTree::ball_search_into`] and the whole hit list sorted by distance before
    /// filtering into `refxy`; everything else (sigma2, the dedup window, the test
    /// list, the odds scan) is the same arithmetic on fresh buffers rather than the
    /// thread-local `Scratch`. It also returns the `refxy` sequence - the input of
    /// first-come matching - which the survivor-collecting code must reproduce.
    fn verify_hit_reference(
        index: &Index,
        hit: &QuadHit,
        stars: &[Star],
        width: f64,
        height: f64,
    ) -> (VerifyResult, Vec<[f64; 2]>) {
        let arcsec = hit.arcsec_per_pix();
        let sigma2 = VERIFY_PIX * VERIFY_PIX + f64::powi(INDEX_JITTER_ARCSEC / arcsec, 2);
        let center = {
            let (ra, dec) = hit.wcs.pix_to_radec(width / 2.0, height / 2.0);
            radec_to_xyz(ra, dec)
        };
        let corner = {
            let (ra, dec) = hit.wcs.pix_to_radec(0.0, 0.0);
            radec_to_xyz(ra, dec)
        };
        let radius = f64::sqrt(distsq(&center, &corner));
        let quad_ids = index.quad(hit.quad);
        let dimquads = index.meta.dimquads;
        let proj = hit.wcs.xyz_to_pix_map();

        // Materialize the whole ball, sort it, then filter: the order of work
        // `collect_refs` replaced.
        let mut ballhits: Vec<crate::kdtree::Hit> = Vec::new();
        index.stars.ball_search_into(&center, radius, &mut ballhits);
        ballhits.sort_by(|a, b| f64::total_cmp(&a.dist2, &b.dist2));
        let mut refxy: Vec<[f64; 2]> = Vec::new();
        for h in ballhits.iter() {
            if quad_ids[..dimquads].contains(&(h.index as u32)) {
                continue;
            }
            let (x, y) = proj.project(index.star_xyz(h.index));
            if (0.0..=width).contains(&x) && (0.0..=height).contains(&y) {
                refxy.push([x, y]);
            }
        }
        let nr = refxy.len();
        let mut reford = Vec::new();
        let mut refxs = Vec::new();
        x_order(&refxy, &mut reford, &mut refxs);

        let qa = &stars[hit.field_stars[0]];
        let qb = &stars[hit.field_stars[1]];
        let qc = [(qa.x + qb.x) / 2.0, (qa.y + qb.y) / 2.0];
        let q2 = {
            let dx = qa.x - qc[0];
            let dy = qa.y - qc[1];
            dx * dx + dy * dy
        };
        let inv_q2 = if q2 > 0.0 { 1.0 / q2 } else { 0.0 };
        let sigma2s: Vec<f64> = stars
            .iter()
            .map(|s| {
                let r2 = f64::powi(s.x - qc[0], 2) + f64::powi(s.y - qc[1], 2);
                sigma2 * (1.0 + r2 * inv_q2)
            })
            .collect();

        let pre = FieldPre::new(stars);
        let nstars = stars.len();
        let mut keep = vec![true; nstars];
        if nstars > 1 {
            for i in 0..nstars {
                if !keep[i] {
                    continue;
                }
                let r = f64::sqrt(sigma2s[i]);
                let (xi, yi) = (stars[i].x, stars[i].y);
                let lo = pre.xs.partition_point(|&x| x < xi - r);
                let hi = pre.xs.partition_point(|&x| x <= xi + r);
                for &j32 in &pre.byx[lo..hi] {
                    let j = j32 as usize;
                    if j <= i || !keep[j] {
                        continue;
                    }
                    let dx = xi - stars[j].x;
                    let dy = yi - stars[j].y;
                    if dx * dx + dy * dy <= sigma2s[i] {
                        keep[j] = false;
                    }
                }
            }
        }
        for &slot in &hit.field_stars[..dimquads] {
            keep[slot] = false;
        }
        let mut testxy = Vec::new();
        let mut testsigma2 = Vec::new();
        for (i, s) in stars.iter().enumerate() {
            if keep[i] {
                testxy.push([s.x, s.y]);
                testsigma2.push(sigma2s[i]);
            }
        }
        let nt = testxy.len();
        let mut used = Vec::new();
        let (bestlogodds, nmatch, bailed) = odds_scan_inner(
            &testxy,
            &testsigma2,
            &refxy,
            &refxs,
            &reford,
            width * height,
            BAIL_THRESHOLD_LN,
            f64::INFINITY,
            &mut used,
        );
        (
            VerifyResult {
                bestlogodds,
                nmatch,
                nr,
                nt,
                bailed,
            },
            refxy,
        )
    }

    /// The `refxy` the production path builds, straight out of `collect_refs`.
    fn refxy_new(index: &Index, hit: &QuadHit, width: f64, height: f64) -> Vec<[f64; 2]> {
        let mut refs = Vec::new();
        collect_refs(index, hit, width, height, &mut refs);
        refs.into_iter().map(|(_, p)| p).collect()
    }

    /// Collecting only the ball's survivors and sorting those must hand the matcher
    /// the same `refxy` as the old materialize-and-sort-everything path, and so the
    /// same `VerifyResult`. Checked on a real winning hit of a shipped field and on
    /// garbage fits: reference Dec shifted (no true matches left), a 500x too coarse
    /// scale (ball = the whole star table, nearly every candidate rejected), a
    /// zero-CD fit (empty ball, `nr == 0`), a field of exactly `dimquads` stars
    /// (`nt == 0`).
    #[test]
    fn verify_hit_matches_the_materialize_and_sort_reference() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;

        // A fit 500x coarser than the field's: the ball reaches the whole sky, so
        // the survivor fraction is tiny.
        let coarse = QuadHit {
            wcs: Wcs {
                cd: [
                    [winner.wcs.cd[0][0] * 500.0, winner.wcs.cd[0][1] * 500.0],
                    [winner.wcs.cd[1][0] * 500.0, winner.wcs.cd[1][1] * 500.0],
                ],
                ..winner.wcs
            },
            ..winner.clone()
        };
        // A zero CD makes the corner and the center pixel project to the same sky
        // point, so the ball radius is 0 and empty.
        let degenerate = QuadHit {
            wcs: Wcs {
                cd: [[0.0, 0.0], [0.0, 0.0]],
                ..winner.wcs
            },
            ..winner.clone()
        };
        let mut bogus = winner.clone();
        bogus.wcs.crval_deg[1] += 0.5;

        // A field of exactly `dimquads` stars, all far apart, whose every star
        // is one of the quad's slots: the test list comes out empty (`nt == 0`).
        let d = index.meta.dimquads;
        let tiny_field: Vec<Star> = (0..d)
            .map(|i| Star {
                x: 100.0 + 1000.0 * i as f64,
                y: 100.0 + 1000.0 * i as f64,
                flux: 1.0,
            })
            .collect();
        let mut tiny_slots = [0usize; 4];
        for (i, slot) in tiny_slots.iter_mut().enumerate().take(d) {
            *slot = i;
        }
        let tiny = QuadHit {
            field_stars: tiny_slots,
            ..winner.clone()
        };

        for (name, index_stars, hit) in [
            ("winner", &run.stars[..], winner.clone()),
            ("bogus dec+0.5", &run.stars[..], bogus.clone()),
            ("500x scale", &run.stars[..], coarse.clone()),
            ("zero cd", &run.stars[..], degenerate.clone()),
            ("nt == 0", &tiny_field[..], tiny.clone()),
        ] {
            let pre = FieldPre::new(index_stars);
            let new = verify_hit(&index, &hit, index_stars, &pre, run.width, run.height);
            let (old, old_refs) =
                verify_hit_reference(&index, &hit, index_stars, run.width, run.height);
            let new_refs = refxy_new(&index, &hit, run.width, run.height);
            assert_eq!(
                new_refs.len(),
                old_refs.len(),
                "{name}: refxy length differs (nr {} vs {})",
                new.nr,
                old.nr
            );
            for (i, (n, o)) in new_refs.iter().zip(old_refs.iter()).enumerate() {
                assert_eq!(
                    (n[0].to_bits(), n[1].to_bits()),
                    (o[0].to_bits(), o[1].to_bits()),
                    "{name}: refxy[{i}] differs"
                );
            }
            assert_eq!(
                new.bestlogodds.to_bits(),
                old.bestlogodds.to_bits(),
                "{name}: bestlogodds differs ({new:?} vs {old:?})"
            );
            assert_eq!(
                (new.nmatch, new.nr, new.nt, new.bailed),
                (old.nmatch, old.nr, old.nt, old.bailed),
                "{name}: (nmatch, nr, nt, bailed) differs"
            );
        }
        // The legs above must actually exercise the shapes they claim.
        let (old, old_refs) =
            verify_hit_reference(&index, &winner, &run.stars, run.width, run.height);
        assert!(
            old_refs.len() > 10 && old.nmatch > 10,
            "the winner leg is not a real match: nr {} nmatch {}",
            old_refs.len(),
            old.nmatch
        );
        // The coarse leg's ball has to dwarf the winner's and cover a large share
        // of the star table, or the leg does not test a huge ball whose survivors
        // are a handful.
        let ball_at = |wcs: &Wcs| {
            let (ra, dec) = wcs.pix_to_radec(run.width / 2.0, run.height / 2.0);
            let center = radec_to_xyz(ra, dec);
            let (ra, dec) = wcs.pix_to_radec(0.0, 0.0);
            let corner = radec_to_xyz(ra, dec);
            index
                .stars
                .ball_search(&center, f64::sqrt(distsq(&center, &corner)))
                .len()
        };
        let coarse_ball = ball_at(&coarse.wcs);
        assert!(
            coarse_ball >= index.meta.nstars / 10 && coarse_ball >= 100 * old_refs.len(),
            "the 500x-scale leg is not a huge ball: {coarse_ball} of {} stars, {nr} survivors",
            index.meta.nstars,
            nr = old_refs.len()
        );
        assert_eq!(
            verify_hit_reference(&index, &tiny, &tiny_field, run.width, run.height)
                .0
                .nt,
            0,
            "the tiny-field leg must have an empty test list"
        );
        assert_eq!(
            verify_hit_reference(&index, &degenerate, &run.stars, run.width, run.height)
                .0
                .nr,
            0,
            "the zero-cd leg must have an empty reference list"
        );
    }

    /// The ordering rule `collect_refs` relies on, on crafted keys: stable-sorting a
    /// candidate list then filtering equals filtering then stable-sorting the
    /// survivors, for duplicate keys (real star tables hold duplicates), for `-0.0`
    /// and `+0.0` (the inclusive in-frame test admits both) and for a NaN coordinate
    /// (a corrupt index star), which `f64::total_cmp` puts at a fixed order end.
    #[test]
    fn filter_and_sort_commute_on_the_survivor_subsequence() {
        // (ball dist2, x, y); `x.is_nan()` is the reject test, so the NaN-x
        // entries are the ones the in-frame test drops.
        let nan = f64::NAN;
        let cases: Vec<(f64, f64, f64)> = vec![
            (1.0, 10.0, 1.0),
            (0.5, nan, 2.0),
            (2.0, 20.0, 3.0),
            (0.5, nan, 4.0),
            (0.0, -0.0, 5.0),
            (0.0, 0.0, 6.0),
            (2.0, 30.0, 7.0),
            (f64::INFINITY, 40.0, 8.0),
            (0.0, nan, 9.0),
            (2.0, 50.0, 10.0),
            (1.0, 60.0, 11.0),
            (f64::NEG_INFINITY, 70.0, 12.0),
        ];

        // sort-then-filter
        let mut sorted: Vec<(f64, [f64; 2])> = cases.iter().map(|&(d, x, y)| (d, [x, y])).collect();
        sorted.sort_by(|a, b| f64::total_cmp(&a.0, &b.0));
        let after: Vec<[f64; 2]> = sorted
            .into_iter()
            .filter(|&(_, [x, _])| !x.is_nan())
            .map(|(_, p)| p)
            .collect();

        // filter-then-stable-sort
        let mut survivors: Vec<(f64, [f64; 2])> = cases
            .iter()
            .filter(|&&(_, x, _)| !x.is_nan())
            .map(|&(d, x, y)| (d, [x, y]))
            .collect();
        survivors.sort_by(|a, b| f64::total_cmp(&a.0, &b.0));
        let before: Vec<[f64; 2]> = survivors.iter().map(|&(_, p)| p).collect();

        assert_eq!(before.len(), after.len());
        for (i, (a, b)) in before.iter().zip(after.iter()).enumerate() {
            assert_eq!(
                (a[0].to_bits(), a[1].to_bits()),
                (b[0].to_bits(), b[1].to_bits()),
                "entry {i}: filter-then-sort != sort-then-filter"
            );
        }
        // The legs above must actually have exercised the interesting cases:
        // duplicate keys keep their incoming order (the three survivors at
        // dist2 = 2.0 come out as they went in), and the two records at dist2 = 0.0
        // (x = -0.0 and +0.0) share an EQUAL sort key, so their order - negative x
        // first - comes from the sort's STABILITY; both pass the in-frame test.
        let twos: Vec<f64> = survivors
            .iter()
            .filter(|s| s.0 == 2.0)
            .map(|s| s.1[1])
            .collect();
        assert_eq!(twos, vec![3.0, 7.0, 10.0], "duplicate keys lost theirs");
        let zeroes: Vec<f64> = before.iter().map(|p| p[0]).filter(|x| *x == 0.0).collect();
        assert_eq!(zeroes.len(), 2, "both signed zeroes must survive");
        assert!(
            f64::is_sign_negative(zeroes[0]) && f64::is_sign_positive(zeroes[1]),
            "the equal-key -0.0 (earlier incoming) must precede +0.0 under a stable sort, got {zeroes:?}"
        );
    }

    /// The scan with a fresh `used` buffer and the production `x_order` build.
    fn odds_scan(
        testxy: &[[f64; 2]],
        testsigma2: &[f64],
        refxy: &[[f64; 2]],
        eff_area: f64,
        bail_ln: f64,
        stop_ln: f64,
    ) -> (f64, usize, bool) {
        let mut used = Vec::new();
        let mut reford = Vec::new();
        let mut refxs = Vec::new();
        x_order(refxy, &mut reford, &mut refxs);
        odds_scan_inner(
            testxy, testsigma2, refxy, &refxs, &reford, eff_area, bail_ln, stop_ln, &mut used,
        )
    }

    /// Pure accumulator: perfect matches must lift the odds above the solve
    /// threshold, pure noise must bury it, and the bail flag must fire on a long
    /// run of distractors.
    #[test]
    fn odds_scan_synthetic() {
        let refs: Vec<[f64; 2]> = (0..20).map(|i| [10.0 * i as f64, 10.0]).collect();
        // 15 exact matches and nothing else: each pays
        // ln((1-delta)*effA / (2pi*sigma2*nr)) = ln(0.75e6 / (2pi*20)) = 8.69 nats,
        // so 130.4 in total - far above the ln(1e9) = 20.7 solve gate.
        let matched: Vec<[f64; 2]> = refs[..15].to_vec();
        let sig = vec![1.0; matched.len()];
        let (lo, nm, bailed) = odds_scan(&matched, &sig, &refs, 1e6, -300.0, 1e30);
        assert!(!bailed);
        assert_eq!(nm, 15);
        assert!(lo > ODDS_TO_SOLVE_LN, "perfect matches: {lo}");

        // No matches at all: every star is a distractor costing
        // ln(delta) = -1.39 nats, so the best prefix - the first star - stays
        // negative. 50 of them come to -69 nats, nowhere near the -300 floor
        // passed here, so this scan runs to the end.
        let far: Vec<[f64; 2]> = (0..50).map(|i| [10.0 * i as f64, 10000.0]).collect();
        let sig = vec![1.0; far.len()];
        let (lo, nm, _) = odds_scan(&far, &sig, &refs, 1e6, -300.0, 1e30);
        assert_eq!(nm, 0);
        assert!(lo < 0.0, "noise-only odds should stay negative: {lo}");

        // Bail: distractors cost 1.39 nats each, so the -100 floor used here
        // trips on the 73rd star and 600 of them are comfortably past it.
        let many: Vec<[f64; 2]> = (0..600).map(|i| [7.0 * i as f64, 50000.0]).collect();
        let sig = vec![1.0; many.len()];
        let (_, _, bailed) = odds_scan(&many, &sig, &refs, 1e6, -100.0, 1e30);
        assert!(bailed, "bail did not fire");
    }

    /// The round winner on field 1 must verify as solved.
    ///
    /// The odds gate is ours (`ODDS_TO_SOLVE_LN`) so it must hold on any installed
    /// index set, while which quad wins and its exact log-odds belong to that set
    /// and are deliberately not pinned. Kept: the gate, a match count floored by
    /// `testutil::MIN_WINNER_NMATCH` well above what chance pairs reach, and the
    /// field's published scale. Every field-star slot of the winning quad (the
    /// `dimquads` slots the index uses) must sit below the round's object ceiling:
    /// the quad can only have been enumerated from that round's stars.
    #[test]
    fn winner_quad_on_field1_verifies_as_solved() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let h = testutil::winning_hit(run);
        assert!(
            h.verify.bestlogodds >= ODDS_TO_SOLVE_LN,
            "winner log-odds below the solve gate: {:?}",
            h.verify
        );
        assert!(!h.verify.bailed, "the winner bailed its odds scan");
        assert!(
            h.verify.nmatch >= testutil::MIN_WINNER_NMATCH,
            "winner nmatch too low: {:?}",
            h.verify
        );
        assert!(h.verify.nr >= 10, "NR = {}", h.verify.nr);
        assert!(
            f64::abs(h.hit.arcsec_per_pix() - FIELD1_SCALE_ARCSEC) < 0.05,
            "scale {}",
            h.hit.arcsec_per_pix()
        );
        for &slot in &h.hit.field_stars[..run.meta.dimquads] {
            assert!(
                slot < run.max_objs,
                "quad slot {slot} outside the round window 0..{}",
                run.max_objs
            );
        }
    }

    /// The winning hit's WCS with its reference Dec shifted by 0.5 degrees must
    /// NOT solve: log-odds below `ODDS_TO_SOLVE_LN`. The bogus WCS is built at
    /// runtime from the winner, so no quad id is pinned.
    ///
    /// That shift moves every projected index star by roughly 660 px at the field's
    /// arcsec/pixel, far past the 5-sigma match radius: only chance pairs remain.
    #[test]
    fn bogus_quad_does_not_verify_as_solved() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let mut hit = testutil::winning_hit(run).hit;
        hit.wcs.crval_deg[1] += 0.5;
        let pre = super::FieldPre::new(&run.stars);
        let vr = verify_hit(&index, &hit, &run.stars, &pre, run.width, run.height);
        assert!(
            vr.bestlogodds < ODDS_TO_SOLVE_LN,
            "bogus quad must not solve: {vr:?}"
        );
    }

    /// The thread-local `Scratch` must be as good as fresh on every call: two
    /// consecutive `verify_hit` runs of one hit on one thread, with a run of a
    /// different hit between them, must return identical results.
    #[test]
    fn verify_hit_is_repeatable_through_the_scratch() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let pre = FieldPre::new(&run.stars);
        let winner = testutil::winning_hit(run).hit;
        let first = verify_hit(&index, &winner, &run.stars, &pre, run.width, run.height);
        // A second hit dirties the buffers with different nr/nt contents.
        let mut other = winner.clone();
        other.wcs.crval_deg[1] += 0.5;
        let _ = verify_hit(&index, &other, &run.stars, &pre, run.width, run.height);
        let again = verify_hit(&index, &winner, &run.stars, &pre, run.width, run.height);
        assert_eq!(again.bestlogodds.to_bits(), first.bestlogodds.to_bits());
        assert_eq!(
            (again.nmatch, again.nr, again.nt, again.bailed),
            (first.nmatch, first.nr, first.nt, first.bailed)
        );
    }

    /// `top_k` picks the probe's candidate detections, so it must return the
    /// brightest entries in descending order, hand a tie at the cap boundary to
    /// the EARLIER list rank (the probe's answer has to stay a function of the
    /// field list alone, never of how a buffer filled up), cope with a cap wider
    /// than the list, and drop `None` keys.
    #[test]
    fn top_k_picks_the_brightest_and_breaks_ties_by_list_rank() {
        let mk = |fluxes: &[f64]| -> Vec<Star> {
            fluxes
                .iter()
                .map(|&f| Star {
                    x: f,
                    y: 0.0,
                    flux: f,
                })
                .collect()
        };
        let fluxes_of = |idx: &[usize], stars: &[Star]| -> Vec<f64> {
            idx.iter().map(|&i| stars[i].flux).collect()
        };
        let stars = mk(&[10.0, 30.0, 20.0, 40.0, 20.0, 50.0]);

        let mut buf = [0usize; 3];
        assert_eq!(top_k(&stars, |s| Some(s.flux), &mut buf), 3);
        assert_eq!(fluxes_of(&buf, &stars), vec![50.0, 40.0, 30.0]);

        // Three equally bright stars against a cap of two: the two earliest
        // ranks win and the third never displaces them.
        let mut two = [0usize; 2];
        assert_eq!(top_k(&mk(&[5.0, 5.0, 5.0]), |s| Some(s.flux), &mut two), 2);
        assert_eq!(two, [0, 1]);

        // A cap wider than the list returns the whole list, ties still in list
        // order (index 2 before index 4).
        let mut wide = [0usize; 8];
        assert_eq!(top_k(&stars, |s| Some(s.flux), &mut wide), 6);
        assert_eq!(wide[..6], [5, 3, 1, 2, 4, 0]);

        // A `None` key rejects a star outright.
        let mut buf = [0usize; 3];
        assert_eq!(
            top_k(&stars, |s| (s.flux < 40.0).then_some(s.flux), &mut buf),
            3
        );
        assert_eq!(fluxes_of(&buf, &stars), vec![30.0, 20.0, 20.0]);
    }

    /// The probe's two-sided contract: the hit that solves the field must get
    /// through (a false rejection here is a lost field), and a WCS pointed away from
    /// the truth must be dropped before `verify_hit` runs.
    ///
    /// The bogus leg shifts the reference Dec by 5 degrees, not by the 0.5 the
    /// verification-level bogus uses: at 0.5 degrees the probes still land inside a
    /// healpix tile's sky area, so a couple of chance coincidences could lift it over
    /// the acceptance count and make this test flaky. At 5 degrees they land off the
    /// tile (or, for an all-sky index, where the chance rate over 32 probes is
    /// ~1e-4), and a pass is out of the question.
    #[test]
    fn probe_passes_the_winner_and_rejects_a_wrong_pointing() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let cands = probe_stars(&run.stars);
        let winner = testutil::winning_hit(run).hit;
        assert!(
            verify_probe(&index, &winner, &run.stars, &cands, run.width, run.height),
            "the probe rejected the hit that solves the field"
        );
        let mut bogus = winner.clone();
        bogus.wcs.crval_deg[1] += 5.0;
        assert!(
            !verify_probe(&index, &bogus, &run.stars, &cands, run.width, run.height),
            "a WCS 5 deg off the truth must be rejected before verification"
        );
    }

    /// Where the probe cannot price a chance coincidence it must abstain rather
    /// than judge: a frame too small to hold [`PROBE_MIN_CAT_STARS`] catalog
    /// stars, and a detection list too short to reach [`PROBE_MIN_PROBES`] once
    /// the hit's own quad stars are dropped.
    #[test]
    fn probe_abstains_instead_of_judging_what_it_cannot_price() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let cands = probe_stars(&run.stars);
        let winner = testutil::winning_hit(run).hit;
        assert!(
            verify_probe(&index, &winner, &run.stars, &cands, 1.0, 1.0),
            "a 1x1 px frame must not be judged"
        );
        let few: Vec<Star> = run.stars[..4].to_vec();
        let few_cands = probe_stars(&few);
        assert!(
            verify_probe(&index, &winner, &few, &few_cands, run.width, run.height),
            "four detections are below the probe's floor"
        );
    }

    /// `(stars, cands, width, height)` of a square sub-frame of `run`, centered on
    /// the field, whose `cat_in_frame` for `hit` is about `target`: the estimate is
    /// `density * W * H * deg_per_pix^2`, so the side follows from `target` alone,
    /// and the star list is filtered to the square so the probe's footprint ball
    /// stays with it. The fit itself is untouched, which is what makes this usable
    /// for a hit the probe must accept; scaling `cd` would move probes off stars.
    fn sub_frame(
        index: &Index,
        hit: &QuadHit,
        run: &testutil::FieldRun,
        target: f64,
    ) -> (Vec<Star>, ProbeStars, f64, f64) {
        let density = index
            .meta
            .stars_per_deg2()
            .expect("the test index carries a catalog density");
        let deg_per_pix = hit.arcsec_per_pix() / DEG2ARCSEC;
        let side = f64::sqrt(target / (density * deg_per_pix * deg_per_pix));
        let x0 = (run.width - side) / 2.0;
        let y0 = (run.height - side) / 2.0;
        let stars: Vec<Star> = run
            .stars
            .iter()
            .copied()
            .filter(|s| (x0..=(x0 + side)).contains(&s.x) && (y0..=(y0 + side)).contains(&s.y))
            .collect();
        let cands = probe_stars(&stars);
        (stars, cands, side, side)
    }

    /// Fits to probe: the winner plus perturbations - shifts from well below the
    /// probe radius to far outside the tile, a rotated and a rescaled CD, a shifted
    /// reference pixel. The garbage end of this spread is what the probe sorts out in
    /// production, and the population the two lookup paths must never disagree on.
    fn probe_fixture_hits(winner: &QuadHit) -> Vec<QuadHit> {
        let mut hits = vec![winner.clone()];
        for shift in [0.0005, 0.002, 0.02, 0.2, 2.0, 5.0] {
            for sign in [1.0, -1.0] {
                let mut h = winner.clone();
                h.wcs.crval_deg[1] += sign * shift;
                hits.push(h);
            }
        }
        for px in [10.0, 100.0, 1000.0] {
            let mut h = winner.clone();
            h.wcs.crpix[0] += px;
            hits.push(h);
        }
        for factor in [0.5, 2.0, 10.0] {
            let mut h = winner.clone();
            h.wcs.cd = [
                [h.wcs.cd[0][0] * factor, h.wcs.cd[0][1] * factor],
                [h.wcs.cd[1][0] * factor, h.wcs.cd[1][1] * factor],
            ];
            hits.push(h);
        }
        for deg in [0.1, 1.0, 90.0] {
            let (sin, cos) = f64::sin_cos(f64::to_radians(deg));
            let mut h = winner.clone();
            h.wcs.cd = [
                [
                    h.wcs.cd[0][0] * cos - h.wcs.cd[0][1] * sin,
                    h.wcs.cd[0][0] * sin + h.wcs.cd[0][1] * cos,
                ],
                [
                    h.wcs.cd[1][0] * cos - h.wcs.cd[1][1] * sin,
                    h.wcs.cd[1][0] * sin + h.wcs.cd[1][1] * cos,
                ],
            ];
            hits.push(h);
        }
        hits
    }

    /// The real guard on the batched lookup: for the same hit, the ball path and
    /// the per-probe searches must return the same verdict, on real index data.
    /// Every hit is probed over a frame that puts `cat_in_frame` comfortably inside
    /// the batched path's guard, so the comparison cannot be vacuous by abstention,
    /// and both verdicts must show up.
    #[test]
    fn probe_ball_and_searches_agree_for_every_hit() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;
        // A quarter of the guard's ceiling: the footprint ball holds about twice
        // the frame's estimate, and a ball over the ceiling aborts into the
        // fallback instead of answering here.
        let target = PROBE_BALL_MAX_STARS as f64 / 4.0;

        let hits = probe_fixture_hits(&winner);
        let mut compared = 0usize;
        let mut accepted = 0usize;
        let mut rejected = 0usize;
        for hit in &hits {
            let (stars, cands, w, h) = sub_frame(&index, hit, run, target);
            let Some((searches, ball, cat)) = probe_paths(&index, hit, &stars, &cands, w, h) else {
                continue;
            };
            assert!(
                f64::abs(cat - target) <= 0.05 * target,
                "the frame did not land inside the guard: {cat} vs {target}"
            );
            if let Some(ball) = ball {
                assert_eq!(
                    ball,
                    searches,
                    "the probe paths disagree for quad {} at {} arcsec/pixel",
                    hit.quad,
                    hit.arcsec_per_pix()
                );
                compared += 1;
                if searches {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
        }
        assert!(
            compared * 2 >= probe_fixture_hits(&winner).len(),
            "the batched path answered for only {compared} of {} hits: the comparison would prove nothing",
            hits.len()
        );
        assert!(
            accepted >= 1 && rejected >= 1,
            "compared {accepted} accepts against {rejected} rejects: one verdict proves nothing"
        );
    }

    /// The guard decides which path a hit takes, and neither path may change the
    /// answer: the same winner probed over a frame above the ceiling (the batched
    /// path is never started) and inside it (the batched path answers) must both
    /// get through, and so must a pointing wrong by 5 degrees.
    #[test]
    fn probe_guard_chooses_the_path_without_choosing_the_verdict() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;
        let mut bogus = winner.clone();
        bogus.wcs.crval_deg[1] += 5.0;
        let cap = PROBE_BALL_MAX_STARS as f64;

        for (name, hit) in [("winner", &winner), ("5 deg off", &bogus)] {
            let mut batched_frames = 0usize;
            for target in [cap / 4.0, cap * 4.0] {
                let (stars, cands, w, h) = sub_frame(&index, hit, run, target);
                let (searches, ball, cat) =
                    probe_paths(&index, hit, &stars, &cands, w, h).expect(name);
                let verdict = verify_probe(&index, hit, &stars, &cands, w, h);
                assert_eq!(verdict, searches, "{name} at cat_in_frame {cat:.1}");
                if cat > cap {
                    assert_eq!(
                        ball, None,
                        "{name}: a frame above the ceiling must not be batched"
                    );
                } else {
                    batched_frames += 1;
                    assert_eq!(
                        ball,
                        Some(searches),
                        "{name}: inside the ceiling the ball must answer what the searches answer"
                    );
                    assert_eq!(verdict, ball.unwrap());
                }
            }
            assert_eq!(
                batched_frames, 1,
                "{name}: one of the two frames was not batched"
            );
        }
        // The two verdicts must still be distinguishable, or the test above only
        // compared rejections with rejections.
        let (stars, cands, w, h) = sub_frame(&index, &winner, run, cap / 4.0);
        assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
        assert!(!verify_probe(&index, &bogus, &stars, &cands, w, h));
    }

    /// The probe's ball buffer is thread-local scratch, so it must be as good as
    /// fresh on every call:
    /// - the same hit twice on one worker, and two hits in either order, give the
    ///   same verdicts;
    /// - a hit that abstains right after a filled ball leaves the scratch observably
    ///   empty (its path never reaches the collector, so only the entry clear can do
    ///   it);
    /// - repeated batched hits keep it bounded: every collection starts empty, so it
    ///   never grows past one ball.
    #[test]
    fn probe_is_repeatable_through_the_scratch() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;
        let mut bogus = winner.clone();
        bogus.wcs.crval_deg[1] += 5.0;
        // Inside the guard, so the batched path (and its buffer) really runs.
        let (stars, cands, w, h) =
            sub_frame(&index, &winner, run, PROBE_BALL_MAX_STARS as f64 / 4.0);

        assert!(
            verify_probe(&index, &winner, &stars, &cands, w, h),
            "the winner must be accepted"
        );
        assert!(
            !verify_probe(&index, &bogus, &stars, &cands, w, h),
            "a WCS 5 deg off must be rejected"
        );
        // A-then-B-then-A and B-then-A-then-B, on this thread's scratch.
        for _ in 0..2 {
            assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
            assert!(!verify_probe(&index, &bogus, &stars, &cands, w, h));
            assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
            assert!(!verify_probe(&index, &bogus, &stars, &cands, w, h));
            assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
        }
        // A hit the probe abstains on must leave the scratch clean too: the next
        // hit is judged from its own ball. The winner first fills the buffer - the
        // abstaining hit's path never reaches the collector, so an empty scratch
        // afterwards is the entry clear's doing alone.
        assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
        let (few, few_cands, fw, fh) = sub_frame(&index, &winner, run, PROBE_MIN_CAT_STARS / 2.0);
        assert!(verify_probe(&index, &winner, &few, &few_cands, fw, fh));
        SCRATCH.with(|cell| {
            assert!(
                cell.borrow().probe.is_empty(),
                "an abstaining hit left a stale ball in the scratch"
            );
        });
        assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
        assert!(!verify_probe(&index, &bogus, &stars, &cands, w, h));
        // Repeated batched probing through one scratch stays as bounded as a
        // single call: a collection that did not clear first would grow this
        // buffer across the whole run.
        for _ in 0..32 {
            assert!(verify_probe(&index, &winner, &stars, &cands, w, h));
            assert!(!verify_probe(&index, &bogus, &stars, &cands, w, h));
        }
        SCRATCH.with(|cell| {
            let len = cell.borrow().probe.len();
            assert!(
                len <= PROBE_BALL_MAX_STARS,
                "the scratch ball buffer grew past one ball: {len}"
            );
        });
    }

    /// A fit whose projections are not finite must be rejected rather than
    /// silently accepted, and the batched path must decline to answer from a ball
    /// it cannot size: a NaN radius collects nothing, which would read as a
    /// rejection for the wrong reason.
    #[test]
    fn probe_rejects_a_non_finite_wcs_and_agrees_with_the_searches() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;
        // Inside the guard, so the only reason left to decline the ball is the
        // non-finite projection itself.
        let (stars, cands, w, h) =
            sub_frame(&index, &winner, run, PROBE_BALL_MAX_STARS as f64 / 4.0);
        for bad in [
            [f64::NAN, f64::NAN],
            [f64::INFINITY, f64::NAN],
            [f64::NAN, f64::INFINITY],
        ] {
            let mut nan = winner.clone();
            nan.wcs.crval_deg = bad;
            let (searches, ball, _) =
                probe_paths(&index, &nan, &stars, &cands, w, h).expect("the scale is finite");
            assert!(!searches, "{bad:?}: no sky position means no matches");
            assert!(
                ball.is_none(),
                "{bad:?}: a non-finite projection must leave the batched path no ball to size. \
                 A `Some(false)` here would be a NaN radius silently collecting nothing - a \
                 rejection for the wrong reason, not a decline to answer"
            );
            assert!(
                !verify_probe(&index, &nan, &stars, &cands, w, h),
                "{bad:?} must be rejected before verification"
            );
        }

        // One probe whose own projection is garbage, the rest fine: no ball can be
        // sized (its radius would be NaN), so the searches answer - and a probe
        // that cannot match can only cost a match, never invent one.
        let (clean, clean_ball, _) =
            probe_paths(&index, &winner, &stars, &cands, w, h).expect("the scale is finite");
        assert_eq!(clean_ball, Some(clean), "the clean case must be batched");
        let mut broken = stars.clone();
        let quad = &winner.field_stars[..index.meta.dimquads];
        let victim = cands
            .idx
            .iter()
            .copied()
            .find(|i| !quad.contains(i))
            .expect("the probe set holds more than the quad's own stars");
        broken[victim].x = f64::NAN;
        let broken_cands = probe_stars(&broken);
        let (searches, ball, _) =
            probe_paths(&index, &winner, &broken, &broken_cands, w, h).expect("still finite");
        assert_eq!(
            ball, None,
            "a non-finite probe must leave the ball uncomputed"
        );
        assert_eq!(
            verify_probe(&index, &winner, &broken, &broken_cands, w, h),
            searches,
            "verify_probe must take the fallback the batched path declined"
        );
        if !clean {
            assert!(
                !searches,
                "a hit the clean probes rejected cannot be accepted by a list with one probe removed"
            );
        }
    }

    /// The accept threshold is `matches >= need`, not `> need`: a hit whose probes
    /// match exactly `need` catalog stars must be accepted by both paths and by
    /// `verify_probe`, one short of `need` rejected by all three. The winner cannot
    /// show this - it matches far more than `need` and the probe count cannot drop
    /// below `PROBE_MIN_PROBES` - so the fixture sweeps small pointing offsets,
    /// which peel probe stars off their catalog counterparts one by one, and walks
    /// candidate-list prefixes until the count sits exactly on `need` and one below,
    /// in a batched frame and one dense enough to force the per-probe fallback.
    #[test]
    fn probe_accepts_at_exactly_need_matches() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;
        let cap = PROBE_BALL_MAX_STARS as f64;
        let mut at_need = 0usize;
        let mut below_need = 0usize;
        let mut trace: Vec<(f64, f64, usize, usize, usize, usize)> = Vec::new();
        for shift in [0.0, 0.002, 0.004, 0.006, 0.008, 0.010, 0.012, 0.015] {
            for sign in [1.0, -1.0] {
                let mut hit = winner.clone();
                hit.wcs.crval_deg[1] += sign * shift;
                for target in [cap / 4.0, cap * 4.0] {
                    let (stars, cands, w, h) = sub_frame(&index, &hit, run, target);
                    for keep in 1..=cands.idx.len() {
                        let cut = ProbeStars {
                            idx: cands.idx[..keep].to_vec(),
                        };
                        let Some(plan) = probe_setup(&index, &hit, &stars, &cut, w, h) else {
                            continue;
                        };
                        let matches = plan.qs[..plan.probes]
                            .iter()
                            .filter(|q| {
                                all_finite(q)
                                    && index.stars.nearest_within(*q, plan.cutoff2).is_some()
                            })
                            .count();
                        if matches <= 6 {
                            trace.push((shift, sign, keep, plan.probes, plan.need, matches));
                        }
                        if matches != plan.need && matches + 1 != plan.need {
                            continue;
                        }
                        let mut buf = Vec::new();
                        let batched = probe_batched(&index.stars, &plan, &mut buf);
                        let searches = probe_via_searches(&index.stars, &plan);
                        let verdict = verify_probe(&index, &hit, &stars, &cut, w, h);
                        if matches == plan.need {
                            at_need += 1;
                            assert!(
                                searches,
                                "exactly `need` matches must be accepted by the searches"
                            );
                            assert!(verdict, "exactly `need` matches must be accepted by verify");
                            if plan.cat_in_frame <= cap {
                                assert_eq!(
                                    batched,
                                    Some(true),
                                    "exactly `need` matches must be accepted by the ball"
                                );
                            }
                        } else {
                            below_need += 1;
                            assert!(
                                !searches,
                                "one short of `need` must be rejected by the searches"
                            );
                            assert!(!verdict, "one short of `need` must be rejected by verify");
                            if plan.cat_in_frame <= cap {
                                assert_eq!(
                                    batched,
                                    Some(false),
                                    "one short of `need` must be rejected by the ball"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert!(
            at_need >= 1 && below_need >= 1,
            "the fixture sweep never sat exactly on the threshold: \
             at_need {at_need}, below_need {below_need}, near-threshold trace {trace:?}"
        );
    }

    /// The collector owns the buffer's state: `probe_ball_coords` empties `buf` on
    /// entry, so a buffer that arrives holding a stale witness still answers from
    /// this ball only - one hit's collection can never match another hit's probes.
    #[test]
    fn probe_ball_coords_clears_its_buffer() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let winner = testutil::winning_hit(run).hit;
        let (stars, cands, w, h) =
            sub_frame(&index, &winner, run, PROBE_BALL_MAX_STARS as f64 / 4.0);
        let plan = probe_setup(&index, &winner, &stars, &cands, w, h).expect("plannable");
        let mut fresh = Vec::new();
        assert!(probe_ball_coords(
            &index.stars,
            &plan.center,
            plan.radius,
            usize::MAX,
            &mut fresh
        ));
        // A star of the winner's ball that really answers one of its probes: a
        // stale copy of it surviving into the next collection would match.
        let mut stale = None;
        'probes: for q in &plan.qs[..plan.probes] {
            for s in &fresh {
                if within_chord2(q, s, plan.cutoff2) {
                    stale = Some(*s);
                    break 'probes;
                }
            }
        }
        let stale = stale.expect("an accepted winner has a probe with a star under it");

        // A ball far from every star: nothing collected, nothing matched.
        let far_plan = ProbePlan {
            cat_in_frame: plan.cat_in_frame,
            qs: plan.qs,
            probes: plan.probes,
            cutoff2: plan.cutoff2,
            need: 1,
            center: [1e3, 1e3, 1e3],
            radius: 1e-9,
        };
        let mut clean = Vec::new();
        assert_eq!(
            probe_via_ball(&index.stars, &far_plan, &mut clean),
            Some(false)
        );
        assert!(clean.is_empty(), "the far ball holds no star");
        // The same collection into a buffer that already holds the witness: the
        // entry clear must erase it, or the stale star answers `Some(true)` for a
        // ball that holds nothing.
        let mut stale_buf = vec![stale];
        assert_eq!(
            probe_via_ball(&index.stars, &far_plan, &mut stale_buf),
            Some(false),
            "a stale witness must not answer for a ball that holds nothing"
        );
    }
}
