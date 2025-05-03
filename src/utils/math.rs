#![allow(dead_code)]

use std::fmt::Debug;

use itertools::*;

#[inline(always)]
pub fn cmp_f64(v1: &f64, v2: &f64) -> core::cmp::Ordering {
    if      *v1 < *v2 { core::cmp::Ordering::Less }
    else if *v1 > *v2 { core::cmp::Ordering::Greater }
    else              { core::cmp::Ordering::Equal }
}

#[inline(always)]
pub fn median3<T: Ord + Copy>(a: T, b: T, c: T) -> T {
    T::max(T::min(a, b), T::min(c, T::max(a, b)))
}

#[test]
fn test_median3() {
    assert_eq!(median3(1, 2, 3), 2);
    assert_eq!(median3(2, 3, 1), 2);
    assert_eq!(median3(3, 1, 2), 2);
    assert_eq!(median3(1, 3, 2), 2);
    assert_eq!(median3(3, 2, 1), 2);
}

pub fn median4_u16(a: u16, b: u16, c: u16, d: u16) -> u16
{
    let f = u16::max(u16::min(a, b), u16::min(c, d));
    let g = u16::min(u16::max(a, b), u16::max(c, d));
    ((f as u32 + g as u32 + 1) / 2) as _
}

pub fn median4_i32(a: i32, b: i32, c: i32, d: i32) -> i32
{
    let f = i32::max(i32::min(a, b), i32::min(c, d));
    let g = i32::min(i32::max(a, b), i32::max(c, d));
    ((f as i64 + g as i64) / 2) as _
}

#[test]
fn test_median4() {
    for p in [1, 3, 5, 7].iter().permutations(4) {
        let m = median4_i32(*p[0], *p[1], *p[2], *p[3]);
        assert_eq!(m, 4);
    }
}

pub fn median5<T: core::cmp::Ord + Copy>(a: T, b: T, c: T, d: T, e: T) -> T {
    let f = T::max(T::min(a, b), T::min(c, d));
    let g = T::min(T::max(a, b), T::max(c, d));
    median3(e, f, g)
}

#[test]
fn test_median5() {
    for p in [1, 2, 3, 4, 5].iter().permutations(5) {
        let m = median5(*p[0], *p[1], *p[2], *p[3], *p[4]);
        assert_eq!(m, 3);
    }
}

pub fn median<T: core::cmp::Ord + Copy>(values: &mut [T]) -> T {
    let pos = values.len() / 2;
    *values.select_nth_unstable(pos).1
}

#[inline(always)]
pub fn linear_interpolate(x: f64, x1: f64, x2: f64, y1: f64, y2: f64) -> f64 {
    (x - x1) * (y2 - y1) / (x2 - x1) + y1
}

pub struct Mat2 {
    pub a11: f64, pub a12: f64,
    pub a21: f64, pub a22: f64,
}

impl Mat2 {
    pub fn new(a11: f64, a12: f64, a21: f64, a22: f64) -> Self {
        Self { a11, a12, a21, a22 }
    }

    pub fn det(&self) -> f64 {
        self.a11 * self.a22 - self.a12 * self.a21
    }
}

pub struct Mat3 {
    pub a11: f64, pub a12: f64, pub a13: f64,
    pub a21: f64, pub a22: f64, pub a23: f64,
    pub a31: f64, pub a32: f64, pub a33: f64,
}

impl Debug for Mat3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("\n{:12.6e} {:12.6e} {:12.6e}", self.a11, self.a12, self.a13))?;
        f.write_fmt(format_args!("\n{:12.6e} {:12.6e} {:12.6e}", self.a21, self.a22, self.a23))?;
        f.write_fmt(format_args!("\n{:12.6e} {:12.6e} {:12.6e}\n", self.a31, self.a32, self.a33))?;
        std::fmt::Result::Ok(())
    }
}

impl Mat3 {
    pub fn new(
        a11: f64, a12: f64, a13: f64,
        a21: f64, a22: f64, a23: f64,
        a31: f64, a32: f64, a33: f64,
    ) -> Self {
        Self {
            a11, a12, a13,
            a21, a22, a23,
            a31, a32, a33,
        }
    }

    pub fn det(&self) -> f64 {
        self.a11 * Mat2::new(self.a22, self.a23, self.a32, self.a33).det() -
        self.a12 * Mat2::new(self.a21, self.a23, self.a31, self.a33).det() +
        self.a13 * Mat2::new(self.a21, self.a22, self.a31, self.a32).det()
    }

    pub fn inv(&self) -> Self {
        let adj11 =  Mat2::new(self.a22, self.a23, self.a32, self.a33).det();
        let adj21 = -Mat2::new(self.a21, self.a23, self.a31, self.a33).det();
        let adj31 =  Mat2::new(self.a21, self.a22, self.a31, self.a32).det();

        let adj12 = -Mat2::new(self.a12, self.a13, self.a32, self.a33).det();
        let adj22 =  Mat2::new(self.a11, self.a13, self.a31, self.a33).det();
        let adj32 = -Mat2::new(self.a11, self.a12, self.a31, self.a32).det();

        let adj13 =  Mat2::new(self.a12, self.a13, self.a22, self.a23).det();
        let adj23 = -Mat2::new(self.a11, self.a13, self.a21, self.a23).det();
        let adj33 =  Mat2::new(self.a11, self.a12, self.a21, self.a22).det();

        let det = self.det();

        Mat3 {
            a11: adj11/det, a12: adj12/det, a13: adj13/det,
            a21: adj21/det, a22: adj22/det, a23: adj23/det,
            a31: adj31/det, a32: adj32/det, a33: adj33/det
        }
    }
}

pub fn linear_solve2(
    a11: f64, a12: f64, b1: f64,
    a21: f64, a22: f64, b2: f64,
) -> Option<(f64, f64)> {
    let det = Mat2{
        a11, a12,
        a21, a22,
    }.det();

    if det == 0.0 {
        return None;
    }

    let det1 = Mat2::new(
        b1, a12,
        b2, a22,
    ).det();

    let det2 = Mat2::new(
        a11, b1,
        a21, b2,
    ).det();

    Some((det1/det, det2/det))
}

#[test]
fn test_linear_solve2() {
    let (x, y) = linear_solve2(
        3.0,  2.0, 16.0,
        2.0, -1.0,  6.0,
    ).unwrap();

    assert!(f64::abs(x - 4.0) < 0.01);
    assert!(f64::abs(y - 2.0) < 0.01);

    let (x, y) = linear_solve2(
        1.0, 2.0, 35.0,
        1.0, 1.0, 31.0
    ).unwrap();

    assert!(f64::abs(x - 27.0) < 0.01);
    assert!(f64::abs(y - 4.0) < 0.01);
}

fn linear_solve3(
    a11: f64, a12: f64, a13: f64, b1: f64,
    a21: f64, a22: f64, a23: f64, b2: f64,
    a31: f64, a32: f64, a33: f64, b3: f64
) -> Option<(f64, f64, f64)> {
    let det = Mat3::new(
        a11, a12, a13,
        a21, a22, a23,
        a31, a32, a33
    ).det();

    if det == 0.0 {
        return None;
    }

    let det1 = Mat3::new(
        b1, a12, a13,
        b2, a22, a23,
        b3, a32, a33
    ).det();

    let det2 = Mat3::new(
        a11, b1, a13,
        a21, b2, a23,
        a31, b3, a33
    ).det();

    let det3 = Mat3::new(
        a11, a12, b1,
        a21, a22, b2,
        a31, a32, b3
    ).det();

    Some((det1/det, det2/det, det3/det))
}

#[test]
fn test_linear_solve3() {
    let (a, b, c) = linear_solve3(
        2.0, 3.0, -1.0, 5.0,
        3.0, -1.0, 4.0, 13.0,
        5.0, -2.0, 2.0, 7.0,
    ).unwrap();

    assert!(f64::abs(a - 1.0) < 0.01);
    assert!(f64::abs(b - 2.0) < 0.01);
    assert!(f64::abs(c - 3.0) < 0.01);
}

#[derive(Clone, Debug)]
pub struct QuadraticCoeffs {
    pub a2: f64,
    pub a1: f64,
    pub a0: f64,
}

impl QuadraticCoeffs {
    pub fn calc(&self, x: f64) -> f64 {
        self.a2*x*x + self.a1*x + self.a0
    }
}

pub fn square_ls(x_values: &[f64], y_values: &[f64]) -> Option<QuadraticCoeffs> {
    assert!(x_values.len() == y_values.len());
    if x_values.len() < 3 { return None; }

    let mut sum_x = 0_f64;
    let mut sum_x2 = 0_f64;
    let mut sum_x3 = 0_f64;
    let mut sum_x4 = 0_f64;
    let mut sum_y = 0_f64;
    let mut sum_xy = 0_f64;
    let mut sum_x2y = 0_f64;

    for (&x, &y) in x_values.iter().zip(y_values) {
        let x2 = x * x;
        let x3 = x2 * x;
        let x4 = x3 * x;

        sum_x += x;
        sum_x2 += x2;
        sum_x3 += x3;
        sum_x4 += x4;
        sum_y += y;
        sum_xy += x * y;
        sum_x2y += x2 * y;
    }

    linear_solve3(
        x_values.len() as f64, sum_x,  sum_x2, sum_y,
        sum_x,                 sum_x2, sum_x3, sum_xy,
        sum_x2,                sum_x3, sum_x4, sum_x2y,
    ).map(|coeffs| {
        QuadraticCoeffs {a0: coeffs.0, a1: coeffs.1, a2: coeffs.2,}
    })
}

pub fn parabola_extremum(sc: &QuadraticCoeffs) -> Option<f64> {
    if sc.a2 != 0.0 {
        Some(-0.5 * sc.a1 / sc.a2)
    } else {
        None
    }
}

pub fn linear_regression(x: &[f64], y: &[f64]) -> Option<(f64, f64)> {
    if x.len() != y.len() || x.is_empty() {
        return None;
    }
    let n = x.len() as f64;
    let sum_x: f64 = x.iter().sum();
    let sum_y: f64 = y.iter().sum();
    let sum_xy: f64 = x.iter().zip(y.iter()).map(|(&xi, &yi)| xi * yi).sum();
    let sum_x_sq: f64 = x.iter().map(|&xi| xi * xi).sum();
    let denominator = n * sum_x_sq - sum_x * sum_x;
    if denominator == 0.0 {
        return None;
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denominator;
    let intercept = (sum_y - slope * sum_x) / n;
    Some((slope, intercept))
}


pub struct IirFilterCoeffs {
    a0: f32,
    b0: f32,
}

impl IirFilterCoeffs {
    pub fn new(b0: f32) -> IirFilterCoeffs {
        IirFilterCoeffs {
            a0: 1.0 - b0,
            b0,
        }
    }
}

pub struct IirFilter {
    y0: Option<f32>,
}

impl IirFilter {
    pub fn new() -> Self {
        Self {
            y0: None,
        }
    }

    fn set_first_time(&mut self) {
        self.y0 = None;
    }

    pub fn filter(&mut self, coeffs: &IirFilterCoeffs, x: u32) -> u32 {
        let x = x as f32;
        let result = coeffs.a0 * x + coeffs.b0 * self.y0.unwrap_or(x);
        self.y0 = Some(result);
        result as u32
    }

    #[inline(never)]
    pub fn filter_direct_and_revert_u16(&mut self, coeffs: &IirFilterCoeffs, src: &[u16], dst: &mut [u16]) {
        self.filter_direct_u16(coeffs, src, dst);
        self.filter_revert_u16(coeffs, src, dst);
    }

    #[inline(never)]
    fn filter_direct_u16(&mut self, coeffs: &IirFilterCoeffs, src: &[u16], dst: &mut [u16]) {
        self.set_first_time();
        for (s, d) in izip!(src, dst) {
            let mut res = self.filter(coeffs, *s as u32);
            if res > u16::MAX as u32 { res = u16::MAX as u32; }
            *d = res as u16;
        }
    }

    #[inline(never)]
    fn filter_revert_u16(&mut self, coeffs: &IirFilterCoeffs, src: &[u16], dst: &mut [u16]) {
        self.set_first_time();
        for (s, d) in izip!(src.iter().rev(), dst.iter_mut().rev()) {
            let mut res = (self.filter(coeffs, *s as u32) + *d as u32) / 2;
            if res > u16::MAX as u32 { res = u16::MAX as u32; }
            *d = res as u16;
        }
    }
}

pub struct Point3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

pub struct Line {
    a: f64,
    b: f64
}

impl Line {
    pub fn get(&self, x: f64) -> f64 {
        self.a * x + self.b
    }
}

pub struct Plane {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
}

impl Plane {
    pub fn intersect_by_xz_plane(&self, y: f64) -> Option<Line> {
        if self.c != 0.0 {
            let a = -self.a / self.c;
            let b = -(self.b * y + self.d) / self.c;
            Some(Line {a, b})
        } else {
            None
        }
    }

    pub fn calc_z(&self, x: f64, y: f64) -> f64 {
        (-self.a * x - self.b * y - self.d) / self.c
    }
}

pub fn calc_fitting_plane_z_dist(points: &[Point3D]) -> Option<Plane> {
    if points.len() < 3 { return None; }
    let x_sum = points.iter().map(|p| p.x).sum::<f64>();
    let y_sum = points.iter().map(|p| p.y).sum::<f64>();
    let z_sum = points.iter().map(|p| p.z).sum::<f64>();
    let x2_sum = points.iter().map(|p| p.x * p.x).sum::<f64>();
    let y2_sum = points.iter().map(|p| p.y * p.y).sum::<f64>();
    let xy_sum = points.iter().map(|p| p.x * p.y).sum::<f64>();
    let xz_sum = points.iter().map(|p| p.x * p.z).sum::<f64>();
    let yz_sum = points.iter().map(|p| p.y * p.z).sum::<f64>();
    let n = points.len() as f64;
    let Some((a, b, d)) = linear_solve3(
        x2_sum, xy_sum, x_sum, -xz_sum,
        xy_sum, y2_sum, y_sum, -yz_sum,
        x_sum,  y_sum,  n,     -z_sum,
    ) else { return None; };
    Some(Plane{a, b, c: 1.0, d})
}

#[test]
fn test_fitting_plane_z_dist() {
    let points = [
        Point3D { x: 1.0,  y: 2.0,  z: 3.0   },
        Point3D { x: 11.0, y: -3.0, z: 2.9 },
        Point3D { x: 6.0,  y: 5.0,  z: 3.1   },
    ];
    let plane = calc_fitting_plane_z_dist(&points).unwrap();

    for p in &points {
        let z = plane.calc_z(p.x, p.y);
        let f = plane.a * p.x + plane.b * p.y + plane.c * p.z + plane.d;

        assert!(f64::abs(f) < 0.001);
        assert!(f64::abs(z - p.z) < 0.001);

        let line = plane.intersect_by_xz_plane(p.y).unwrap();
        let z = line.get(p.x);
        assert!(f64::abs(z - p.z) < 0.001);
    }
}