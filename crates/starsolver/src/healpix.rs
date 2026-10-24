//! Healpix cell geometry for the index-tile position gate: does a sub-region
//! index's sky cell overlap the `--ra/--dec` hint circle? The gate is a
//! performance filter only (`sweep.rs::tile_in_hint`); it must never prune a
//! tile that could contain the field. All-sky indices (`HEALPIX=-1`) have no
//! cell to test and never reach this module.
//!
//! Cell numbers are read in the astrometry.net *xy* numbering, the one the
//! index loader (`index.rs`) takes from the `HEALPIX`/`HPNSIDE` header keys:
//! `hp = (base * nside + x) * nside + y` - the 12 base pixels in order, and
//! inside each one the nside rows (`x`) of nside columns (`y`). The FITS ring
//! numbering instead runs along rings of constant latitude, so the same
//! integer names another cell: at nside=2 ring number 9 is the cell that xy
//! number 10 names, centered at RA 247.5 deg, dec +41.8 deg, 33 deg away
//! from the xy pixel 9 cell (RA 202.5 deg) whose center is 5.4 deg from the
//! `1920x1080-1` field center.
//!
//! That numbering fixes the base-pixel orientation (pinned by
//! `nside_one_base_pixel_centers`): pixels 0..3 are the north polar caps,
//! centered at lon 45/135/225/315 deg and dec +asin(2/3) = +41.81 deg, pixels
//! 4..7 the equatorial belt at lon 0/90/180/270 deg and dec 0, pixels 8..11
//! the south caps at the same longitudes and dec -41.81 deg.
//!
//! A cell center is that (x, y) -> direction map evaluated at the middle of
//! the cell's slot, the fractional pixel (0.5, 0.5).

use crate::coords::{chord2_to_rad, distsq, radec_to_xyz};
use std::f64::consts::PI;

/// xyz of the center of xy-scheme healpix cell `hp` at subdivision `nside`,
/// or `None` when the pair does not name a cell: `nside <= 0`, `hp` outside
/// `[0, 12 * nside^2)`, or an `nside` so large that `12 * nside^2` overflows.
///
/// The math needs no power-of-two `nside`; the tile load gate in `index.rs`
/// refuses a non-power-of-two `HPNSIDE` before any cell is looked up, so this
/// stays general (see `non_power_of_two_nside_still_computes`).
#[must_use]
pub fn healpix_center_xyz(hp: i64, nside: i64) -> Option<[f64; 3]> {
    if nside <= 0 {
        return None;
    }
    let npix = i64::checked_mul(12, i64::checked_mul(nside, nside)?)?;
    if hp < 0 || hp >= npix {
        return None;
    }

    // The xy numbering: base pixel 0..11 first, then the nside rows (x) of
    // nside columns (y) inside it.
    let ns2 = nside * nside;
    let chp = hp / ns2; // base healpix, 0..11
    let sub = hp % ns2;
    let xp = sub / nside;
    let yp = sub % nside;

    // The cell center is the middle of its slot: fractional pixel (0.5, 0.5).
    let mut x = xp as f64 + 0.5;
    let mut y = yp as f64 + 0.5;
    let ns = nside as f64;

    let northpolar = chp <= 3;
    let southpolar = chp >= 8;
    let mut zfactor = 1.0;
    // A polar base pixel's square overhangs the cap: once x + y passes nside,
    // the equatorial branch below would put the point past the z = +-2/3 cap
    // edge (up to 4/3 at the far corner, which is off the sphere), so such
    // points take the polar branch instead.
    let equatorial = !(northpolar && x + y > ns) && !(southpolar && x + y < ns);
    if southpolar && !equatorial {
        zfactor = -1.0;
    }

    let z: f64;
    let mut phi: f64;
    if equatorial {
        // Equatorial band (|z| <= 2/3): z is linear in x + y, so every cell
        // spans the same z range and therefore the same area, and the
        // longitude is linear in x - y. zoff maps a polar overhang onto a
        // belt panel (0 = its north half, -2 = its south half, -1 = the whole
        // belt of base pixels 4..7); phioff turns a polar overhang by 45 deg
        // so that it lands in its cap's quadrant, and panel picks the
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
        // rings around the pole. phi_t is the position within the base
        // pixel's 90 deg quadrant (0 at the y = nside edge, pi/2 at the
        // x = nside edge) and sq the fraction of the cap's area enclosed by
        // the square ring through the point: 0 at the pole, 1 on the cap
        // edge, so z = +-(1 - sq/3) runs from +-1 down to the +-2/3 edge.
        // phi_t < pi/4 selects the side of the square ring being crossed.
        // The south cap is worked out as the north one (x and y swapped and
        // reflected in nside) with zfactor flipping z.
        // The (x, y) = (nside, nside) test is the pole itself, where phi_t
        // would be 0/0; a cell center never gets there, since x and y are at
        // most nside - 0.5.
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
/// A cell covers `4 pi / (12 N^2)` sr; the equivalent equal-area cap radius
/// is `sqrt(area / pi) = 1 / (N sqrt 3)` rad. The true worst-case xy-cell
/// circumradius is a center-to-corner distance (a corner is the farthest
/// point of a cell, checked against a dense scan of its interior), and N
/// times it grows monotonically toward ~61 deg: 48.2 deg at N=1, 60.9 deg at
/// N=32, measured over every cell. The margin below doubles the equal-area
/// radius and adds a further 1.5x safety factor
/// (`2 * 1.5 / sqrt(3) = sqrt(3)`), giving ~99.2 deg / N, about 1.6x above
/// that worst case: over-inclusion only costs extra tile trials, never a
/// missed solve.
///
/// Panics in debug builds if `nside <= 0`; the only caller is
/// [`tile_overlaps_circle`], which reaches it only once a center has been
/// computed, and a computable center means `nside > 0`.
#[must_use]
pub fn cell_margin_rad(nside: i64) -> f64 {
    debug_assert!(nside > 0, "margin is only defined for a real subdivision");
    f64::sqrt(3.0) / nside as f64
}

/// Conservative test: might healpix cell `hp` at `nside` come within
/// `radius_deg` of `(ra_deg, dec_deg)`?
///
/// Accepts iff the great-circle distance from the hint center to the cell
/// center is at most `radius_deg + margin`, the margin being a safe upper
/// bound on the cell circumradius (see [`cell_margin_rad`]): a hint circle
/// that reaches the cell at all must come that close to its center. A cell
/// whose center cannot be computed (a corrupt tile header that still passed
/// the load gate) is kept: the gate must never over-prune.
///
/// `radius_deg` is the hint circle radius (`--radius`, default
/// `params::DEFAULT_RADECS_RADIUS_DEG`).
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

    /// Center-to-center angle, degrees.
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
        // nside=1: the 12 base pixels cover [0, 12).
        assert!(healpix_center_xyz(11, 1).is_some());
        assert!(healpix_center_xyz(12, 1).is_none());
    }

    /// The 12 base-pixel centers pin the xy-scheme orientation: pixels 0..3
    /// are the north polar caps (lon 45/135/225/315, dec +asin(2/3)), 4..7 the
    /// equatorial belt (lon 0/90/180/270, dec 0) and 8..11 the south caps (the
    /// same longitudes, dec -asin(2/3)). See the module docs for why a tile
    /// number must be read as an xy number and not a FITS ring one.
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

    /// Ordering sanity: cell 0 sits in the north half and the last cell in the
    /// south half, and refinement is nested in the xy layout - the children of
    /// (base, x, y) at nside 2N are (base, 2x + dx, 2y + dy) - so every child
    /// center stays within the worst-case circumradius bound of its nside N
    /// parent center.
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

        // Every nside 2N child center is within the measured worst-case
        // xy-cell circumradius of its nside N parent center: 61 deg / N (see
        // `cell_margin_rad`).
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

    /// Base-pixel centers are far apart: the equatorial belt centers (dec 0)
    /// sit 90 deg apart at nside=1, a belt/polar-cap pair ~58 deg.
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

    /// Orientation pins against the two narrow-field centers: the nside=1
    /// cell 2 center is 16.87 deg from field 1 and 20.34 deg from field 2, so
    /// a radius-0 hint at either center must keep the tile (the nside=1 margin
    /// alone is ~99 deg). At nside=2 the closest cell center to field 1 is xy
    /// pixel 9 (5.42 deg) and to field 2 it is xy pixel 10 (5.79 deg); read as
    /// a FITS ring number, the value 9 would have named the cell at RA 247.5
    /// deg, dec +41.8 deg - 32 deg from field 1.
    #[test]
    fn winning_tile_contains_the_narrow_fields() {
        let f1 = radec(202.484305, 47.232352);
        let f2 = radec(250.423162, 36.479129);
        let c2 = healpix_center_xyz(2, 1).unwrap();
        assert!((angsep_deg(c2, f1) - 16.87).abs() < 0.01);
        assert!((angsep_deg(c2, f2) - 20.34).abs() < 0.01);
        assert!(tile_overlaps_circle(2, 1, 202.484305, 47.232352, 0.0));
        assert!(tile_overlaps_circle(2, 1, 250.423162, 36.479129, 0.0));
        // The same fields one subdivision finer: the cell center closest to
        // field 1 is xy pixel 9, the one closest to field 2 is xy pixel 10
        // (both less than 6 deg away).
        assert!(angsep_deg(healpix_center_xyz(9, 2).unwrap(), f1) < 6.0);
        assert!(angsep_deg(healpix_center_xyz(10, 2).unwrap(), f2) < 6.0);
    }

    /// Never over-prune: a radius-0 hint placed exactly on a cell center keeps
    /// that cell at every tested nside.
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
    /// radius keeps only a fraction of the sky (measured: 9/12 cells at
    /// nside=1 -- the nside=1 margin ~99 deg prunes little by design -- 12/48
    /// at nside=2, 16/192 at nside=4), and a far-away cell is rejected at
    /// every tested nside.
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
        // Known far cells (center 82-160 deg from the hint) are rejected at
        // each tested subdivision.
        assert!(!tile_overlaps_circle(8, 1, ra, dec, 10.0));
        assert!(!tile_overlaps_circle(47, 2, ra, dec, 10.0));
        assert!(!tile_overlaps_circle(150, 4, ra, dec, 10.0));
        // A wider hint keeps strictly more cells.
        assert!(keep(2, 20.0) > keep(2, 10.0));
        assert!(keep(4, 20.0) > keep(4, 10.0));
        // The cell whose center is nearest the hint's antipode (0.1 deg away
        // at nside=64) is always rejected.
        assert!(!tile_overlaps_circle(36177, 64, ra, dec, 10.0));
        // And the cell whose center is nearest the hint (nside=2 pixel 10,
        // 5.8 deg) always survives.
        assert!(tile_overlaps_circle(10, 2, ra, dec, 0.0));
    }

    /// A corrupt tile header the load gate somehow let through (nside <= 0,
    /// hp out of range) must keep the tile, never crash or over-prune.
    #[test]
    fn uncomputable_geometry_keeps_the_tile() {
        assert!(tile_overlaps_circle(0, 0, 0.0, 0.0, 0.0));
        assert!(tile_overlaps_circle(99, 1, 0.0, 0.0, 0.0));
    }

    /// The margin formula pin: sqrt(3) rad / nside, i.e. 99.24 deg at
    /// nside=1 in inverse proportion to nside, always above the measured
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

    /// No power-of-two requirement in the geometry: at nside=6, cell 20
    /// centers at RA 52.5 deg, dec +41.81 deg. A tile whose HPNSIDE is not a
    /// power of two is refused by the load gate in `index.rs` instead.
    #[test]
    fn non_power_of_two_nside_still_computes() {
        let v = healpix_center_xyz(20, 6).expect("nside 6 hp 20 exists");
        let (ra, dec) = xyz_to_radec(v);
        assert!((ra - 52.5).abs() < 1e-6, "ra {ra}");
        assert!((dec - 41.81031).abs() < 1e-4, "dec {dec}");
    }
}
