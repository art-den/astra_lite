#![allow(dead_code)]

use std::f64::consts::PI;
use super::math::*;

// t = (JD - 2451545) / 36525

pub fn mini_moon(t: f64) -> EqCoord {
    const P2:     f64 = 2.0 * PI;
    const ARC:    f64 = 206264.8062;
    const COSEPS: f64 = 0.91748;
    const SINEPS: f64 = 0.39778; // cos/sin(obliquity ecliptic)

    let l0 = f64::fract(0.606433 + 1336.855225 * t); // mean longitude Moon (in rev)
    let l = P2 * f64::fract(0.374897 + 1325.552410 * t); // mean anomaly of the Moon
    let ls = P2 * f64::fract(0.993133 + 99.997361 * t); // mean anomaly of the Sun
    let d = P2 * f64::fract(0.827361 + 1236.853086 * t); // diff. longitude Moon-Sun
    let f = P2 * f64::fract(0.259086 + 1342.227825 * t); // mean argument of latitude
    let dl =
         22640.0 * f64::sin(l)
        - 4586.0 * f64::sin(l - 2.0 * d)
        + 2370.0 * f64::sin(2.0 * d)
        + 769.0 * f64::sin(2.0 * l)
        - 668.0 * f64::sin(ls)
        - 412.0 * f64::sin(2.0 * f)
        - 212.0 * f64::sin(2.0 * l - 2.0 * d)
        - 206.0 * f64::sin(l + ls - 2.0 * d)
        + 192.0 * f64::sin(l + 2.0 * d)
        - 165.0 * f64::sin(ls - 2.0 * d)
        - 125.0 * f64::sin(d)
        - 110.0 * f64::sin(l + ls)
        + 148.0 * f64::sin(l - ls)
        - 55.0 * f64::sin(2.0 * f - 2.0 * d);

    let s = f + (dl + 412.0 * f64::sin(2.0 * f) + 541.0 * f64::sin(ls)) / ARC;
    let h = f - 2.0 * d;
    let n =
        -526.0 * f64::sin(h)
        + 44.0 * f64::sin(l + h)
        - 31.0 * f64::sin(-l + h)
        - 23.0 * f64::sin(ls + h)
        + 11.0 * f64::sin(-ls + h)
        - 25.0 * f64::sin(-2.0 * l + f)
        + 21.0 * f64::sin(-l + f);

    let l_moon = P2 * f64::fract(l0 + dl / 1296E3); // in rad
    let b_moon = (18520.0 * f64::sin(s) + n) / ARC; // in rad

    let cb = f64::cos(b_moon);
    let x = cb * f64::cos(l_moon);
    let v = cb * f64::sin(l_moon);
    let w = f64::sin(b_moon);
    let y = COSEPS * v - SINEPS * w;
    let z = SINEPS * v + COSEPS * w;
    let rho = f64::sqrt(1.0 - z * z);
    let dec = f64::atan2(z, rho);
    let mut ra = 2.0 * f64::atan2(y, x + rho);
    if ra < 0.0 {
        ra += 2.0 * PI;
    }

    EqCoord { ra, dec }
}


// from https://celestialprogramming.com/meeus-illuminated_fraction_of_the_moon.html

pub fn moon_phase(t: f64) -> f64 {
    const TO_RAD: f64 = PI/180.0;

    let constrain = |mut angle| {
        angle /= 360.0;
        angle = f64::fract(angle);
        angle *= 360.0;
        if angle < 0.0 { angle += 360.0; }
        angle
    };

    let d = constrain(
        297.8501921
        + 445267.1114034*t
        - 0.0018819*t*t
        + 1.0/545868.0*t*t*t
        - 1.0/113065000.0*t*t*t*t
    )*TO_RAD;

    let m = constrain(
        357.5291092
        + 35999.0502909*t
        - 0.0001536*t*t
        + 1.0/24490000.0*t*t*t
    )*TO_RAD;

    let mp = constrain(
        134.9633964
        + 477198.8675055*t
        + 0.0087414*t*t
        + 1.0/69699.0*t*t*t
        - 1.0/14712000.0*t*t*t*t
    )*TO_RAD;

    let i = constrain(
        180.0 - d*180.0/PI
        - 6.289 * f64::sin(mp)
        + 2.1 * f64::sin(m)
        -1.274 * f64::sin(2.0*d - mp)
        -0.658 * f64::sin(2.0*d)
        -0.214 * f64::sin(2.0*mp)
        -0.11 * f64::sin(d)
    )*TO_RAD;

    (1.0 + f64::cos(i)) / 2.0
}

pub fn mini_sun(t: f64) -> EqCoord {
    const P2: f64 = 2.0 * PI;
    const COSEPS: f64 = 0.91748;
    const SINEPS: f64 = 0.39778;

    let m = P2 * f64::fract(0.993133 + 99.997361 * t);
    let dl = 6893.0 * f64::sin(m) + 72.0 * f64::sin(2.0 * m);
    let l = P2 * f64::fract(0.7859453 + m / P2 + (6191.2 * t + dl) / 1296E3);
    let sl = f64::sin(l);
    let x = f64::cos(l);
    let y = COSEPS * sl;
    let z = SINEPS * sl;
    let rho = f64::sqrt(1.0 - z * z);
    let dec = f64::atan2(z, rho);
    let mut ra = 2.0 * f64::atan2(y, x + rho);
    if ra < 0.0 {
        ra += 2.0 * PI;
    }

    EqCoord { ra, dec }
}

fn cos360(x: f64) -> f64 {
    const RAD: f64 = 0.0174532925199433;
     f64::cos(x * RAD)
}

fn sin360(x: f64) -> f64 {
    const RAD: f64 = 0.0174532925199433;
    f64::sin(x * RAD)
}

fn prec_mat_equ(t1: f64, t2: f64) -> Matrix33 {
    const SEC: f64 = 3600.0;
    let dt = t2 - t1;
    let zeta = ((2306.2181 + (1.39656 - 0.000139 * t1) * t1) +
        ((0.30188 - 0.000345 * t1) + 0.017998 * dt) * dt) * dt / SEC;
    let z = zeta + ((0.79280 + 0.000411 * t1) + 0.000205 * dt) * dt * dt / SEC;
    let theta = ((2004.3109 - (0.85330 + 0.000217 * t1) * t1) -
        ((0.42665 + 0.000217 * t1) + 0.041833 * dt) * dt) * dt / SEC;

    let c1 = cos360(z);
    let c2 = cos360(theta);
    let c3 = cos360(zeta);
    let s1 = sin360(z);
    let s2 = sin360(theta);
    let s3 = sin360(zeta);

    Matrix33 {
        a11: -s1 * s3 + c1 * c2 * c3,
        a12: -s1 * c3 - c1 * c2 * s3,
        a13: -c1 * s2,
        a21:  c1 * s3 + s1 * c2 * c3,
        a22:  c1 * c3 - s1 * c2 * s3,
        a23: -s1 * s2,
        a31:  s2 * c3,
        a32: -s2 * s3,
        a33:  c2,
    }
}

fn nut_equ(t: f64, x: &mut f64, y: &mut f64, z: &mut f64) {
    const ARC: f64 = 3600.0*180.0/PI;
    const P2: f64 = 2.0 * PI;
    let ls = P2 * f64::fract(0.993133 + 99.997306 * t); // mean anomaly Sun
    let d = P2 * f64::fract(0.827362 + 1236.853087 * t); // diff. longitude Moon-Sun
    let f = P2 * f64::fract(0.259089 + 1342.227826 * t); // mean argument of latitude
    let n = P2 * f64::fract(0.347346 - 5.372447 * t); // longit. ascending node
    let eps = 0.4090928 - 2.2696E-4 * t; // obliquity of the ecliptic
    let dpsi = (
        -17.200 * f64::sin(n)
        - 1.319 * f64::sin(2.0 * (f - d + n))
        - 0.227 * f64::sin(2.0 * (f + n))
        + 0.206 * f64::sin(2.0 * n)
        + 0.143 * f64::sin(ls)
    ) / ARC;
    let deps = (
        9.203 * f64::cos(n)
        + 0.574 * f64::cos(2.0 * (f - d + n))
        + 0.098 * f64::cos(2.0 * (f + n))
        - 0.090 * f64::cos(2.0 * n)
    ) / ARC;
    let c = dpsi * f64::cos(eps);
    let s = dpsi * f64::sin(eps);
    let dx = -(c * *y + s * *z);
    let dy = c * *x - deps * *z;
    let dz = s * *x + deps * *y;
    *x += dx;
    *y += dy;
    *z += dz;
}

pub fn pn_matrix(t0: f64, t: f64) -> Matrix33 {
    let mut m = prec_mat_equ(t0, t);
    nut_equ(t, &mut m.a11, &mut m.a21, &mut m.a31); // transform column vectors of
    nut_equ(t, &mut m.a12, &mut m.a22, &mut m.a32); // matrix A from mean equinox T
    nut_equ(t, &mut m.a13, &mut m.a23, &mut m.a33); // to true equinox T
    m
}

pub fn calc_atmospheric_refraction(alt: f64) -> f64 {
    const MIN_ALT: f64 = 3.0; // in degrees
    let c = 0.5 * PI - f64::max(alt, MIN_ALT * PI / 180.0);
    const A: f64 = degree_to_radian(57.085 / (60.0 * 60.0));
    const B: f64 = degree_to_radian(0.0666 / (60.0 * 60.0));
    A * f64::tan(c) - B * f64::tan(c)
}
