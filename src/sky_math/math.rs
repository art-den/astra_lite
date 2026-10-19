use std::{f64::consts::PI, fmt::Debug, ops::{Mul, Sub}};
use chrono::{Datelike, Timelike, NaiveDateTime, NaiveDate};
use serde::Serialize;
use ::indi::{degree_to_str, hour_to_str};
use super::solar_system::pn_matrix;

#[derive(Clone, Copy, Default, Serialize)]
pub struct EqCoord {
    pub dec: f64, // in radians
    pub ra:  f64, // in radians
}

impl EqCoord {
    pub fn angle_between(crd1: &EqCoord, crd2: &EqCoord) -> f64 {
        let sin_diff_dec = f64::sin((crd2.dec - crd1.dec) / 2.0);
        let sin_diff_ra = f64::sin((crd2.ra - crd1.ra) / 2.0);
        let root_expr =
            sin_diff_dec * sin_diff_dec +
            f64::cos(crd1.dec) * f64::cos(crd2.dec) * sin_diff_ra * sin_diff_ra;
        2.0 * f64::asin(f64::sqrt(root_expr))
    }

    //  dec:           ra:
    //  ^Z             ^X
    //  |   *          |   *
    //  |  /           |ra/
    //  | /            | /
    //  |/ dec         |/
    //  *--------XY    O-------->Y
    pub fn to_sphere_pt(&self) -> Point3D {
        let rcst = f64::cos(self.dec);
        Point3D {
            x: rcst * f64::cos(self.ra),
            y: rcst * f64::sin(self.ra),
            z: f64::sin(self.dec)
        }
    }

    pub fn from_sphere_pt(pt: &Point3D) -> Self {
        let dec = f64::atan2(pt.z, f64::sqrt(pt.x * pt.x + pt.y * pt.y));
        let mut ra = f64::atan2(pt.y, pt.x);
        if ra < 0.0 {
            ra += 2.0 * PI;
        }
        Self { dec, ra }
    }
}

impl Debug for EqCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EqCoord")
            .field("ra", &hour_to_str(radian_to_hour(self.ra)))
            .field("dec", &degree_to_str(radian_to_degree(self.dec)))
            .finish()
    }
}

#[test]
fn test_eq_coord_to_sphere() {
    let test = |crd: EqCoord| {
        let mut pt = crd.to_sphere_pt();

        pt.x *= 0.5;
        pt.y *= 0.5;
        pt.z *= 0.5;

        let crd_from = EqCoord::from_sphere_pt(&pt);
        assert!(f64::abs(crd.dec - crd_from.dec) < 1e-8);
        assert!(f64::abs(crd.ra - crd_from.ra) < 1e-8);
    };

    test(EqCoord { dec: 0.0, ra: 0.0 });
    test(EqCoord { dec: PI / 2.0, ra: 0.0 });
    test(EqCoord { dec: 0.0, ra: PI / 2.0 });
    test(EqCoord { dec: PI / 2.0, ra: PI / 2.0 });
    test(EqCoord { dec: PI / 4.0, ra: PI / 4.0 });
    test(EqCoord { dec: PI / 8.0, ra: PI / 8.0 });
}

// XYZ mapping:
//
//  ^X(alt=Pi/2)
//  |   North
//  |   Z(az=0,alt=0)
//  |  /
//  | /
//  |/
//  *----->Y(az=Pi/2,alt=0) East
#[derive(Clone, Copy)]
pub struct HorizCoord {
    pub alt: f64,
    pub az:  f64,
}

impl HorizCoord {
    pub fn to_sphere_pt(&self) -> Point3D {
        let x = f64::sin(self.alt);
        let r = f64::cos(self.alt);
        let y = r * f64::sin(self.az);
        let z = r * f64::cos(self.az);
        Point3D { x, y, z }
    }

    pub fn from_sphere_pt(pt: &Point3D) -> Self {
        Self {
            az: f64::atan2(pt.y, pt.z),
            alt: f64::asin(pt.x),
        }
    }
}

impl Debug for HorizCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HorizCoord")
            .field("alt", &degree_to_str(radian_to_degree(self.alt)))
            .field("az", &degree_to_str(radian_to_degree(self.az)))
            .finish()
    }
}


#[derive(Default)]
pub struct RotMatrix {
    sin: f64,
    cos: f64,
}

impl RotMatrix {
    pub fn new(angle: f64) -> Self {
        Self {
            sin: f64::sin(angle),
            cos: f64::cos(angle),
        }
    }

    pub fn rotate(&self, x: &mut f64, y: &mut f64) {
        let res_x = self.cos * *x - self.sin * *y;
        let res_y = self.sin * *x + self.cos * *y;
        *x = res_x;
        *y = res_y;
    }
}

// Screen mapping:
//  ^X
//  |   Z
//  |  /
//  | /
//  |/
//  *----->Y
#[derive(Debug, Clone)]
pub struct Point3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Point3D {
    pub fn length(&self) -> f64 {
        f64::sqrt(self.x * self.x + self.y * self.y + self.z * self.z)
    }

    pub fn rotate_over_x_mat(&mut self, mat: &RotMatrix) {
        mat.rotate(&mut self.z, &mut self.y);
    }

    pub fn rotate_over_x(&mut self, angle: f64) {
        self.rotate_over_x_mat(&RotMatrix::new(angle));
    }

    pub fn rotate_over_y_mat(&mut self, mat: &RotMatrix) {
        mat.rotate(&mut self.z, &mut self.x);
    }

    pub fn rotate_over_y(&mut self, angle: f64) {
        self.rotate_over_y_mat(&RotMatrix::new(angle));
    }

    pub fn rotate_over_z_mat(&mut self, mat: &RotMatrix) {
        mat.rotate(&mut self.y, &mut self.x);
    }

    pub fn rotate_over_z(&mut self, angle: f64) {
        self.rotate_over_z_mat(&RotMatrix::new(angle));
    }

    pub fn normalized(&self) -> Option<Self> {
        let len = self.length();
        if len != 0.0 {
            Some(Self {
                x: self.x / len,
                y: self.y / len,
                z: self.z / len,
            })
        } else {
            None
        }
    }

    pub fn normal(pt1: &Self, pt2: &Self, pt3: &Self) -> Self {
        let vec1 = pt2 - pt1;
        let vec2 = pt3 - pt2;
        &vec1 * &vec2
    }

    pub fn angle(pt1: &Self, pt2: &Self) -> Option<f64> {
        let len1 = pt1.length();
        let len2 = pt2.length();
        if len1 != 0.0 && len2 != 0.0 {
            let scalar_prod = (pt1.x * pt2.x + pt1.y * pt2.y + pt1.z * pt2.z) / (len1 * len2);
            let scalar_prod = scalar_prod.clamp(-1.0, 1.0);
            Some(f64::acos(scalar_prod))
        } else {
            None
        }
    }
}

impl Mul<&Matrix33> for &Point3D {
    type Output = Point3D;

    fn mul(self, mat: &Matrix33) -> Self::Output {
        Point3D {
            x: mat.a11 * self.x + mat.a12 * self.y + mat.a13 * self.z,
            y: mat.a21 * self.x + mat.a22 * self.y + mat.a23 * self.z,
            z: mat.a31 * self.x + mat.a32 * self.y + mat.a33 * self.z,
        }
    }
}

// Cross product
impl Mul<&Point3D> for &Point3D {
    type Output = Point3D;

    fn mul(self, other: &Point3D) -> Self::Output {
        Point3D {
            x: self.z * other.y - self.y * other.z,
            y: self.x * other.z - self.z * other.x,
            z: self.y * other.x - self.x * other.y,
        }
    }
}

impl Sub<&Point3D> for &Point3D {
    type Output = Point3D;

    fn sub(self, other: &Point3D) -> Self::Output {
        Point3D {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }
}

#[test]
fn test_point3d_rotate() {
    let mut pt = Point3D { x: 0.0, y: 0.0, z: 1.0 };
    let mat = RotMatrix::new(degree_to_radian(90.0));
    pt.rotate_over_x_mat(&mat);
    assert!(f64::abs(pt.x-0.0) < 1e-10);
    assert!(f64::abs(pt.y-1.0) < 1e-10);
    assert!(f64::abs(pt.z-0.0) < 1e-10);
}

pub struct Matrix33 {
    pub a11: f64, pub a12: f64, pub a13: f64,
    pub a21: f64, pub a22: f64, pub a23: f64,
    pub a31: f64, pub a32: f64, pub a33: f64,
}

pub struct EpochCvt {
    pn_mat: Matrix33,
}

impl EpochCvt {
    pub fn new(time0: &NaiveDateTime, time: &NaiveDateTime) -> Self {
        let centuries0 = calc_julian_centuries(time0);
        let centuries = calc_julian_centuries(time);
        Self {
            pn_mat: pn_matrix(centuries0, centuries),
        }
    }

    pub fn convert_eq(&self, crd: &EqCoord) -> EqCoord {
        let pt = crd.to_sphere_pt();
        let pt = &pt * &self.pn_mat;
        EqCoord::from_sphere_pt(&pt)
    }

    pub fn convert_pt(&self, pt: &Point3D) -> Point3D {
        pt * &self.pn_mat
    }
}

#[derive(Default)]
pub struct EqToSphereCvt {
    z_rot: RotMatrix,
    y_rot: RotMatrix,
    n_z_rot: RotMatrix,
    n_y_rot: RotMatrix,
}

impl EqToSphereCvt {
    pub fn new(
        longitude: f64,
        latitude: f64,
        utc_time: &NaiveDateTime,
    ) -> Self {
        let lst = calc_sidereal_time(utc_time) + longitude;

        let ra_rot = RotMatrix::new(lst);
        let dec_rot = RotMatrix::new(latitude);

        let n_ra_rot = RotMatrix::new(-lst);
        let n_dec_rot = RotMatrix::new(-latitude);

        Self { z_rot: ra_rot, y_rot: dec_rot, n_z_rot: n_ra_rot, n_y_rot: n_dec_rot }
    }

    pub fn apply(&self, pt: &mut Point3D) {
        pt.rotate_over_z_mat(&self.z_rot);
        pt.rotate_over_y_mat(&self.y_rot);
    }

    pub fn eq_to_sphere(&self, eq_crd: &EqCoord) -> Point3D {
        let mut result = eq_crd.to_sphere_pt();
        self.apply(&mut result);
        result
    }

    pub fn sphere_to_eq(&self, pt: &Point3D) -> EqCoord {
        let mut pt = pt.clone();
        pt.rotate_over_y_mat(&self.n_y_rot);
        pt.rotate_over_z_mat(&self.n_z_rot);
        EqCoord::from_sphere_pt(&pt)
    }
}

pub fn radian_to_degree(radian: f64) -> f64 {
    180.0 * radian / PI
}

pub const fn degree_to_radian(degree: f64) -> f64 {
    PI * degree / 180.0
}

pub fn arcmin_to_radian(arcmin: f64) -> f64 {
    PI * arcmin / (60.0 * 180.0)
}

pub fn radian_to_hour(radian: f64) -> f64 {
    12.0 * radian / PI
}

pub fn hour_to_radian(hour: f64) -> f64 {
    PI * hour / 12.0
}

pub fn calc_julian_day(date: &NaiveDate) -> i64 {
    let mon = date.month() as i64;
    let day = date.day() as i64;
    let year = date.year() as i64;
    let a = (14 - mon) / 12;
    let y = year + 4800 - a;
    let m = mon + 12 * a - 3;
    day + (153 * m + 2)/5 + 365*y + y/4 - y/100 + y/400 - 32045
}

#[test]
fn test_calc_julian_day() {
    assert_eq!(
        calc_julian_day(&NaiveDate::from_ymd_opt(2001, 1, 1).unwrap()),
        2_451_911
    );
}

pub fn calc_julian_time(dt: &NaiveDateTime) -> f64 {
    let julian_day = calc_julian_day(&dt.date()) as f64;
    let hour = dt.hour() as f64;
    let min = dt.minute() as f64;
    let mut sec = dt.second() as f64;
    let msecs = (dt.nanosecond() / 1_000_000) as f64;
    sec += msecs / 1000.0;
    julian_day + (hour - 12.0) / 24.0 + min / 1440.0 + sec / 86400.0
}

pub fn calc_julian_centuries(dt: &NaiveDateTime) -> f64 {
    let jdt = calc_julian_time(dt);
    (jdt - 2451545.0) / 36525.0
}

pub fn calc_sidereal_time(dt: &NaiveDateTime) -> f64 {
    let jdt = calc_julian_time(dt);
    let dtt = jdt - 2451545.0;
    let t = dtt / 36525.0;
    let mut result_in_degrees =
        280.46061837
        + 360.98564736629 * dtt
        + 0.000387933 * t * t
        - (t * t * t) / 38710000.0;
    result_in_degrees = 360.0 * f64::fract(result_in_degrees / 360.0);
    degree_to_radian(result_in_degrees)
}

pub fn j2000_time() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2000, 1, 1).unwrap().and_hms_opt(12, 0, 0).unwrap()
}

/// Corrects the apparent (observed) altitude of a celestial body for
/// atmospheric refraction and returns the true altitude (both in radians).
/// Bennett (1982) formula with Meeus refinement, standard atmosphere
/// (1010 hPa, 10 °C). Accurate to ~1" for altitudes above ~5 degrees.
///
/// See <https://en.wikipedia.org/wiki/Atmospheric_refraction> and
/// Bennett (1982), J. Nav. 35(2) 255, <https://doi.org/10.1017/S0373463300022037>
pub fn correct_atmospheric_refraction(apparent_alt: f64) -> f64 {
    let h = radian_to_degree(apparent_alt);
    // Formula is unreliable below this apparent altitude, no correction there
    const MIN_ALT_DEG: f64 = -1.0;
    if !h.is_finite() || h < MIN_ALT_DEG {
        return apparent_alt;
    }
    // Clamp to the zenith (input above 90 deg is out of range)
    let h = f64::min(h, 90.0);
    // Refraction in arcminutes, h in degrees
    let r0 = 1.0 / f64::tan(degree_to_radian(h + 7.31 / (h + 4.4)));
    // Meeus refinement (argument in degrees), reduces max error to ~1"
    let r = r0 - 0.06 * f64::sin(degree_to_radian(14.7 * r0 + 13.0));
    let r = f64::max(r, 0.0);
    // Refraction lifts objects, so the true altitude is lower
    degree_to_radian(h - r / 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference refraction R (in arcminutes) for apparent altitudes,
    // Bennett (1982), standard atmosphere (1010 hPa, 10 °C).
    // Source: Bennett 1982 / Meeus "Astronomical Algorithms" ch. 15 tables.
    const REF_TABLE: [(f64, f64); 10] = [
        // (apparent alt, deg; R, arcmin)
        (0.0, 34.48),
        (0.5, 28.76),
        (1.0, 24.33),
        (2.0, 18.22),
        (3.0, 14.35),
        (5.0, 9.88),
        (10.0, 5.39),
        (20.0, 2.70),
        (45.0, 1.00),
        (60.0, 0.58),
    ];

    // Tolerance in arcmin: covers the difference between the plain Bennett
    // table and our Meeus-refined variant (~0.06'), leaves headroom
    const TOL_ARCMIN: f64 = 0.15;

    #[test]
    fn refraction_matches_bennett_table() {
        for &(alt_deg, r_arcmin) in &REF_TABLE {
            let apparent = degree_to_radian(alt_deg);
            let true_alt = correct_atmospheric_refraction(apparent);
            let got_arcmin = radian_to_degree(apparent - true_alt) * 60.0;
            assert!(
                f64::abs(got_arcmin - r_arcmin) < TOL_ARCMIN,
                "alt={alt_deg} deg: expected R={r_arcmin}', got {got_arcmin}'",
            );
        }
    }

    #[test]
    fn correction_always_lowers_altitude() {
        // Up to 88 deg: near the zenith the correction shrinks below 0.01'
        let mut alt = 0.0;
        while alt <= 88.0 {
            let apparent = degree_to_radian(alt);
            let true_alt = correct_atmospheric_refraction(apparent);
            assert!(
                true_alt < apparent,
                "alt={alt} deg: true altitude must be below apparent",
            );
            alt += 1.0;
        }
    }

    #[test]
    fn refraction_decreases_with_altitude() {
        let r = |alt_deg: f64| {
            let apparent = degree_to_radian(alt_deg);
            radian_to_degree(apparent - correct_atmospheric_refraction(apparent))
        };
        let mut alt = 0.0;
        while alt < 89.0 {
            assert!(
                r(alt) > r(alt + 1.0),
                "refraction must decrease with altitude ({} deg)",
                alt,
            );
            alt += 1.0;
        }
    }

    #[test]
    fn zenith_and_edge_cases() {
        // At the zenith refraction is ~0 (within 0.1")
        let zenith = correct_atmospheric_refraction(degree_to_radian(90.0));
        assert!(f64::abs(zenith - degree_to_radian(90.0)) < degree_to_radian(0.1 / 3600.0));

        // Below -1 deg the formula is unreliable: no correction
        let below = degree_to_radian(-5.0);
        assert_eq!(correct_atmospheric_refraction(below), below);

        // Above 90 deg is clamped to the zenith
        let over = correct_atmospheric_refraction(degree_to_radian(95.0));
        assert!(f64::abs(over - degree_to_radian(90.0)) < degree_to_radian(0.1 / 3600.0));

        // NaN passes through unchanged
        assert!(correct_atmospheric_refraction(f64::NAN).is_nan());
    }

    /// Prints the apparent -> true altitude shift table (run with --nocapture)
    #[test]
    fn print_refraction_table() {
        println!("apparent alt          R          true alt");
        for alt in [0.0, 1.0, 3.0, 5.0, 10.0, 20.0, 30.0, 45.0, 60.0, 75.0, 90.0] {
            let apparent = degree_to_radian(alt);
            let true_alt = correct_atmospheric_refraction(apparent);
            let r_arcmin = radian_to_degree(apparent - true_alt) * 60.0;
            println!(
                "{alt:>8.1} deg    {r_arcmin:>7.2}'    {:.4} deg",
                radian_to_degree(true_alt),
            );
        }
    }
}
