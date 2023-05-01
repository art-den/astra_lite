use itertools::*;

pub fn cmp_f64(v1: &f64, v2: &f64) -> core::cmp::Ordering {
    if      *v1 < *v2 { core::cmp::Ordering::Less }
    else if *v1 > *v2 { core::cmp::Ordering::Greater }
    else              { core::cmp::Ordering::Equal }
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
