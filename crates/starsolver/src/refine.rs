//! Post-verification refine of a solved hit: a "tweak-lite" pass that re-fits
//! the raw quad WCS on a wider, tightly matched star set.
//!
//! The raw solve result is a similarity fit of one quad - 3 or 4 stars, the
//! index's `dimquads` - so the printed rotation/scale/center carry
//! the anchor noise of that tiny support set. Here a clean pair set is rebuilt
//! with a tight fixed radius, the same 4-parameter similarity (two
//! translations, rotation or reflection, uniform scale; no distortion terms)
//! is re-fitted on it by `fit_tan_wcs` over the whole set instead of one quad,
//! match + refit iterate until the pair set stops changing (bounded by
//! `MAX_REFINE_ITERS`), and the refined WCS replaces the raw one only once the
//! guard below has judged it against the raw fit on that common pair set.
//!
//! The verification matches are deliberately NOT reused as the seed: the odds
//! scan matches within `NN_RADIUS_SIGMA` (5) sigmas of a per-star sigma that
//! widens with distance from the quad backbone - a star one half-backbone from
//! its midpoint already carries twice the variance - while `fit_tan_wcs` has
//! no outlier rejection. The tight fixed radius, not the generous odds-scan
//! one, is what keeps the refit clean. Verification itself stays untouched:
//! this module post-processes an already-verified solution only, and a refine
//! failure never turns solved into unsolved (the caller keeps the raw WCS on
//! `None`).

use std::collections::HashSet;

use crate::coords::{distsq, radec_to_xyz, xyz_to_radec};
use crate::index::Index;
use crate::params::{MAX_REFINE_ITERS, REFINE_MATCH_PIX, REFINE_MIN_PAIRS};
use crate::solver::QuadHit;
use crate::wcs::{Wcs, fit_tan_wcs};
use crate::xyls::Star;

/// Matched pairs: index star record id -> field star list index.
pub type Pairs = Vec<(u32, usize)>;

/// Matches index stars to field stars under `wcs` with a tight fixed radius.
///
/// Every ordering below is part of the spec: under first-come claiming the
/// pair set - and so the fixed point `refine_wcs` iterates toward - is only
/// reproducible if the candidate set, the visiting order and the tie-breaks
/// are fixed.
/// * Candidate cap as in verification: the index stars within the chord from
///   the sky position of the frame-center pixel (`wcs.pix_to_radec(w / 2,
///   h / 2)`) to that of the `(0, 0)` corner, projected into pixels and kept
///   inside `[0, w] x [0, h]`, inclusive, all of it under the WCS passed in.
/// * Index stars visited in ascending record id: a plain scan of the star
///   table, not the distance-ordered kd-tree ball search verification uses.
/// * The quad's own index stars are NOT excluded, unlike in the odds scan
///   (there a quad star is a star the fit was made from, so matching it would
///   reuse evidence): here the anchors have to survive re-matching.
/// * First-come claiming: each index star takes the nearest field star not yet
///   claimed within `radius_px`, and a field star is claimed at most once, so
///   a later index star settles for the next-nearest free one. An incumbent is
///   replaced only by a strictly nearer candidate, which sends an
///   exact-distance tie to the smaller field index.
#[must_use]
pub fn match_pairs(
    index: &Index,
    wcs: &Wcs,
    stars: &[Star],
    width: f64,
    height: f64,
    radius_px: f64,
) -> Pairs {
    let (ra, dec) = wcs.pix_to_radec(width / 2.0, height / 2.0);
    let center = radec_to_xyz(ra, dec);
    let (ra, dec) = wcs.pix_to_radec(0.0, 0.0);
    let radius2 = distsq(&center, &radec_to_xyz(ra, dec));
    let r2max = radius_px * radius_px;

    let mut claimed = vec![false; stars.len()];
    let mut pairs = Pairs::new();
    for sid in 0..index.meta.nstars {
        let xyz = index.star_xyz(sid);
        if distsq(&xyz, &center) > radius2 {
            continue;
        }
        let (ra, dec) = xyz_to_radec(xyz);
        let (x, y) = wcs.radec_to_pix(ra, dec);
        if !(0.0..=width).contains(&x) || !(0.0..=height).contains(&y) {
            continue;
        }
        // Nearest unclaimed field star within radius; ascending j with a
        // strict `<` after the first candidate keeps the smaller field index
        // on exact-distance ties.
        let mut bj: Option<usize> = None;
        let mut bd2 = 0.0;
        for (j, s) in stars.iter().enumerate() {
            if claimed[j] {
                continue;
            }
            let dx = x - s.x;
            let dy = y - s.y;
            let d2 = dx * dx + dy * dy;
            let take = if bj.is_none() { d2 <= r2max } else { d2 < bd2 };
            if take {
                bd2 = d2;
                bj = Some(j);
            }
        }
        if let Some(j) = bj {
            claimed[j] = true;
            pairs.push((sid as u32, j));
        }
    }
    pairs
}

/// The quad's own (index star, field star) anchor pairs of a hit: quad slot
/// `i` paired with `hit.field_stars[i]`, cut at `dimquads` so the unused
/// sentinel slot of a 3-star quad is never read.
#[must_use]
fn anchor_pairs(index: &Index, hit: &QuadHit) -> Pairs {
    let d = index.meta.dimquads;
    index.quad(hit.quad)[..d]
        .iter()
        .copied()
        .zip(hit.field_stars[..d].iter().copied())
        .collect()
}

/// Merges matched pairs with the quad anchors, de-duplicated: the anchors come
/// first and win on both sides, so a match that would re-pair an anchor's
/// index star or its field star is the one dropped (a re-match must never drop
/// or re-pair an anchor). `match_pairs` output is already unique on both
/// sides, so the surviving matches need no further de-duplication; sorting them
/// by `(sid, field index)` keeps the Vec order canonical, which is what the
/// pair-set equality test in `refine_wcs` compares.
#[must_use]
fn merge_pairs(matched: &Pairs, anchors: &Pairs) -> Pairs {
    let used_s: HashSet<u32> = anchors.iter().map(|(s, _)| *s).collect();
    let used_f: HashSet<usize> = anchors.iter().map(|(_, f)| *f).collect();
    let mut out = anchors.clone();
    let mut rest: Pairs = matched
        .iter()
        .copied()
        .filter(|(s, f)| !used_s.contains(s) && !used_f.contains(f))
        .collect();
    rest.sort_unstable();
    out.extend(rest);
    out
}

/// Projection residuals (px) of every pair's index star against its field
/// star under `wcs`.
fn residuals(index: &Index, stars: &[Star], wcs: &Wcs, pairs: &Pairs) -> Vec<f64> {
    pairs
        .iter()
        .map(|&(sid, fid)| {
            let (ra, dec) = xyz_to_radec(index.star_xyz(sid as usize));
            let (x, y) = wcs.radec_to_pix(ra, dec);
            f64::hypot(x - stars[fid].x, y - stars[fid].y)
        })
        .collect()
}

#[must_use]
fn mean_residual(index: &Index, stars: &[Star], wcs: &Wcs, pairs: &Pairs) -> f64 {
    let r = residuals(index, stars, wcs, pairs);
    r.iter().sum::<f64>() / r.len() as f64
}

#[must_use]
fn rms_residual(index: &Index, stars: &[Star], wcs: &Wcs, pairs: &Pairs) -> f64 {
    let r = residuals(index, stars, wcs, pairs);
    f64::sqrt(r.iter().map(|v| v * v).sum::<f64>() / r.len() as f64)
}

/// Similarity refit of a pair set: `fit_tan_wcs` with N = pairs.len().
/// Degenerate sets return `None` through that fit's own guards: fewer than 3
/// pairs, a zero-variance pixel set (`fvar = 0`, all pairs on one point) or a
/// zero-variance star set (`pvar = 0`), and NaN in the inputs, which every
/// earlier comparison fails open on, so the finite-CD check at the end of that
/// fit is what rejects it. Collinear-but-spread pixels are NOT rejected: there
/// the two orientation-branch magnitudes are algebraically equal (in floating
/// point they tie only to within rounding), so which branch - and with it the
/// parity - the bogus fit lands on is decided by rounding; rejecting it is left
/// to the parity and residual checks in `accept_refined`.
fn fit_pairs(index: &Index, stars: &[Star], pairs: &Pairs) -> Option<Wcs> {
    let sxyz: Vec<[f64; 3]> = pairs
        .iter()
        .map(|&(sid, _)| index.star_xyz(sid as usize))
        .collect();
    let fxy: Vec<[f64; 2]> = pairs
        .iter()
        .map(|&(_, fid)| [stars[fid].x, stars[fid].y])
        .collect();
    fit_tan_wcs(&sxyz, &fxy)
}

/// The accept guard, judged with BOTH WCSes over the SAME final pair set: the
/// refined WCS replaces the raw one only when the set has at least
/// `REFINE_MIN_PAIRS` pairs, the parity is unchanged (parity is decided by
/// which orientation branch `fit_tan_wcs` takes, and a small support set can
/// flip it), and the refined fit is no worse by mean residual. NaN-safe: a
/// non-finite residual on either side rejects instead of slipping through the
/// `<=`. `Err` carries the reject reason for the `-v` line.
fn accept_refined(
    index: &Index,
    stars: &[Star],
    raw: &Wcs,
    refined: &Wcs,
    pairs: &Pairs,
) -> Result<(), &'static str> {
    if pairs.len() < REFINE_MIN_PAIRS {
        return Err("too few pairs");
    }
    if refined.parity_is_neg() != raw.parity_is_neg() {
        return Err("parity flip");
    }
    let rr = mean_residual(index, stars, refined, pairs);
    let rm = mean_residual(index, stars, raw, pairs);
    // Explicit finiteness: a NaN residual (poisoned WCS) must reject, and a
    // plain `!(rr <= rm)` would trip clippy's neg_cmp_op_on_partial_ord.
    if !(rr.is_finite() && rm.is_finite() && rr <= rm) {
        return Err("refined fits worse (mean residual)");
    }
    Ok(())
}

/// Refines the raw WCS of a hit that has passed the solve gate (the caller
/// reaches this only for a hit whose `VerifyResult::solved()` held): seed the
/// pair set with `match_pairs` under the raw WCS at `REFINE_MATCH_PIX` plus
/// the quad anchors, then at most `MAX_REFINE_ITERS` times: `fit_pairs` ->
/// `match_pairs` under that candidate -> re-merge the anchors (a re-match can
/// otherwise drop them, and losing the anchors moves the fixed point the loop
/// is chasing), stopping as soon as the pair set stops changing. A set that
/// keeps changing instead runs to the cap, and the guard then judges the last
/// candidate against the last set. `None` means the guard rejected and the
/// caller keeps the raw WCS: refine replaces the raw fit only with one that is
/// no worse, so it never unsolves a field. `verbose` prints the `[refine]`
/// summary line to stderr (stdout is the product).
#[must_use]
pub fn refine_wcs(
    index: &Index,
    hit: &QuadHit,
    stars: &[Star],
    width: f64,
    height: f64,
    verbose: bool,
) -> Option<Wcs> {
    let raw = hit.wcs;
    let anchors = anchor_pairs(index, hit);
    let mut pairs = merge_pairs(
        &match_pairs(index, &raw, stars, width, height, REFINE_MATCH_PIX),
        &anchors,
    );
    let mut refined = raw;
    for _ in 0..MAX_REFINE_ITERS {
        // Degenerate fit: stop with the previous candidate (the raw fit on
        // the first pass) and let the guard judge it.
        let Some(candidate) = fit_pairs(index, stars, &pairs) else {
            break;
        };
        let next = merge_pairs(
            &match_pairs(index, &candidate, stars, width, height, REFINE_MATCH_PIX),
            &anchors,
        );
        refined = candidate;
        let stable = next == pairs;
        pairs = next;
        if stable {
            break;
        }
    }
    match accept_refined(index, stars, &raw, &refined, &pairs) {
        Ok(()) => {
            if verbose {
                let rms = rms_residual(index, stars, &refined, &pairs);
                eprintln!(
                    "[refine] {} pairs, rms {rms:.2} px, rot {:.3} deg",
                    pairs.len(),
                    refined.orientation_deg()
                );
            }
            Some(refined)
        }
        Err(reason) => {
            if verbose {
                eprintln!("[refine] kept raw: {reason} ({} pairs)", pairs.len());
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use crate::wcs::pixel_center_of_size;

    /// The `4656x3522-2` fixture frame. Its published sky truth is
    /// `testutil::FIELD2_TRUTH` (the reference run behind it is recorded in
    /// `tests/ground_truth.rs`), so the convergence test below anchors on the
    /// sky rather than on any one index file's fit numbers, and holds whichever
    /// installed index solves the field.
    const FIELD2: (&str, f64, f64) = ("4656x3522-2.xyls", 4656.0, 3522.0);

    /// Agreement budgets of the convergence test's leg (a): how far the reports
    /// of different solved hits may drift apart. They are 10x tighter on
    /// rotation and 3x tighter on center than the sky-truth budgets of leg (b)
    /// (`testutil::TOL_ORIENT_DEG`, `testutil::TOL_CENTER_PX`).
    const CONVERGE_ROT_DEG: f64 = 0.05;
    const CONVERGE_CENTER_PX: f64 = 0.5;

    fn star(x: f64, y: f64) -> Star {
        Star { x, y, flux: 1.0 }
    }

    /// The field-center pixel of a `w x h` frame.
    fn center_pix_of(w: f64, h: f64) -> [f64; 2] {
        [pixel_center_of_size(w), pixel_center_of_size(h)]
    }

    /// Refines a hit and re-centres it on the field center: the report path of
    /// the production sweep (`sweep::RoundCapture::collect` - refine, fall back
    /// to the raw WCS on `None`, then `Wcs::recentered_at` at the field-center
    /// pixel).
    fn refined_report(index: &Index, hit: &QuadHit, stars: &[Star], w: f64, h: f64) -> Wcs {
        refine_wcs(index, hit, stars, w, h, false)
            .unwrap_or(hit.wcs)
            .recentered_at(center_pix_of(w, h))
    }

    /// The reports to compare: the solved hits of the round that won the field,
    /// extended by `testutil::solved_hits_with_peers(run, 2)` with the solved
    /// hits of the following depth rounds whenever the winning round produced
    /// fewer than two. Those peers are fits over different field-star subsets,
    /// which is exactly the material this comparison needs.
    fn field2_reports(run: &testutil::FieldRun) -> Vec<(usize, Wcs)> {
        let index = testutil::load(run.meta);
        testutil::solved_hits_with_peers(run, 2)
            .into_iter()
            .map(|h| {
                (
                    h.hit.quad,
                    refined_report(&index, &h.hit, &run.stars, run.width, run.height),
                )
            })
            .collect()
    }

    /// `fit_pairs` inherits `fit_tan_wcs`'s guards: fewer than 3 pairs and
    /// NaN-poisoned pixels return `None`; so do zero-variance pixel sets
    /// (all pairs on one point: `fvar = 0`). Collinear-but-spread sets are
    /// NOT rejected by the fit (there the rotation and reflection branch
    /// magnitudes are algebraically equal, so rounding alone decides which one
    /// the fit's `>=` takes); a bogus fit like that is what the accept guard
    /// (parity plus residual comparison against the raw WCS) exists to reject.
    #[test]
    fn fit_pairs_guards_synthetic_pair_sets() {
        // The star ids below run up to 3 * 997 + 3 = 2994, so this fixture
        // needs a star table with at least 4096 stars: `smallest_index` hands
        // out the smallest file that qualifies, or `None` - the `return` below
        // - when nothing does.
        let Some(meta) = testutil::smallest_index(4096) else {
            return;
        };
        let index = testutil::load(meta);
        // Real index stars against synthetic pixels; the assertion below is
        // what shows this star side clears the fit's degeneracy guards.
        let sids: Pairs = (0..4u32).map(|s| (s * 997 + 3, 0usize)).collect();
        let good = [
            star(100.0, 100.0),
            star(900.0, 140.0),
            star(260.0, 700.0),
            star(1200.0, 650.0),
        ];
        let mut pairs: Pairs = sids.iter().enumerate().map(|(i, &(s, _))| (s, i)).collect();
        assert!(
            fit_pairs(&index, &good, &pairs).is_some(),
            "4 well-spread pairs must fit"
        );
        pairs.truncate(2);
        assert!(
            fit_pairs(&index, &good, &pairs).is_none(),
            "< 3 pairs must be rejected"
        );

        // Zero variance: all pixels identical (`fvar = 0`).
        let same = [star(42.0, 42.0); 4];
        let pairs: Pairs = sids.iter().enumerate().map(|(i, &(s, _))| (s, i)).collect();
        assert!(
            fit_pairs(&index, &same, &pairs).is_none(),
            "zero-variance pixels must be rejected"
        );

        // NaN-poisoned pixel.
        let mut poisoned = good;
        poisoned[1] = star(f64::NAN, 140.0);
        let pairs: Pairs = sids.iter().enumerate().map(|(i, &(s, _))| (s, i)).collect();
        assert!(
            fit_pairs(&index, &poisoned, &pairs).is_none(),
            "NaN must be rejected"
        );
    }

    /// The accept-guard rejects: too few pairs, a parity flip, a refined WCS
    /// that fits worse on the common pair set, and a NaN-poisoned refined WCS
    /// (a non-finite residual must reject, not slip through `<=`).
    ///
    /// The fixture is the winning hit of `4656x3522-2` on whichever installed
    /// index solves the field (`testutil::field_run`): case (a) starves the pair
    /// set, the rest deform one part of the refined WCS and must be rejected by
    /// the check each of them names.
    #[test]
    fn guard_rejects_small_flipped_worse_and_nan() {
        let Some(run) = testutil::field_run(FIELD2.0, FIELD2.1, FIELD2.2, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let hit = testutil::winning_hit(run).hit;
        let stars = &run.stars;
        let raw = hit.wcs;
        let anchors = anchor_pairs(&index, &hit);

        // (a) fewer than REFINE_MIN_PAIRS pairs.
        let few = anchors.clone();
        assert_eq!(
            accept_refined(&index, stars, &raw, &raw, &few),
            Err("too few pairs"),
            "{} anchor pairs must not pass the {REFINE_MIN_PAIRS}-pair floor",
            few.len()
        );

        // A real refined WCS for this hit (`unwrap_or(raw)` keeps the case
        // well-formed even where the guard rejects), plus the pair set
        // rematched under it: that set stays fixed from here on, so below only
        // the WCS gets deformed.
        let refined = refine_wcs(&index, &hit, stars, run.width, run.height, false).unwrap_or(raw);
        let pairs = merge_pairs(
            &match_pairs(
                &index,
                &refined,
                stars,
                run.width,
                run.height,
                REFINE_MATCH_PIX,
            ),
            &anchors,
        );
        assert!(pairs.len() >= REFINE_MIN_PAIRS);
        assert!(accept_refined(&index, stars, &raw, &refined, &pairs).is_ok());

        // (b) parity flip: negating one CD column flips the sign of det(CD)
        // while |det(CD)| - the scale - stays as it was.
        let mut flipped = refined;
        flipped.cd[0][1] = -refined.cd[0][1];
        flipped.cd[1][1] = -refined.cd[1][1];
        assert_ne!(flipped.parity_is_neg(), raw.parity_is_neg());
        assert_eq!(
            accept_refined(&index, stars, &raw, &flipped, &pairs),
            Err("parity flip")
        );

        // (c) refined fits worse: inflate the scale 2%.
        let mut worse = refined;
        for row in &mut worse.cd {
            for v in row {
                *v *= 1.02;
            }
        }
        assert_eq!(
            accept_refined(&index, stars, &raw, &worse, &pairs),
            Err("refined fits worse (mean residual)")
        );

        // NaN-poisoned refined WCS: a NaN in `crval` keeps the parity check
        // clean (the CD is untouched) and poisons only the residuals, where
        // the explicit finiteness test in `accept_refined` must reject it.
        let mut nanw = refined;
        nanw.crval_deg[0] = f64::NAN;
        assert_eq!(nanw.parity_is_neg(), raw.parity_is_neg());
        assert!(
            accept_refined(&index, stars, &raw, &nanw, &pairs).is_err(),
            "NaN refined residual must reject"
        );
    }

    /// `match_pairs` under the raw fit of a real solved hit of field 2: index
    /// star ids strictly ascending (the deterministic claiming order), each
    /// field star claimed at most once, and every quad anchor pair still in the
    /// set after `merge_pairs` - the merge drops the match that would re-pair
    /// an anchor instead of displacing the anchor.
    #[test]
    fn match_pairs_fixture_anchor_survival_and_uniqueness() {
        let Some(run) = testutil::field_run(FIELD2.0, FIELD2.1, FIELD2.2, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let hit = testutil::winning_hit(run).hit;
        let stars = &run.stars;
        let pairs = match_pairs(
            &index,
            &hit.wcs,
            stars,
            run.width,
            run.height,
            REFINE_MATCH_PIX,
        );
        assert!(
            pairs.len() >= REFINE_MIN_PAIRS,
            "seed must find real matches: {}",
            pairs.len()
        );
        let mut sids: Vec<u32> = pairs.iter().map(|(s, _)| *s).collect();
        assert!(
            {
                let sorted = sids.clone();
                sids.sort_unstable();
                sids.dedup();
                sorted == sids
            },
            "index star ids must be strictly ascending (deterministic iteration)"
        );
        let mut fids: Vec<usize> = pairs.iter().map(|(_, f)| *f).collect();
        fids.sort_unstable();
        let n = fids.len();
        fids.dedup();
        assert_eq!(fids.len(), n, "a field star may be claimed at most once");

        let anchors = anchor_pairs(&index, &hit);
        let merged = merge_pairs(&pairs, &anchors);
        for &(s, f) in &anchors {
            assert!(
                merged.contains(&(s, f)),
                "anchor ({s},{f}) did not survive the merge"
            );
        }
    }

    /// Invariant: every solved hit of the round that wins `4656x3522-2`, put
    /// through the report path (`refine_wcs(..).unwrap_or(raw)`, then
    /// `recentered_at(center)`), lands on ONE fixed point - within
    /// `CONVERGE_ROT_DEG` of rotation and `CONVERGE_CENTER_PX` of center of the
    /// others - and that point sits on the field's published sky truth within
    /// the `testutil` gate budgets. The second leg is what turns the agreement
    /// into proof of being right rather than mere self-consistency.
    ///
    /// No index or quad id is named here: `testutil::field_run` discovers at
    /// runtime which index and round win the field on the machine's index set,
    /// and the budgets are the gate's own, so the same assertion holds for
    /// whichever index solves it.
    #[test]
    fn refined_solved_hits_converge_to_one_sky_point() {
        let Some(run) = testutil::field_run(FIELD2.0, FIELD2.1, FIELD2.2, None) else {
            return;
        };
        let reports = field2_reports(run);

        // (a) one fixed point: every report agrees with the first.
        let (first_quad, first) = &reports[0];
        for (quad, w) in &reports[1..] {
            let drot = testutil::wrap_deg(w.orientation_deg() - first.orientation_deg());
            let dcenter = {
                let dra = testutil::wrap_deg(w.crval_deg[0] - first.crval_deg[0])
                    * f64::cos(f64::to_radians(w.crval_deg[1]));
                f64::hypot(dra, w.crval_deg[1] - first.crval_deg[1]) * 3600.0 / w.arcsec_per_pix()
            };
            assert!(
                f64::abs(drot) < CONVERGE_ROT_DEG,
                "quads {first_quad}/{quad} rotation delta {drot} deg"
            );
            assert!(
                dcenter < CONVERGE_CENTER_PX,
                "quads {first_quad}/{quad} center delta {dcenter} px"
            );
        }

        // (b) sky-truth anchor on the converged point (parity included).
        let truth = testutil::FIELD2_TRUTH;
        let w = &reports[0].1;
        assert!(
            f64::abs(testutil::wrap_deg(w.orientation_deg() - truth.orient_deg))
                < testutil::TOL_ORIENT_DEG,
            "rot {}",
            w.orientation_deg()
        );
        assert!(
            f64::abs(w.arcsec_per_pix() / truth.scale_arcsec - 1.0) < testutil::TOL_SCALE_FRAC,
            "scale {}",
            w.arcsec_per_pix()
        );
        assert!(
            truth.center_off_px(w) < testutil::TOL_CENTER_PX,
            "center off by {} px: {:?}",
            truth.center_off_px(w),
            w.crval_deg
        );
        assert!(w.parity_is_neg(), "parity");
    }

    /// Budget of the generated-field refine gate: what is left once the truth
    /// carries no fit noise is the refine's own drift, so the center budget is
    /// 4x tighter than `genfield::TOL_CENTER_PX`. Rotation and scale reuse the
    /// generator's budgets: a refine that moves them is a bug, not noise.
    const GEN_REFINE_CENTER_PX: f64 = 0.05;
    /// Frame of the generated field this gate solves.
    const GEN_FRAME: (f64, f64) = (1920.0, 1080.0);

    /// Gate: on a generated field - a real catalog pattern with an exact truth -
    /// every solved hit of the winning round, refined and re-centred, lands on
    /// that truth. This is the "refine must not wander" pin: the raw fit comes
    /// from one quad, the refine re-fits on a tight pair set, and the two must
    /// agree with the sky to a fraction of a pixel, not merely with each other.
    #[test]
    fn refine_of_a_generated_field_lands_on_the_exact_truth() {
        let Some((run, gen_truth)) = testutil::generated_field_run(23, GEN_FRAME.0, GEN_FRAME.1)
        else {
            return;
        };
        let truth = testutil::exact_truth(&gen_truth);
        let index = testutil::load(run.meta);
        // `min_want` 1: an empty hit list must fail the gate, not pass it vacuously.
        for h in &testutil::solved_hits_with_peers(&run, 1) {
            let quad = h.hit.quad;
            let report = refined_report(&index, &h.hit, &run.stars, run.width, run.height);
            let off = truth.center_off_px(&report);
            assert!(
                off <= GEN_REFINE_CENTER_PX,
                "quad {quad}: refined report off the exact truth by {off} px"
            );
            let drot = testutil::wrap_deg(report.orientation_deg() - truth.orient_deg);
            assert!(
                f64::abs(drot) <= crate::genfield::TOL_ORIENT_DEG,
                "quad {quad}: rotation off by {drot} deg"
            );
            assert!(
                f64::abs(report.arcsec_per_pix() / truth.scale_arcsec - 1.0)
                    <= crate::genfield::TOL_SIZE_FRAC,
                "quad {quad}: scale {} vs truth {}",
                report.arcsec_per_pix(),
                truth.scale_arcsec
            );
            assert_eq!(report.parity_is_neg(), truth.parity_neg, "quad {quad}");
        }
    }
}
