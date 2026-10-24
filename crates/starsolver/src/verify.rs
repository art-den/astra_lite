//! Verification and odds: take a fitted code hit, project the index stars that
//! land in the frame against it, and accumulate the log-odds that the match is
//! real over the field stars.
//!
//! The odds model is the minimal two-term one: every field star contributes
//! `max(match term, distractor term)` against a uniform background over the
//! whole frame (`effA = W*H`), and nothing else. There is no conflict
//! resolution when two field stars want one reference star: the first to claim
//! it keeps it. The test list is scanned as delivered - in the order
//! [`crate::augment::preprocess`] produced, with no re-shuffling here - and
//! `bestlogodds` is the maximum over the prefixes of that scan.
//!
//! First-come matching is a deliberate project decision that biases the odds
//! slightly upward: when a second field star lands near an already-taken
//! reference star, a matcher that resolves that conflict can deny the match,
//! while here it banks a fresh full match term. The frozen ln(1e9) solve gate
//! ([`crate::params::ODDS_TO_SOLVE_LN`]) is calibrated against this slightly
//! looser matcher, so the two must not be tuned independently.
//!
//! Flow:
//! 1. `sigma2 = VERIFY_PIX^2 + (INDEX_JITTER_ARCSEC / arcsec_per_pix)^2`, px^2:
//!    the field-side centroid uncertainty added in quadrature with the index
//!    position uncertainty converted from arcsec to pixels by the fitted scale.
//!    Every per-star variance below is a multiple of this base value.
//! 2. Index stars are collected by a 3D chord-ball search around the sky
//!    position of the frame-center pixel `(W/2, H/2)`, radius = the chord to
//!    the `(0, 0)` corner. The ball only narrows the candidate set (see the
//!    note in [`verify_hit`]), so each star it returns is projected into pixels
//!    and kept only when `0 <= x <= W` and `0 <= y <= H`, inclusive. Those
//!    bounds are 0-based, which makes the upper edge one pixel looser than the
//!    1-based FITS convention the reference solver's in-frame test uses. Since
//!    `nr` divides both odds terms, moving this edge shifts the log-odds a run
//!    reports, so it is frozen.
//! 3. The quad's own index stars never enter the reference list (and so are not
//!    in `nr`): they are the stars the WCS was just fitted from, so a match
//!    against one would be evidence reused as evidence. The quad's field stars
//!    leave the test list too, but only after deduplication.
//! 4. Each field star takes the nearest still-unused reference star within
//!    `NN_RADIUS_SIGMA * sigma_i`, where `sigma2_i = sigma2 * (1 + R_i^2 / Q^2)`
//!    with `R_i` the star's distance to the midpoint of the quad backbone `AB`
//!    and `Q = |AB|/2`. The quad's own stars are what the fit was pinned to, so
//!    a star far from them is projected less accurately and is matched against a
//!    wider Gaussian.
//! 5. The accumulator ([`odds_scan`]) adds `term - logbg` per star, with
//!    `term = max(logfg, logd)`, abandons the hit below `BAIL_THRESHOLD_LN` and
//!    stops early above `stop_ln` — which [`verify_hit`] sets to infinity, so a
//!    production scan always visits the whole list.

use std::f64::consts::PI;

use crate::coords::{distsq, radec_to_xyz, xyz_to_radec};
use crate::index::Index;
use crate::params::{
    BAIL_THRESHOLD_LN, DISTRACTOR_RATIO, INDEX_JITTER_ARCSEC, NN_RADIUS_SIGMA, ODDS_TO_SOLVE_LN,
    VERIFY_PIX,
};
use crate::solver::QuadHit;
use crate::xyls::Star;

/// Outcome of verifying one code hit.
#[derive(Debug, Clone, Copy)]
pub struct VerifyResult {
    /// The highest the running log-odds reached over the prefixes of the scan.
    /// The terms are not all positive and the prefix that trips the bail is
    /// never offered, so this is the best prefix, not the last total.
    pub bestlogodds: f64,
    /// Matches banked up to that best prefix.
    pub nmatch: usize,
    /// Reference stars kept in the frame, quad stars removed. `nr` divides both
    /// odds terms, so it sets the price of a match and of a miss.
    pub nr: usize,
    /// Test stars that reach the scan: after deduplication and after the quad's
    /// field stars are dropped.
    pub nt: usize,
    /// Set when the running total dropped below `BAIL_THRESHOLD_LN` and the
    /// rest of the list was abandoned.
    pub bailed: bool,
}

impl VerifyResult {
    #[must_use]
    pub fn solved(&self) -> bool {
        self.bestlogodds >= ODDS_TO_SOLVE_LN
    }
}

/// Odds accumulator over the test stars, visited in the order they are handed
/// in (the preprocessed field order, which is not flux-sorted), against the
/// projected reference stars. First come wins: once a reference star is matched
/// it is no longer offered to later test stars.
///
/// Each star contributes `ln(P(observed | solved) / P(observed | unsolved))`,
/// both sides as densities per square pixel: the unsolved side is the uniform
/// background `1 / eff_area` (the distractor term), the solved side a Gaussian
/// of variance `testsigma2[i]` at the nearest still-unused reference star (the
/// match term). The larger of the two is banked, so a star whose counterpart is
/// missing or far away costs odds instead of paying for a match. That makes the
/// traversal order part of the result: `bestlogodds` is a maximum over prefixes
/// of the scan.
///
/// Returns `(bestlogodds, nmatch_at_best, bailed)`.
#[must_use]
pub fn odds_scan(
    testxy: &[[f64; 2]],
    testsigma2: &[f64],
    refxy: &[[f64; 2]],
    eff_area: f64,
    bail_ln: f64,
    stop_ln: f64,
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

    let mut used = vec![false; refxy.len()];
    let mut logodds = 0.0f64;
    let mut mu = 0usize;
    let mut bestlogodds = f64::NEG_INFINITY;
    let mut best_mu = 0usize;
    let mut bailed = false;

    let nnsig2 = NN_RADIUS_SIGMA * NN_RADIUS_SIGMA;
    for (i, tp) in testxy.iter().enumerate() {
        let s2 = testsigma2[i];
        // Distractor density. A fraction `delta` of detections is junk
        // outright; the rest are real stars, and a share `mu / nr` of the
        // reference stars is already consumed, so a real star has that chance of
        // finding no free counterpart and adding only noise. The price of a miss
        // is therefore ln(delta + (1 - delta) * mu / nr): -1.39 nats at the
        // start, rising toward 0 as the scan uses up the reference stars, so it
        // is the first misses that drive a bail. `mu` never exceeds `nr`
        // (matches are one-to-one), so the `u32` step into `f64::from` is
        // lossless.
        let logd = f64::ln(delta + (1.0 - delta) * f64::from(mu as u32) / nr) + logbg;

        // The nearest reference star that is still free and lies within
        // NN_RADIUS_SIGMA (5) sigmas of this star: `bd2` starts as the squared
        // search radius and only shrinks, so the loop returns the nearest one
        // rather than any one. Brute force is fine at these counts: the list is
        // the index stars inside the frame, not the catalog behind it, and a
        // solved hit keeps tens of them.
        let mut bd2 = s2 * nnsig2;
        let mut bi = usize::MAX;
        for (j, rp) in refxy.iter().enumerate() {
            if used[j] {
                continue;
            }
            let dx = tp[0] - rp[0];
            let dy = tp[1] - rp[1];
            let d2 = dx * dx + dy * dy;
            if d2 <= bd2 {
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
            // evaluated at the offset actually found (`bd2` is 0 for an exact
            // overlap, where the term is largest).
            let loggmax = f64::ln((1.0 - delta) / (2.0 * PI * s2 * nr));
            let logfg = loggmax - bd2 / (2.0 * s2);
            if logfg > logd {
                used[bi] = true;
                mu += 1;
                logfg
            } else {
                // Too weak to beat the distractor term: the star contributes
                // noise and the reference star stays free for later stars.
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

/// Verifies a fitted code hit against the index star list.
#[must_use]
pub fn verify_hit(
    index: &Index,
    hit: &QuadHit,
    stars: &[Star],
    width: f64,
    height: f64,
) -> VerifyResult {
    let arcsec = hit.arcsec_per_pix();
    // px^2, the base of every per-star variance below.
    let sigma2 = VERIFY_PIX * VERIFY_PIX + f64::powi(INDEX_JITTER_ARCSEC / arcsec, 2);

    // Candidate cap for the index stars: a ball in 3D chord space around the
    // sky position of the frame-center pixel (W/2, H/2), radius = the chord
    // from there to the (0, 0) corner. It is a pre-filter, not a strict bound:
    // the fitted WCS puts its tangent point at the matched quad's pixel
    // centroid (see fit_tan_wcs), so chord distance from the center pixel is
    // not monotone in pixel distance and the (0, 0) corner is not generally the
    // frame's farthest point on the sky. With the tangent point displaced
    // toward one corner that corner pokes out of the ball - on field 1's
    // reference WCS (tangent point ~650 px off the frame center) it is 4.8
    // arcsec, about 1.8 px, beyond it. Stars inside the frame but outside the
    // ball are lost at the search; sky the ball reaches outside the frame is cut
    // by the pixel test below.
    let center = {
        let (ra, dec) = hit.wcs.pix_to_radec(width / 2.0, height / 2.0);
        radec_to_xyz(ra, dec)
    };
    let corner = {
        let (ra, dec) = hit.wcs.pix_to_radec(0.0, 0.0);
        radec_to_xyz(ra, dec)
    };
    let radius = f64::sqrt(distsq(&center, &corner));

    // Reference stars: the ball hits that project inside the frame, minus the
    // quad's own index stars. The comparison is direct because a quad row holds
    // star-table row numbers, which is what the star tree reports as a hit
    // index. The in-frame test is inclusive at both ends in 0-based pixels;
    // see the module header for why that upper edge is frozen.
    let quad_ids = index.quad(hit.quad);
    let dimquads = index.meta.dimquads;
    let mut refxy: Vec<[f64; 2]> = Vec::new();
    for h in index.stars.ball_search(&center, radius) {
        if quad_ids[..dimquads].contains(&(h.index as u32)) {
            continue;
        }
        let (ra, dec) = xyz_to_radec(index.star_xyz(h.index));
        let (x, y) = hit.wcs.radec_to_pix(ra, dec);
        if (0.0..=width).contains(&x) && (0.0..=height).contains(&y) {
            refxy.push([x, y]);
        }
    }
    let nr = refxy.len();

    // Per-test-star variance: sigma2 widened by how far the star sits from the
    // quad, sigma2_i = sigma2 * (1 + R_i^2 / Q2). Q2 is the squared half of the
    // backbone AB (the midpoint-to-A distance), so a star one half-backbone from
    // the midpoint already carries twice the variance, and the Gaussian that
    // pays its match is correspondingly lower and wider.
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

    // Deduplication: a star that lands within one sigma of an earlier kept star
    // is dropped, at that earlier star's own sigma2 as the radius. "Earlier" is
    // the incoming list order, so a drop only ever removes a later entry and the
    // kept set depends on that order.
    //
    // Neighbours are looked up in a copy of the indices sorted by x, so the two
    // partition_point calls hand back the x-window [xi-r, xi+r] and only the
    // stars in it get the real 2D test. The radii are a pixel or two near the
    // quad (sigma2 grows away from it, see above), so the window stays short.
    let nstars = stars.len();
    let mut keep = vec![true; nstars];
    if nstars > 1 {
        let mut byx: Vec<u32> = (0..nstars as u32).collect();
        byx.sort_by(|&a, &b| f64::total_cmp(&stars[a as usize].x, &stars[b as usize].x));
        let xs: Vec<f64> = byx.iter().map(|&i| stars[i as usize].x).collect();
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
                // kept when its turn comes: the outer loop skips the rest, so a
                // dropped star never removes anything in turn.
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
    // dedup runs they are still in the list, so they can still suppress a
    // nearby detection, and that suppression is part of the odds a run
    // reproduces. Slicing to dimquads matters - for a 3-star index the unused
    // 4th slot holds the 0 sentinel, which would silently drop star 0.
    for &slot in &hit.field_stars[..index.meta.dimquads] {
        keep[slot] = false;
    }

    let mut testxy = Vec::with_capacity(stars.len());
    let mut testsigma2 = Vec::with_capacity(stars.len());
    for (i, s) in stars.iter().enumerate() {
        if keep[i] {
            testxy.push([s.x, s.y]);
            testsigma2.push(sigma2s[i]);
        }
    }
    let nt = testxy.len();

    // No early stop here: stop_ln is +infinity, so the scan runs the whole test
    // list and the solve decision is made from the odds it returns. That is the
    // caller's job - the sweep's round capture takes hits whose log-odds reach
    // ODDS_TO_SOLVE_LN (see VerifyResult::solved) and keeps the lexicographic
    // best of the round; it deliberately never interrupts a round, so stdout
    // does not depend on the order in which parallel workers reach their hits.
    //
    // Traversal order is part of the odds definition: bestlogodds is a maximum
    // over the prefixes of the incoming list order - the preprocessed .xyls
    // order from augment::preprocess - and nothing re-sorts it here, so any
    // change to that order changes the log-odds a hit reports.
    let (bestlogodds, nmatch, bailed) = odds_scan(
        &testxy,
        &testsigma2,
        &refxy,
        width * height,
        BAIL_THRESHOLD_LN,
        f64::INFINITY,
    );
    VerifyResult {
        bestlogodds,
        nmatch,
        nr,
        nt,
        bailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use crate::wcs::test_data::FIELD1_SCALE_ARCSEC;

    /// Pure accumulator: perfect matches must lift the odds above the solve
    /// threshold, pure noise must bury it, and the bail flag must fire on a
    /// long run of distractors.
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

        // No matches at all: every star is a distractor, so each costs
        // ln(delta) = -1.39 nats and the best prefix - the first star - stays
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
    /// The odds gate is ours (`ODDS_TO_SOLVE_LN`), so it must hold on any
    /// installed index set, while which quad wins and what its exact log-odds
    /// are is a property of that set and is deliberately not pinned. Kept as
    /// invariants: the gate, a match count floored by
    /// `testutil::MIN_WINNER_NMATCH` well above what chance pairs can reach
    /// (the count itself is a property of the index set, so it is not pinned),
    /// and the field's published scale.
    ///
    /// Every field-star slot of the winning quad (the `dimquads` slots the index
    /// actually uses) must sit below the round's object ceiling: the quad can
    /// only have been enumerated from stars that round feeds the search.
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
    /// Shifting the reference Dec by 0.5 degrees moves every projected index
    /// star by roughly 660 px at the field's arcsec/pixel - far past the
    /// 5-sigma match radius - so the pairs the true WCS banked are gone and only
    /// chance pairs remain, which cannot lift the scan to the gate.
    #[test]
    fn bogus_quad_does_not_verify_as_solved() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let mut hit = testutil::winning_hit(run).hit;
        hit.wcs.crval_deg[1] += 0.5;
        let vr = verify_hit(&index, &hit, &run.stars, run.width, run.height);
        assert!(
            vr.bestlogodds < ODDS_TO_SOLVE_LN,
            "bogus quad must not solve: {vr:?}"
        );
    }
}
