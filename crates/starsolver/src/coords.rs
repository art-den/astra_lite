//! Unit-sphere math: direction vectors `xyz` <-> equatorial `(RA, Dec)` in
//! degrees, plus the angle conversions and the chord^2 (squared angular
//! distance) helpers other modules compare their angular thresholds in.
//!
//! The frame is right-handed: a direction is
//! `[cos dec * cos ra, cos dec * sin ra, sin dec]`, with +x at `(RA 0, Dec 0)`,
//! +y at `(RA 90, Dec 0)` and +z at the north celestial pole, so RA grows from
//! +x toward +y. Names carry the units: `...rad` is radians, `...arcsec` is
//! arcseconds, everything else degrees.

/// arcsec per degree.
pub const DEG2ARCSEC: f64 = 3600.0;
/// arcsec per arcmin.
pub const ARCMIN2ARCSEC: f64 = 60.0;
/// arcsec per radian = `180 * 3600 / pi`.
pub const RAD2ARCSEC: f64 = 206264.80624709636;

/// Radians -> arcsec: one multiply by [`RAD2ARCSEC`]; [`arcsec2rad`] divides
/// by the same constant.
#[must_use]
pub fn rad2arcsec(rad: f64) -> f64 {
    rad * RAD2ARCSEC
}

/// Degrees -> arcsec.
#[must_use]
pub fn deg2arcsec(deg: f64) -> f64 {
    deg * DEG2ARCSEC
}

/// Arcmin -> arcsec.
#[must_use]
pub fn arcmin2arcsec(arcmin: f64) -> f64 {
    arcmin * ARCMIN2ARCSEC
}

/// Arcsec -> radians.
#[must_use]
pub fn arcsec2rad(arcsec: f64) -> f64 {
    arcsec / RAD2ARCSEC
}

/// Degrees -> radians: forwards to `f64::to_radians`, a single multiply by
/// `pi / 180`.
#[must_use]
pub fn deg2rad(deg: f64) -> f64 {
    deg.to_radians()
}

/// Radians -> degrees: forwards to `f64::to_degrees`, a single multiply by
/// `180 / pi`.
///
/// `deg2rad` and `rad2deg` are two independent roundings, so their round trip
/// is not the identity: measured on a fine grid (step 1e-4 deg) over 0..370
/// degrees, ~11.6% of the angles come back exactly one ulp off. Compare
/// across such a trip with a tolerance.
#[must_use]
pub fn rad2deg(rad: f64) -> f64 {
    rad.to_degrees()
}

/// Squared Euclidean distance between two direction vectors: on the unit
/// sphere this is the chord^2, `2 - 2 cos(theta)` for separation `theta`.
///
/// It grows monotonically with the angular separation, so thresholds can be
/// compared here without any inverse trig: `solver::PositionHint::precompute`
/// turns the hint radius into chord^2, and `refine::match_pairs` does the
/// same for its field bounding circle (the chord from the field center to
/// the LL corner); the match radius there stays in pixels.
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
/// The chord^2 is clamped to at most 4 before the `acos`. The vectors fed in
/// are only approximately unit - index star positions are dequantized
/// component-wise and never renormalized - so a near-antipodal pair can land
/// a hair above 4; unclamped, `1 - chord2 / 2` would fall below -1 and `acos`
/// would answer NaN. Clamped, such a chord^2 reports pi, and so do +inf and
/// NaN (the latter because `f64::min` returns its non-NaN operand; that is
/// why `min` is used where `f64::clamp` would propagate the NaN). A negative
/// chord^2 is deliberately left alone: its `acos` argument exceeds 1 and the
/// result stays NaN. Pinned by the tests below.
#[must_use]
pub fn chord2_to_rad(chord2: f64) -> f64 {
    f64::acos(1.0 - f64::min(chord2 / 2.0, 2.0))
}

/// Great-circle angle in degrees -> chord^2 on the unit sphere, as the
/// half-angle form `4 sin^2(d / 2)`.
///
/// That is `2(1 - cos d)` by identity, but it keeps its digits as d shrinks:
/// `1 - cos d` cancels and is already exactly 0.0 below about 6.0e-7 degrees,
/// where this form still returns ~1.1e-16.
///
/// Converts the `--ra/--dec/--radius` hint circle: the radius in degrees
/// becomes a chord^2 radius compared against [`distsq`] (see
/// `solver::PositionHint`).
#[must_use]
pub fn deg2distsq(deg: f64) -> f64 {
    let half = f64::sin(deg2rad(deg) / 2.0);
    4.0 * half * half
}

/// Unit-sphere direction of `(ra, dec)` in degrees, in the frame described in
/// the module docs.
///
/// No wrapping or range checks: the degrees go straight into the trig, so an
/// RA of 720 lands back on RA 0 within rounding, and a dec outside
/// `[-90, 90]` gives the direction of some other point rather than an error.
/// The result is unit length only to rounding; nothing normalizes it.
#[must_use]
pub fn radec_to_xyz(ra_deg: f64, dec_deg: f64) -> [f64; 3] {
    let ra = deg2rad(ra_deg);
    let dec = deg2rad(dec_deg);
    let cd = f64::cos(dec);
    [cd * f64::cos(ra), cd * f64::sin(ra), f64::sin(dec)]
}

/// `(ra, dec)` in degrees from a direction vector: RA normalized to
/// `[0, 360)` (one `+ 360.0` suffices because `atan2` never returns below
/// -180 degrees), Dec in `[-90, 90]`.
///
/// Dec is `atan2(z, hypot(x, y))` rather than `asin(z)`, so only the vector
/// *direction* is read: any positive multiple of a vector gives the same
/// answer, and a vector longer than the unit one cannot leave the domain the
/// way `asin(z)` would. The zero vector has no direction; it comes back as
/// `(0, 0)`.
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
        // The literal must equal 180 * 3600 / pi bit for bit.
        assert_eq!(RAD2ARCSEC, (180.0 * 3600.0) / PI);
    }

    #[test]
    fn arcsec_helpers_roundtrip() {
        for &x in &[0.0, 0.5, 3600.0, 206264.806_247_096_36] {
            assert!((arcsec2rad(rad2arcsec(x)) - x).abs() <= f64::EPSILON * x);
        }
        // Unit conversions through arcsec.
        assert_eq!(deg2arcsec(1.0), 3600.0);
        assert_eq!(arcmin2arcsec(1.0), 60.0);
        assert!((rad2deg(arcsec2rad(deg2arcsec(90.0))) - 90.0).abs() < 1e-10);
    }

    #[test]
    fn chord2_to_rad_pins() {
        // Pins the clamp documented on chord2_to_rad: chord^2 > 4 and NaN
        // both map to pi.
        assert_eq!(chord2_to_rad(0.0), 0.0);
        assert_eq!(chord2_to_rad(4.0), PI);
        // The clamp boundary again, not the >4 path: EPSILON is sub-ulp at
        // 4.0 (ulp(4.0) = 4 * EPSILON), so this sum is exactly 4.0; the >4
        // path is pinned by +inf below.
        assert_eq!(chord2_to_rad(4.0 + f64::EPSILON), PI);
        assert_eq!(chord2_to_rad(f64::INFINITY), PI, "inf is clamped -> pi");
        // Negative is not clamped: acos gets an argument > 1, so NaN.
        assert!(chord2_to_rad(-1e-9).is_nan());
        // f64::min(NaN, 2.0) returns the non-NaN operand (2.0), so
        // chord2_to_rad(NaN) = acos(-1) = pi.
        assert_eq!(chord2_to_rad(f64::NAN), PI, "min swallows NaN -> pi");
        // A mid-range value is unaffected by the clamp.
        assert!((chord2_to_rad(2.0) - PI / 2.0).abs() <= 4.0 * f64::EPSILON);
    }

    #[test]
    fn deg2distsq_chord2_roundtrip() {
        // A sub-degree --ra/--dec hint radius must roundtrip through the
        // (clamped) chord^2 -> rad inverse.
        let deg = 0.5;
        let back = rad2deg(chord2_to_rad(deg2distsq(deg)));
        assert!((back - deg).abs() < 1e-12, "back = {back}");
        // 180 deg lands exactly on the clamp boundary (chord^2 = 4).
        assert!((chord2_to_rad(deg2distsq(180.0)) - PI).abs() < 1e-12);
    }
}
