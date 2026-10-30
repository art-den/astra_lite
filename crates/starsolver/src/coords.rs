//! Unit-sphere math: direction vectors `xyz` <-> equatorial `(RA, Dec)` in
//! degrees, plus the angle conversions and the chord^2 (squared angular
//! distance) helpers other modules compare angular thresholds in.
//!
//! Right-handed frame: `[cos dec * cos ra, cos dec * sin ra, sin dec]`, +x at
//! `(RA 0, Dec 0)`, +y at `(RA 90, Dec 0)`, +z at the north celestial pole, so
//! RA grows from +x toward +y. Names carry the units: `...rad` radians,
//! `...arcsec` arcseconds, everything else degrees.

pub const DEG2ARCSEC: f64 = 3600.0;
pub const ARCMIN2ARCSEC: f64 = 60.0;
/// `180 * 3600 / pi`, exactly.
pub const RAD2ARCSEC: f64 = 206264.80624709636;

#[must_use]
pub fn rad2arcsec(rad: f64) -> f64 {
    rad * RAD2ARCSEC
}

#[must_use]
pub fn deg2arcsec(deg: f64) -> f64 {
    deg * DEG2ARCSEC
}

#[must_use]
pub fn arcmin2arcsec(arcmin: f64) -> f64 {
    arcmin * ARCMIN2ARCSEC
}

#[must_use]
pub fn arcsec2rad(arcsec: f64) -> f64 {
    arcsec / RAD2ARCSEC
}

#[must_use]
pub fn deg2rad(deg: f64) -> f64 {
    deg.to_radians()
}

/// The `deg2rad`/`rad2deg` round trip is not the identity (two independent
/// roundings; on a 1e-4 deg grid over 0..370 deg, ~11.6% of angles come back
/// exactly one ulp off), so compare across such a trip with a tolerance.
#[must_use]
pub fn rad2deg(rad: f64) -> f64 {
    rad.to_degrees()
}

/// Squared Euclidean distance between two direction vectors: on the unit
/// sphere this is the chord^2, `2 - 2 cos(theta)` for separation `theta`.
///
/// Monotonic in the angular separation, so angular thresholds can be compared
/// here without inverse trig (`solver::PositionHint::precompute` for the hint
/// radius, `refine::match_pairs` for the field bounding circle (the chord from
/// the field center to the LL corner); the match radius there stays in pixels).
#[must_use]
pub fn distsq(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    let mut d = 0.0;
    for i in 0..3 {
        let t = a[i] - b[i];
        d += t * t;
    }
    d
}

/// Chord^2 on the unit sphere -> great-circle angle in radians, inverting
/// `chord2 = 2 - 2 cos(theta)`.
///
/// chord^2 is clamped to at most 4 before the `acos`: the vectors fed in are
/// only approximately unit (index star positions are dequantized
/// component-wise, never renormalized), so a near-antipodal pair can land a
/// hair above 4, where unclamped `1 - chord2 / 2` falls below -1 and `acos`
/// answers NaN. Such a chord^2 reports pi, and so do +inf and NaN (`f64::min`
/// returns its non-NaN operand - why `min` and not `f64::clamp`, which would
/// propagate the NaN). A negative chord^2 is deliberately left alone: its
/// `acos` argument exceeds 1, so the result is NaN. Pinned by the tests below.
#[must_use]
pub fn chord2_to_rad(chord2: f64) -> f64 {
    f64::acos(1.0 - f64::min(chord2 / 2.0, 2.0))
}

/// Great-circle angle in degrees -> chord^2 on the unit sphere, as the
/// half-angle form `4 sin^2(d / 2)`: equal to `2(1 - cos d)` by identity, but
/// it keeps its digits as d shrinks (`1 - cos d` cancels and is already exactly
/// 0.0 below about 6.0e-7 degrees, where this form still returns ~1.1e-16).
/// Converts the `--ra/--dec/--radius` hint: its radius in degrees becomes a
/// chord^2 compared against [`distsq`] (`solver::PositionHint`).
#[must_use]
pub fn deg2distsq(deg: f64) -> f64 {
    let half = f64::sin(deg2rad(deg) / 2.0);
    4.0 * half * half
}

/// Unit-sphere direction of `(ra, dec)` in degrees (frame: module docs). No
/// wrapping or range checks: an RA of 720 lands back on RA 0 within rounding,
/// a dec outside `[-90, 90]` gives some other point's direction rather than an
/// error. Unit length only to rounding; nothing normalizes.
#[must_use]
pub fn radec_to_xyz(ra_deg: f64, dec_deg: f64) -> [f64; 3] {
    let ra = deg2rad(ra_deg);
    let dec = deg2rad(dec_deg);
    let cd = f64::cos(dec);
    [cd * f64::cos(ra), cd * f64::sin(ra), f64::sin(dec)]
}

/// `(ra, dec)` in degrees from a direction vector: RA in `[0, 360)` (one
/// `+ 360.0` suffices because `atan2` never returns below -180 degrees), Dec in
/// `[-90, 90]`. Dec is `atan2(z, hypot(x, y))`, not `asin(z)`, so only the
/// vector *direction* is read: any positive multiple gives the same answer and
/// an over-long vector cannot leave the domain the way `asin(z)` would. The
/// zero vector has no direction; it comes back as `(0, 0)`.
#[must_use]
pub fn xyz_to_radec(v: [f64; 3]) -> (f64, f64) {
    let ra_rad = f64::atan2(v[1], v[0]);
    let dec_rad = f64::atan2(v[2], f64::hypot(v[0], v[1]));
    let mut ra_deg = rad2deg(ra_rad);
    if ra_deg < 0.0 {
        ra_deg += 360.0;
    }
    (ra_deg, rad2deg(dec_rad))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn radec_roundtrip() {
        let v = radec_to_xyz(60.0, 36.0);
        let (ra, dec) = xyz_to_radec(v);
        assert!((ra - 60.0).abs() < 1e-10);
        assert!((dec - 36.0).abs() < 1e-10);
    }

    #[test]
    fn ra_wraps_negative() {
        let (ra, _) = xyz_to_radec(radec_to_xyz(-10.0, 0.0));
        assert!((ra - 350.0).abs() < 1e-10);
    }

    #[test]
    fn rad2arcsec_const() {
        assert_eq!(RAD2ARCSEC, (180.0 * 3600.0) / PI);
    }

    #[test]
    fn arcsec_helpers_roundtrip() {
        for &x in &[0.0, 0.5, 3600.0, 206264.806_247_096_36] {
            assert!((arcsec2rad(rad2arcsec(x)) - x).abs() <= f64::EPSILON * x);
        }
        assert_eq!(deg2arcsec(1.0), 3600.0);
        assert_eq!(arcmin2arcsec(1.0), 60.0);
        assert!((rad2deg(arcsec2rad(deg2arcsec(90.0))) - 90.0).abs() < 1e-10);
    }

    #[test]
    fn chord2_to_rad_pins() {
        assert_eq!(chord2_to_rad(0.0), 0.0);
        assert_eq!(chord2_to_rad(4.0), PI);
        // The clamp boundary again, not >4: EPSILON is sub-ulp at 4.0
        // (ulp(4.0) = 4 * EPSILON), so this sum is exactly 4.0 (+inf pins >4).
        assert_eq!(chord2_to_rad(4.0 + f64::EPSILON), PI);
        assert_eq!(chord2_to_rad(f64::INFINITY), PI, "inf is clamped -> pi");
        assert!(chord2_to_rad(-1e-9).is_nan());
        assert_eq!(chord2_to_rad(f64::NAN), PI, "min swallows NaN -> pi");
        assert!((chord2_to_rad(2.0) - PI / 2.0).abs() <= 4.0 * f64::EPSILON);
    }

    #[test]
    fn deg2distsq_chord2_roundtrip() {
        // A sub-degree --ra/--dec hint radius must roundtrip through the
        // clamped chord^2 -> rad inverse.
        let deg = 0.5;
        let back = rad2deg(chord2_to_rad(deg2distsq(deg)));
        assert!((back - deg).abs() < 1e-12, "back = {back}");
        // 180 deg lands exactly on the clamp boundary (chord^2 = 4).
        assert!((chord2_to_rad(deg2distsq(180.0)) - PI).abs() < 1e-12);
    }
}
