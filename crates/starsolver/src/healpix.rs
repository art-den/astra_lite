//! Healpix cell geometry for the index-tile position gate: does a sub-region
//! index's sky cell overlap the `--ra/--dec` hint circle? The gate is a
//! performance filter only (`sweep.rs::tile_in_hint`); it must never prune a
//! tile that could contain the field. All-sky indices (`HEALPIX=-1`) have no
//! cell to test and never reach this module.
//!
//! Cell numbers are read in the astrometry.net *xy* numbering `index.rs` takes
//! from the `HEALPIX`/`HPNSIDE` keys: `hp = (base * nside + x) * nside + y` -
//! the 12 base pixels in order, and inside each one nside rows (`x`) of nside
//! columns (`y`). A cell center is that (x, y) -> direction map evaluated at
//! the middle of the cell's slot, the fractional pixel (0.5, 0.5).
//!
//! FITS ring numbering instead runs along rings of constant latitude, so the
//! same integer names another cell: at nside=2 ring 9 is the cell xy 10 names
//! (RA 247.5 deg, dec +41.8 deg), 33 deg from the xy 9 cell (RA 202.5 deg)
//! whose center is 5.4 deg from the `1920x1080-1` field center.
//!
//! The xy numbering fixes the base-pixel orientation: pixels 0..3 are the north
//! polar caps (lon 45/135/225/315 deg, dec +asin(2/3) = +41.81 deg), 4..7 the
//! equatorial belt (lon 0/90/180/270 deg, dec 0), 8..11 the south caps (same
//! longitudes, dec -41.81 deg).

use crate::coords::{chord2_to_rad, distsq, radec_to_xyz};
use std::f64::consts::PI;

/// xyz of the center of xy-scheme healpix cell `hp` at subdivision `nside`, or
/// `None` when the pair names no cell: `nside <= 0`, `hp` outside
/// `[0, 12 * nside^2)`, or `12 * nside^2` overflows.
///
/// Needs no power-of-two `nside`; the `index.rs` load gate refuses a
/// non-power-of-two `HPNSIDE` before any cell is looked up, so this stays
/// general.
#[must_use]
pub fn healpix_center_xyz(hp: i64, nside: i64) -> Option<[f64; 3]> {
    if nside <= 0 {
        return None;
    }
    let npix = i64::checked_mul(12, i64::checked_mul(nside, nside)?)?;
    if hp < 0 || hp >= npix {
        return None;
    }

    let ns2 = nside * nside;
    let chp = hp / ns2; // base healpix, 0..11
    let sub = hp % ns2;
    let xp = sub / nside;
    let yp = sub % nside;

    let mut x = xp as f64 + 0.5;
    let mut y = yp as f64 + 0.5;
    let ns = nside as f64;

    let northpolar = chp <= 3;
    let southpolar = chp >= 8;
    let mut zfactor = 1.0;
    // A polar base pixel's square overhangs the cap: once x + y passes nside,
    // the equatorial branch would put the point past the z = +-2/3 cap edge
    // (up to 4/3 at the far corner, off the sphere), so those points take the
    // polar branch instead.
    let equatorial = !(northpolar && x + y > ns) && !(southpolar && x + y < ns);
    if southpolar && !equatorial {
        zfactor = -1.0;
    }

    let z: f64;
    let mut phi: f64;
    if equatorial {
        // Equatorial band (|z| <= 2/3): z is linear in x + y, so every cell
        // spans the same z range and area, and the longitude is linear in
        // x - y. zoff maps a polar overhang onto a belt panel (0 = its north
        // half, -2 = its south half, -1 = the whole belt of base pixels 4..7),
        // phioff turns it 45 deg into its cap's quadrant, panel picks that
        // quadrant.
        let (zoff, phioff, panel) = if northpolar {
            (0.0, 1.0, chp)
        } else if southpolar {
            (-2.0, 1.0, chp - 8)
        } else {
            (-1.0, 0.0, chp - 4)
        };
        let xf = x / ns;
        let yf = y / ns;
        z = 2.0 / 3.0 * (xf + yf + zoff);
        phi = PI / 4.0 * (xf - yf + phioff + 2.0 * panel as f64);
    } else {
        // Polar-cap branch: inside a polar base pixel the cells form square
        // rings around the pole. phi_t is the position within the base pixel's
        // 90 deg quadrant (0 at the y = nside edge, pi/2 at x = nside); sq the
        // fraction of the cap's area enclosed by the ring through the point (0
        // at the pole, 1 on the cap edge), so z = +-(1 - sq/3) runs from +-1
        // down to the +-2/3 edge; phi_t < pi/4 selects the side of that ring
        // being crossed. The south cap is worked out as the north one (x and y
        // swapped and reflected in nside) with zfactor flipping z.
        // (x, y) = (nside, nside) is the pole, where phi_t would be 0/0; a
        // cell center never gets there, since x and y are at most nside - 0.5.
        if zfactor == -1.0 {
            std::mem::swap(&mut x, &mut y);
            x = ns - x;
            y = ns - y;
        }
        let phi_t = if y == ns && x == ns {
            0.0
        } else {
            PI * (ns - y) / (2.0 * ((ns - x) + (ns - y)))
        };
        let sq = if phi_t < PI / 4.0 {
            let k = PI * (ns - x) / ((2.0 * phi_t - PI) * ns);
            k * k
        } else {
            let k = PI * (ns - y) / (2.0 * phi_t * ns);
            k * k
        };
        z = (1.0 - sq / 3.0) * zfactor;
        phi = if southpolar {
            PI / 2.0 * (chp as f64 - 8.0) + phi_t
        } else {
            PI / 2.0 * chp as f64 + phi_t
        };
    }

    if phi < 0.0 {
        phi += 2.0 * PI;
    }
    let rad = f64::sqrt(1.0 - z * z);
    Some([rad * f64::cos(phi), rad * f64::sin(phi), z])
}

/// Safe upper bound on the angular circumradius of a healpix cell at
/// subdivision `nside`, radians.
///
/// A cell covers `4 pi / (12 N^2)` sr, equal-area cap radius
/// `sqrt(area / pi) = 1 / (N sqrt 3)` rad. The true worst-case circumradius is
/// the center-to-corner distance (a corner is a cell's farthest point, checked
/// against a dense scan of its interior): N times it grows monotonically
/// toward ~61 deg (48.2 at N=1, 60.9 at N=32, measured over every cell). Below
/// we double the equal-area radius and add a further 1.5x safety factor
/// (`2 * 1.5 / sqrt(3) = sqrt(3)`), giving ~99.2 deg / N, about 1.6x above that
/// worst case: over-inclusion only costs extra tile trials, never a missed
/// solve.
///
/// Panics in debug builds if `nside <= 0`; the only caller,
/// [`tile_overlaps_circle`], reaches it only once a center has been computed,
/// which means `nside > 0`.
#[must_use]
pub fn cell_margin_rad(nside: i64) -> f64 {
    debug_assert!(nside > 0, "margin is only defined for a real subdivision");
    f64::sqrt(3.0) / nside as f64
}

/// Conservative test: might healpix cell `hp` at `nside` come within
/// `radius_deg` (the `--radius` hint circle) of `(ra_deg, dec_deg)`? Accepts
/// iff the great-circle distance from the hint center to the cell center is at
/// most `radius_deg + cell_margin_rad(nside)`, the margin being a safe upper
/// bound on the cell circumradius: a circle that reaches the cell at all must
/// come that close to its center. A cell whose center cannot be computed (a
/// corrupt tile header that still passed the load gate) is kept: the gate must
/// never over-prune.
#[must_use]
pub fn tile_overlaps_circle(
    hp: i64,
    nside: i64,
    ra_deg: f64,
    dec_deg: f64,
    radius_deg: f64,
) -> bool {
    let Some(center) = healpix_center_xyz(hp, nside) else {
        return true;
    };
    let dist_rad = chord2_to_rad(distsq(&center, &radec_to_xyz(ra_deg, dec_deg)));
    dist_rad <= f64::to_radians(radius_deg) + cell_margin_rad(nside)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::xyz_to_radec;

    fn norm(v: [f64; 3]) -> f64 {
        f64::sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2])
    }

    fn angsep_deg(a: [f64; 3], b: [f64; 3]) -> f64 {
        crate::coords::rad2deg(chord2_to_rad(distsq(&a, &b)))
    }

    fn radec(ra_deg: f64, dec_deg: f64) -> [f64; 3] {
        radec_to_xyz(ra_deg, dec_deg)
    }

    #[test]
    fn centers_are_unit_vectors() {
        for nside in [1i64, 2, 4, 64] {
            for hp in 0..12 * nside * nside {
                let v = healpix_center_xyz(hp, nside).expect("cell must exist");
                assert!(
                    (norm(v) - 1.0).abs() < 1e-12,
                    "nside {nside} hp {hp}: |v|={}",
                    norm(v)
                );
            }
        }
    }

    #[test]
    fn out_of_range_cells_are_none() {
        assert!(healpix_center_xyz(0, 0).is_none());
        assert!(healpix_center_xyz(0, -4).is_none());
        assert!(healpix_center_xyz(-1, 2).is_none());
        assert!(healpix_center_xyz(12 * 4, 2).is_none());
        assert!(healpix_center_xyz(48, 2).is_none());
        assert!(healpix_center_xyz(11, 1).is_some());
        assert!(healpix_center_xyz(12, 1).is_none());
    }

    /// Pins the base-pixel orientation stated in the module docs.
    #[test]
    fn nside_one_base_pixel_centers() {
        let lat = 41.81031; // asin(2/3) in degrees
        for hp in 0..12i64 {
            let (ra, dec) = xyz_to_radec(healpix_center_xyz(hp, 1).unwrap());
            let (want_ra, want_dec) = match hp {
                0..=3 => (45.0 + 90.0 * hp as f64, lat),
                4..=7 => (90.0 * (hp - 4) as f64, 0.0),
                _ => (45.0 + 90.0 * (hp - 8) as f64, -lat),
            };
            assert!((ra - want_ra).abs() < 1e-4, "hp {hp}: ra {ra} != {want_ra}");
            assert!(
                (dec - want_dec).abs() < 1e-4,
                "hp {hp}: dec {dec} != {want_dec}"
            );
        }
    }

    /// Ordering sanity: cell 0 sits in the north half, the last cell in the
    /// south, and refinement nests in the xy layout - the children of
    /// (base, x, y) at nside 2N are (base, 2x + dx, 2y + dy) - so every child
    /// center stays within its nside N parent's circumradius bound.
    #[test]
    fn cell_order_and_refinement_stay_within_the_margin() {
        let first = healpix_center_xyz(0, 8).unwrap();
        let last = healpix_center_xyz(12 * 64 - 1, 8).unwrap();
        assert!(
            first[2] > 0.0,
            "cell 0 must be in the north: z={}",
            first[2]
        );
        assert!(
            last[2] < 0.0,
            "last cell must be in the south: z={}",
            last[2]
        );

        // Bound: measured worst-case xy-cell circumradius, 61 deg / N
        // (`cell_margin_rad`).
        for nside in [1i64, 2, 4, 8] {
            let bound = 61.0 / nside as f64;
            for hp in 0..12 * nside * nside {
                let parent = healpix_center_xyz(hp, nside).unwrap();
                let b = hp / (nside * nside);
                let x = (hp % (nside * nside)) / nside;
                let y = hp % nside;
                let n2 = 2 * nside;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let child = ((b * n2 + (2 * x + dx)) * n2) + (2 * y + dy);
                    let d = angsep_deg(parent, healpix_center_xyz(child, n2).unwrap());
                    assert!(
                        d < bound,
                        "nside {nside} hp {hp} child ({dx},{dy}): {d} deg"
                    );
                }
            }
        }
    }

    /// Base-pixel centers are far apart: at nside=1 the belt centers are 90 deg
    /// apart, a belt/cap pair ~58 deg.
    #[test]
    fn adjacent_base_pixel_centers_are_distant() {
        let b4 = healpix_center_xyz(4, 1).unwrap();
        let b5 = healpix_center_xyz(5, 1).unwrap();
        let b3 = healpix_center_xyz(3, 1).unwrap();
        assert!((angsep_deg(b4, b5) - 90.0).abs() < 1e-6);
        let d = angsep_deg(b3, b4);
        // Belt/cap base-center spacing: acos(cos(asin(2/3)) * cos(45 deg))
        // = 58.19 deg (center-to-center, not a cell circumradius).
        assert!((57.0..60.0).contains(&d), "belt/cap spacing {d} deg");
    }

    /// Orientation pins against the two narrow-field centers (asserted below):
    /// a radius-0 hint at either center keeps the nside=1 tile 2, whose margin
    /// alone is ~99 deg, and at nside=2 the nearest centers are xy pixels 9 and
    /// 10. Read as a FITS ring number, 9 would name the cell at RA 247.5 deg,
    /// dec +41.8 deg - 32 deg from field 1.
    #[test]
    fn winning_tile_contains_the_narrow_fields() {
        let f1 = radec(202.484305, 47.232352);
        let f2 = radec(250.423162, 36.479129);
        let c2 = healpix_center_xyz(2, 1).unwrap();
        assert!((angsep_deg(c2, f1) - 16.87).abs() < 0.01);
        assert!((angsep_deg(c2, f2) - 20.34).abs() < 0.01);
        assert!(tile_overlaps_circle(2, 1, 202.484305, 47.232352, 0.0));
        assert!(tile_overlaps_circle(2, 1, 250.423162, 36.479129, 0.0));
        // One subdivision finer: nearest centers are xy pixels 9 (field 1) and
        // 10 (field 2).
        assert!(angsep_deg(healpix_center_xyz(9, 2).unwrap(), f1) < 6.0);
        assert!(angsep_deg(healpix_center_xyz(10, 2).unwrap(), f2) < 6.0);
    }

    /// Never over-prune: a radius-0 hint on a cell center keeps that cell.
    #[test]
    fn a_hint_at_a_cell_center_always_keeps_that_cell() {
        for nside in [1i64, 2, 4] {
            for hp in 0..12 * nside * nside {
                let (ra, dec) = xyz_to_radec(healpix_center_xyz(hp, nside).unwrap());
                assert!(
                    tile_overlaps_circle(hp, nside, ra, dec, 0.0),
                    "nside {nside} hp {hp} pruned its own center"
                );
            }
        }
    }

    /// The gate does prune: a hint at field 2's center with the default 10 deg
    /// radius keeps only a fraction of the sky (the nside=1 margin ~99 deg
    /// prunes little by design), and a far-away cell is rejected at every tested
    /// nside.
    #[test]
    fn far_cells_are_pruned_near_cells_are_kept() {
        let (ra, dec) = (250.423162, 36.479129);
        let keep = |nside: i64, radius: f64| -> usize {
            (0..12 * nside * nside)
                .filter(|&hp| tile_overlaps_circle(hp, nside, ra, dec, radius))
                .count()
        };
        assert_eq!(keep(1, 10.0), 9);
        assert_eq!(keep(2, 10.0), 12);
        assert_eq!(keep(4, 10.0), 16);
        // Far cells (centers 82-160 deg from the hint).
        assert!(!tile_overlaps_circle(8, 1, ra, dec, 10.0));
        assert!(!tile_overlaps_circle(47, 2, ra, dec, 10.0));
        assert!(!tile_overlaps_circle(150, 4, ra, dec, 10.0));
        assert!(keep(2, 20.0) > keep(2, 10.0));
        assert!(keep(4, 20.0) > keep(4, 10.0));
        // Cell nearest the hint's antipode (0.1 deg away at nside=64).
        assert!(!tile_overlaps_circle(36177, 64, ra, dec, 10.0));
        // Cell nearest the hint (nside=2 pixel 10, 5.8 deg).
        assert!(tile_overlaps_circle(10, 2, ra, dec, 0.0));
    }

    /// A tile with uncomputable geometry (nside <= 0, hp out of range) is kept.
    #[test]
    fn uncomputable_geometry_keeps_the_tile() {
        assert!(tile_overlaps_circle(0, 0, 0.0, 0.0, 0.0));
        assert!(tile_overlaps_circle(99, 1, 0.0, 0.0, 0.0));
    }

    /// The margin pin: inverse proportion to nside, always above the measured
    /// worst-case circumradius bound of 61 deg / nside.
    #[test]
    fn cell_margin_bound() {
        for nside in [1i64, 2, 4, 64] {
            let m = f64::to_degrees(cell_margin_rad(nside));
            let want = 99.239201 / nside as f64; // sqrt(3) rad / nside
            assert!((m - want).abs() < 1e-4, "nside {nside}: {m}");
            assert!(m > 61.0 / nside as f64);
        }
    }

    /// No power-of-two requirement in the geometry; the `index.rs` load gate is
    /// what refuses a non-power-of-two `HPNSIDE`.
    #[test]
    fn non_power_of_two_nside_still_computes() {
        let v = healpix_center_xyz(20, 6).expect("nside 6 hp 20 exists");
        let (ra, dec) = xyz_to_radec(v);
        assert!((ra - 52.5).abs() < 1e-6, "ra {ra}");
        assert!((dec - 41.81031).abs() < 1e-4, "dec {dec}");
    }
}
