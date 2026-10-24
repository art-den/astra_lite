//! Field quad and code generation.
//!
//! A quad is an ordered backbone pair `(A, B)` plus the non-backbone stars:
//! `C` alone for `dimquads=3`, `C` and `D` for `dimquads=4`. Its code is the
//! image of the non-backbone stars under the unique orientation-preserving
//! similarity that sends `A -> (0, 0)` and `B -> (1, 1)` — in complex notation
//! `z -> (1 + i)(z - A) / (B - A)`: translate `A` to the origin, divide by the
//! backbone difference, then rotate by 45 degrees and scale by sqrt(2). For
//! the star `C` that works out to
//!
//! ```text
//! ABx = Bx - Ax, ABy = By - Ay, s = ABx*ABx + ABy*ABy
//! cos = (ABy + ABx) / s,  sin = (ABy - ABx) / s
//! ux = Cx - Ax, uy = Cy - Ay
//! cx =  ux*cos + uy*sin
//! cy = -ux*sin + uy*cos
//! ```
//!
//! Star `D` is coded by the same formula on the same backbone, so a
//! `dimquads=4` code is the four components `(cx, cy, dx, dy)`: C's image
//! followed by D's, and `dx, dy` are code components, not pixel differences.
//! The backbone itself always lands on `(0, 0)` and `(1, 1)`, and that
//! normalization is what makes a code a shape rather than a position: it does
//! not change when the field is translated, rotated or uniformly rescaled. A
//! non-backbone code is only bounded by the circle below, so its components
//! may leave the unit square.
//!
//! An index carries one such vector per quad, quantized into its codes
//! kd-tree record. The quantization window is the tree's range table, which
//! `kdtree.rs` reads from the file; `tree_ranges_match_the_code_circle`
//! (`index.rs`) pins it to the code circle's bounding box `0.5 +- 1/sqrt(2)` in
//! u16 levels, so
//! one stored step is sqrt(2)/65535 = 2.2e-5 against a `CODE_TOL` of 0.01
//! (~460 steps): bit-for-bit equality between a field code and a stored one is
//! not expected, and the ball search is what absorbs the difference. Whether a
//! stored record lines up with a code or with its parity flip depends on the
//! handedness of the mapping between the sky and the pixel plane, so for every
//! assignment `solver.rs` offers the whole orbit {code, 1 - code, flip(code),
//! 1 - flip(code)} to `canonical_ok`, which keeps only the members a
//! canonicalized index can hold and never the one a stored code lies within
//! `CODE_TOL` of; the search therefore does not depend on that choice, and the
//! parity test below checks it on quads whose sky positions are known.
//!
//! Two transformations of a code vector, each the same quad seen differently:
//! * [`parity_flip`] swaps each `(x, y)` pair. Reflecting the field in the
//!   line through `A` and `B` fixes both backbone stars and reverses
//!   orientation, and in the code frame that reflection is `(x, y) -> (y, x)`:
//!   the reflected quad is the flipped code, slot correspondence unchanged.
//! * [`swap_backbone`] exchanges the backbone labels, mapping
//!   `code -> 1 - code` component-wise — the half-turn about `(0.5, 0.5)`. The
//!   code is *not* invariant under it, so both backbone orders matter unless
//!   the field side canonicalizes with [`canonical_ok`].
//!
//! Valid position of a non-backbone star — the disc centered `(0.5, 0.5)` with
//! radius `1/sqrt(2)`, widened by `CODE_TOL` ([`in_circle`]). The code frame
//! scales lengths by sqrt(2)/|AB|, so in the field's own pixels the
//! untoleranced disc is the one having the backbone as diameter: radius
//! `|AB| / 2` about the midpoint of `AB`. The circle passes through all four
//! corners of the unit square, so a star that coincides with `A` or `B` (code
//! `(0,0)`/`(1,1)`) passes it: the enumeration must keep the slots distinct.

/// Code of star `P` for the backbone `A, B`: the similarity that sends
/// `A -> (0, 0)` and `B -> (1, 1)`, so the result is `(0,0)` for P=A and
/// `(1,1)` for P=B.
#[must_use]
pub fn star_code(ax: f64, ay: f64, bx: f64, by: f64, px: f64, py: f64) -> [f64; 2] {
    let dx = bx - ax;
    let dy = by - ay;
    let s = dx * dx + dy * dy;
    // `cos`/`sin` are the real and the negated imaginary part of the complex
    // factor (1 + i) / (B - A); their squared length is 2 / s, so they already
    // carry the frame's scale sqrt(2) / |AB| and are not unit cosine and sine.
    // The denominator is s = |AB|^2 (the norm of that division), not |AB|: it
    // is what lands B on exactly (1, 1).
    let cos = (dy + dx) / s;
    let sin = (dy - dx) / s;
    let ux = px - ax;
    let uy = py - ay;
    [ux * cos + uy * sin, -ux * sin + uy * cos]
}

/// Parity flip: swap `CX<->CY` and `DX<->DY`. Reflecting the field in the line
/// through `A` and `B` leaves the two backbone stars fixed and reverses
/// orientation, which in the code frame is exactly this coordinate swap, so
/// the reflected quad is the flipped code while the slot correspondence stays
/// put. Accepted lengths: 2 (`dimquads=3`) or 4 (`dimquads=4`). Returns the
/// flipped copy in a fixed-size array; for a length-2 input the trailing slots
/// stay zero, so slice the result to the input length.
#[must_use]
pub fn parity_flip(code: &[f64]) -> [f64; 4] {
    debug_assert!(code.len() == 2 || code.len() == 4);
    let mut out = [0.0; 4];
    // Bound to 2 pairs: at length >= 6 the fixed `[f64; 4]` out-bounds would
    // panic, and `swap_backbone` truncates the same way (its zip keeps the
    // first 4 components).
    for i in 0..usize::min(code.len() / 2, 2) {
        out[2 * i] = code[2 * i + 1];
        out[2 * i + 1] = code[2 * i];
    }
    out
}

/// Swapping the backbone stars A,B rotates the code by 180 degrees about the
/// unit square's center `(0.5, 0.5)`: `code -> 1 - code` component-wise, which
/// leaves the code disc invariant. Accepted lengths: 2 (`dimquads=3`) or 4
/// (`dimquads=4`). Returns the rotated copy in a fixed-size array; slice it to
/// the input length (see [`parity_flip`]).
#[must_use]
pub fn swap_backbone(code: &[f64]) -> [f64; 4] {
    debug_assert!(code.len() == 2 || code.len() == 4);
    let mut out = [0.0; 4];
    for (o, &c) in out.iter_mut().zip(code) {
        *o = 1.0 - c;
    }
    out
}

/// Circle filter: the code of a non-backbone star must lie within `codetol`
/// (L2, in code units) of the disc centered `(0.5, 0.5)` with radius
/// `1/sqrt(2)`. `cx*cx - cx + cy*cy - cy` is `|code - (0.5, 0.5)|^2 - 0.5`, so
/// the test below is the radius bound `|code - center| <= 1/sqrt(2) + codetol`
/// with the square expanded out; in the field's pixels the untoleranced disc
/// is the one having the backbone `AB` as diameter.
/// Every non-backbone star of a quad is filtered, not just C (`solver.rs`),
/// and an index whose codes were not built under this convention is refused at
/// load (`index.rs`), so a rejected quad has no stored code within `codetol`
/// either.
#[must_use]
pub fn in_circle(cx: f64, cy: f64, codetol: f64) -> bool {
    let r = (cx * cx - cx) + (cy * cy - cy);
    r <= codetol * (std::f64::consts::SQRT_2 + codetol)
}

/// Canonicalization filters applied to a candidate code vector before it is
/// queried against the tree; they drop field codes that an index built under
/// the same canonicalization cannot contain.
///
/// Precondition: `code` has length 2 (`dimquads=3`) or 4 (`dimquads=4`); an
/// empty or 1-element slice yields `meanx = 0/0 = NaN` and trivially passes.
///
/// * `cxdx`    (index key `CXDX`):   the x-components in slot order are
///   nondecreasing up to `margin` — `cx <= dx + margin`, and inert for
///   `dimquads=3`, which has a single x to compare against.
/// * `cxdxlt1` (index key `CXDXLT1`): mean of the x-components `<= 0.5 +
///   margin` — `cx <= 0.5 + margin` for `dimquads=3`, `cx + dx <= 1 +
///   2*margin` for `dimquads=4`.
///
/// What the flags promise about the STORED codes is what turns these tests
/// into safe pruning: an index that declares both keeps `cx <= dx` and
/// `cx + dx <= 1` for every stored code, hence mean x `<= 0.5` and `cx <= 0.5`
/// (the two together reach `cx = 0.5` only at `cx = dx = 0.5`) —
/// `dimquads4_index_satisfies_its_declared_constraints` re-asserts exactly
/// that on an installed index, up to quantization. Since
/// `mean_x(1 - code) = 1 - mean_x(code)`, exactly one of a code and its
/// `1 - code` partner passes the mean-x test, and both pass only inside
/// `margin` of the boundary; the parity flip gets a verdict of its own because
/// it moves the y-components into the x slots. Field backbones reach this
/// filter as enumerated — `A` before `B` in field-list order, never
/// canonicalized (`solver.rs`) — so a code can arrive with mean x `> 0.5` and
/// then only its `1 - code` variant can be the one that matches.
///
/// The two flags are COUPLED by the caller: `solver.rs` passes `cxdxlt1` only
/// when the index declares both `CXDX` and `CXDXLT1`, because the mean-x bound
/// rests on the stored codes having been canonicalized, not on the `cxdx`
/// argument alone. For an index whose codes are not canonicalized the safe
/// setting is `cxdxlt1 = false`, which prunes nothing.
///
/// The margin makes the pruning SOUND in one direction: a query within
/// `CODE_TOL` (an L2 ball) of a stored code always passes the filter. Two
/// bounds back this under that ball — the mean of the x-components shifts by
/// at most `CODE_TOL / sqrt(n)` (Cauchy-Schwarz over the n x-components; n = 1
/// needs the full `CODE_TOL`) and `cx - dx` by at most `sqrt(2) * CODE_TOL` —
/// so `margin >= sqrt(2) * CODE_TOL` covers both, and
/// `CXDX_MARGIN_FACTOR * CODE_TOL` = 1.5 * `CODE_TOL` clears it (1.5 > sqrt2).
/// Note the mean-x test looks at the mean only: a code with one x-component
/// above `0.5 + margin` still passes while its mean stays under it.
#[must_use]
pub fn canonical_ok(code: &[f64], cxdx: bool, cxdxlt1: bool, margin: f64) -> bool {
    let n = code.len() / 2;
    if cxdx {
        for j in 1..n {
            if code[2 * (j - 1)] > code[2 * j] + margin {
                return false;
            }
        }
    }
    if cxdxlt1 {
        let meanx = (0..n).map(|j| code[2 * j]).sum::<f64>() / f64::from(n as u32);
        if meanx > 0.5 + margin {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn approx(a: f64, b: f64) -> bool {
        f64::abs(a - b) < 1e-12
    }

    #[test]
    fn backbone_stars_map_to_zero_and_one() {
        // A -> (0,0), B -> (1,1) whatever the backbone geometry is.
        let (ax, ay, bx, by) = (10.5, -3.25, 312.75, 640.0);
        let za = star_code(ax, ay, bx, by, ax, ay);
        let zb = star_code(ax, ay, bx, by, bx, by);
        assert!(approx(za[0], 0.0) && approx(za[1], 0.0));
        assert!(approx(zb[0], 1.0) && approx(zb[1], 1.0));
    }

    #[test]
    fn swapping_backbone_rotates_code_180() {
        // c(B,A,C) == 1 - c(A,B,C): the reason both backbone orders (or the
        // canonicalization filter) are needed.
        let (ax, ay, bx, by) = (0.0, 0.0, 3.0, 4.0);
        let (cx, cy) = (2.5, -1.5);
        let fwd = star_code(ax, ay, bx, by, cx, cy);
        let bwd = star_code(bx, by, ax, ay, cx, cy);
        let rot = swap_backbone(&fwd);
        assert!(approx(bwd[0], rot[0]) && approx(bwd[1], rot[1]));
        assert!(f64::abs(fwd[0] - bwd[0]) > 0.1); // not a symmetry!
    }

    #[test]
    fn parity_flip_swaps_each_pair() {
        let c3 = [0.2, 0.7];
        let f3 = parity_flip(&c3);
        assert_eq!((&f3[..2]), [0.7, 0.2]);
        let c4 = [0.2, 0.7, 0.3, 0.9];
        let f4 = parity_flip(&c4);
        assert_eq!((&f4[..4]), [0.7, 0.2, 0.9, 0.3]);
    }

    #[test]
    fn circle_filter() {
        let tol = crate::params::CODE_TOL;
        // The circle passes through (0,0), (1,0), (0,1), (1,1) and its center
        // (0.5,0.5) is deep inside.
        assert!(in_circle(0.0, 0.0, tol));
        assert!(in_circle(1.0, 1.0, tol));
        assert!(in_circle(0.5, 0.5, tol));
        // Just outside: r = (1.1^2-1.1) + (0.9^2-0.9) = 0.02 > tol*(sqrt2+tol).
        assert!(!in_circle(1.1, 0.9, tol));
        assert!(in_circle(1.1, 0.9, 0.02));
    }

    #[test]
    fn canonical_filters() {
        let m = crate::params::CXDX_MARGIN_FACTOR * crate::params::CODE_TOL;
        assert!(canonical_ok(&[0.4, 0.3], false, true, m));
        assert!(!canonical_ok(&[0.6, 0.3], false, true, m));
        assert!(canonical_ok(&[0.5 + m / 2.0, 0.3], false, true, m));
        // CXDX: cx <= dx + margin
        assert!(canonical_ok(&[0.2, 0.3, 0.25, 0.6], true, true, m));
        assert!(!canonical_ok(&[0.25, 0.3, 0.2, 0.6], true, true, m));
        // CXDXLT1: mean x <= 0.5 + margin
        assert!(!canonical_ok(&[0.1, 0.3, 0.95, 0.6], true, true, m));
    }

    /// The parity convention end to end: the stored code of a quad is
    /// reproduced from the pixel coordinates of its stars (projected through
    /// the field-1 reference WCS) only after the parity flip (swap x/y).
    ///
    /// The quad is not pinned to an id: the index that solves field 1 is
    /// discovered by `testutil::field_run` and scanned for quads whose stars
    /// all land inside the field frame under `FIELD1_WCS` - the quads a
    /// field-1 solve can hit. In-frame quads are a tiny fraction of an
    /// all-sky index (the field covers ~2e-4 of the sky), so the scan is a
    /// full pass with a cheap same-hemisphere dot guard, capped at
    /// `MAX_QUADS` matches. The flipped-vs-unflipped gap is asserted per
    /// quad: a flipped code must match the stored one within 1e-3 while the
    /// unflipped one must sit more than 0.1 away, so a wrong parity convention
    /// cannot sneak through on the rare near-symmetric code (those are counted
    /// separately).
    #[test]
    fn in_field_quad_codes_need_parity_flip() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let wcs = crate::wcs::test_data::FIELD1_WCS;
        let d = index.meta.dimquads;
        // The frame's angular half-diagonal is ~2.2 deg at the reference WCS's
        // scale, so a 4 deg half-angle guard cannot drop a star that belongs
        // in frame, and it keeps the projection off the far hemisphere (where
        // TAN is undefined).
        let center = crate::coords::radec_to_xyz(
            crate::wcs::test_data::FIELD1_CENTER.0,
            crate::wcs::test_data::FIELD1_CENTER.1,
        );
        let cos_guard = f64::cos(4.0f64.to_radians());
        const MAX_QUADS: usize = 200;
        // Floor of discriminating quads the convention check needs: the asserts
        // below only run on quads that survive the guards and the
        // near-symmetry skip, so a scan that leaves none must fail here rather
        // than pass vacuously.
        const MIN_DISCRIMINATING_QUADS: usize = 10;

        let mut checked = 0usize;
        let mut symmetric = 0usize;
        for q in 0..index.meta.nquads {
            if checked >= MAX_QUADS {
                break;
            }
            let quad = index.quad(q);
            let mut px = [[0.0f64; 2]; 4];
            let mut inside = true;
            for j in 0..d {
                let xyz = index.star_xyz(quad[j] as usize);
                let dot = xyz[0] * center[0] + xyz[1] * center[1] + xyz[2] * center[2];
                if dot <= cos_guard {
                    inside = false;
                    break;
                }
                let (ra, dec) = crate::coords::xyz_to_radec(xyz);
                let (x, y) = wcs.radec_to_pix(ra, dec);
                if !(0.0..=run.width).contains(&x) || !(0.0..=run.height).contains(&y) {
                    inside = false;
                    break;
                }
                px[j] = [x, y];
            }
            if !inside {
                continue;
            }
            let stored = index.codes.record(q);
            if !stored.iter().all(|v| v.is_finite()) {
                continue;
            }
            // Field-side code of each non-backbone star (C[, D]) from the
            // projected pixels, compared against the stored code.
            let mut code = vec![0.0f64; 2 * (d - 2)];
            for j in 2..d {
                let c = star_code(px[0][0], px[0][1], px[1][0], px[1][1], px[j][0], px[j][1]);
                code[2 * (j - 2)] = c[0];
                code[2 * (j - 2) + 1] = c[1];
            }
            if !code.iter().all(|v| v.is_finite()) {
                continue;
            }
            let flip = parity_flip(&code);
            let dist = |a: &[f64]| {
                f64::sqrt(
                    a.iter()
                        .zip(stored.iter())
                        .map(|(x, y)| (x - y) * (x - y))
                        .sum::<f64>(),
                )
            };
            let d_flip = dist(&flip[..code.len()]);
            let d_raw = dist(&code);
            if d_raw <= 0.1 {
                // cx ~ cy (and dx ~ dy): the flip moves the code too little
                // to tell a right convention from a wrong one.
                symmetric += 1;
                continue;
            }
            assert!(d_flip <= 0.001, "quad {q}: flipped code off by {d_flip}");
            checked += 1;
        }
        // The 1e-3 per-quad bound is 100x below the 0.1 cutoff that skips
        // near-symmetric quads and over 40x above one u16 step of the codes
        // tree (2.2e-5), so it tests the convention while leaving room for the
        // difference between a code read off the sky and one rebuilt from
        // projected pixels.
        assert!(
            checked >= MIN_DISCRIMINATING_QUADS,
            "only {checked} discriminating in-field quads ({symmetric} near-symmetric \
             skipped): the code convention went unexercised"
        );
    }

    /// A `dimquads=4` index declares CIRCLE/CXDX/CXDXLT1 and really stores only
    /// codes that satisfy the circle for both C and D, `cx <= dx` and
    /// `cx + dx <= 1` (the mean-x bound for two non-backbone stars), up to the
    /// tree's quantization.
    #[test]
    fn dimquads4_index_satisfies_its_declared_constraints() {
        let Some(meta) = testutil::dim4_index() else {
            return;
        };
        let index = testutil::load(meta);
        assert!(index.meta.circle);
        assert!(index.meta.cx_less_than_dx);
        assert!(index.meta.meanx_less_than_half);
        // The codes tree's range table (`kdtree.rs` reads minval/maxval/scale
        // from the file) is the circle's bounding box 0.5 +- 1/sqrt(2) in u16
        // levels - `tree_ranges_match_the_code_circle` pins it - so one step is
        // sqrt(2)/65535 = 2.2e-5. Inside that window |2c - 1| <= sqrt(2), so a
        // one-step error in both components moves `c*c - c` by
        // 2 * sqrt(2) * (sqrt(2)/65535) = 4/65535 = 6.1e-5; the 1e-3 slack
        // below keeps ~16x of margin.
        let slack = 1e-3;
        let stride = usize::max(1, index.meta.nquads / 300);
        let mut checked = 0;
        for q in (0..index.meta.nquads).step_by(stride) {
            let c = index.codes.record(q);
            let (cx, cy, dx, dy) = (c[0], c[1], c[2], c[3]);
            // Circle for both C and D (CIRCLE=T), within quantization slack.
            assert!(
                (cx * cx - cx) + (cy * cy - cy) <= slack,
                "quad {q}: C code ({cx},{cy}) outside the circle"
            );
            assert!(
                (dx * dx - dx) + (dy * dy - dy) <= slack,
                "quad {q}: D code ({dx},{dy}) outside the circle"
            );
            // CXDX and CXDXLT1 (exact up to quantization, no solver margin).
            assert!(cx <= dx + slack, "quad {q}: cx {cx} > dx {dx}");
            assert!(cx + dx <= 1.0 + slack, "quad {q}: cx+dx {}", cx + dx);
            checked += 1;
        }
        assert!(checked > 100);
    }
}
