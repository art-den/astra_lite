use itertools::*;

#[inline(always)]
pub fn cmp_f64(v1: &f64, v2: &f64) -> core::cmp::Ordering {
    if      *v1 < *v2 { core::cmp::Ordering::Less }
    else if *v1 > *v2 { core::cmp::Ordering::Greater }
    else              { core::cmp::Ordering::Equal }
}

#[inline(always)]
pub fn median3<T: PartialOrd>(a: T, b: T, c: T) -> T {
    if (a > b) ^ (a > c) {
        a
    } else if (a > b) ^ (b > c) {
        c
    } else {
        b
    }
}

#[test]
fn test_median3() {
    assert_eq!(median3(1, 2, 3), 2);
    assert_eq!(median3(2, 3, 1), 2);
    assert_eq!(median3(3, 1, 2), 2);
    assert_eq!(median3(1, 3, 2), 2);
    assert_eq!(median3(3, 2, 1), 2);
}

#[inline(always)]
pub fn linear_interpolate(x: f64, x1: f64, x2: f64, y1: f64, y2: f64) -> f64 {
    (x - x1) * (y2 - y1) / (x2 - x1) + y1
}

fn det2(
    a11: f64, a12: f64,
    a21: f64, a22: f64
) -> f64 {
    a11 * a22 - a12 * a21
}

fn det3(
    a11: f64, a12: f64, a13: f64,
    a21: f64, a22: f64, a23: f64,
    a31: f64, a32: f64, a33: f64
) -> f64 {
    a11 * det2(a22, a23, a32, a33) -
    a12 * det2(a21, a23, a31, a33) +
    a13 * det2(a21, a22, a31, a32)
}

pub fn linear_solve2(
    a11: f64, a12: f64, b1: f64,
    a21: f64, a22: f64, b2: f64,
) -> Option<(f64, f64)> {
    let det = det2(
        a11, a12,
        a21, a22,
    );

    if det == 0.0 {
        return None;
    }

    let det1 = det2(
        b1, a12,
        b2, a22,
    );

    let det2 = det2(
        a11, b1,
        a21, b2,
    );

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
    let det = det3(
        a11, a12, a13,
        a21, a22, a23,
        a31, a32, a33
    );

    if det == 0.0 {
        return None;
    }

    let det1 = det3(
        b1, a12, a13,
        b2, a22, a23,
        b3, a32, a33
    );

    let det2 = det3(
        a11, b1, a13,
        a21, b2, a23,
        a31, b3, a33
    );

    let det3 = det3(
        a11, a12, b1,
        a21, a22, b2,
        a31, a32, b3
    );

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
pub struct SquareCoeffs {
    pub a2: f64,
    pub a1: f64,
    pub a0: f64,
}

impl SquareCoeffs {
    pub fn calc(&self, x: f64) -> f64 {
        self.a2*x*x + self.a1*x + self.a0
    }
}

pub fn square_ls(x_values: &[f64], y_values: &[f64]) -> Option<SquareCoeffs> {
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
        SquareCoeffs {a0: coeffs.0, a1: coeffs.1, a2: coeffs.2,}
    })
}

pub fn parabola_extremum(sc: &SquareCoeffs) -> Option<f64> {
    if sc.a2 != 0.0 {
        Some(-0.5 * sc.a1 / sc.a2)
    } else {
        None
    }
}
pub struct IirFilterCoeffs {
    a0: u32,
    b0: u32,
}

impl IirFilterCoeffs {
    pub fn new(b0: u32) -> IirFilterCoeffs {
        IirFilterCoeffs {
            a0: 256 - b0,
            b0,
        }
    }
}

pub struct IirFilter {
    y0: Option<u32>,
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
        let result = (coeffs.a0 * x + coeffs.b0 * self.y0.unwrap_or(x) + (1 << 7)) >> 8;
        self.y0 = Some(result);
        result
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