use std::f64::consts::PI;
use chrono::{Datelike, Timelike, NaiveDateTime, NaiveDate};
use gtk::prelude::*;
use crate::{
    ui::gtk_utils::{self, DEFAULT_DPMM},
    utils::math::{linear_interpolate, linear_solve2}
};
use super::{consts::*, data::*, painter::*};

#[derive(Debug, Clone, Copy)]
pub struct EqCoord {
    pub dec: f64,
    pub ra:  f64,
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
}

#[derive(Debug, Clone)]
pub struct HorizCoord {
    pub alt: f64,
    pub az:  f64,
}

impl HorizCoord {
    pub fn to_sphere_pt(&self) -> Point3D {
        let y = f64::sin(self.alt);
        let r_xz = f64::cos(self.alt);
        let x = r_xz * f64::sin(self.az);
        let z = r_xz * f64::cos(self.az);
        Point3D { x, y, z }
    }
}

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
}

//   |y
//   |  /z
//   | /
//   |/      x
//   *--------
/// Point on sphere with r = 1
#[derive(Debug)]
pub struct Point3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Point3D {
    pub fn to_horiz_crd(&self) -> HorizCoord {
        HorizCoord {
            az: f64::atan2(self.x, self.z),
            alt: f64::asin(self.y),
        }
    }

    pub fn rotate_over_x(&mut self, mat: &RotMatrix) {
        let z = mat.cos * self.z - mat.sin * self.y;
        let y = mat.sin * self.z + mat.cos * self.y;
        self.z = z;
        self.y = y;
    }

    pub fn rotate_over_y(&mut self, mat: &RotMatrix) {
        let x = mat.cos * self.x - mat.sin * self.z;
        let z = mat.sin * self.x + mat.cos * self.z;
        self.x = x;
        self.z = z;
    }

}

#[test]
fn test_point3d_rotate() {
    let mut pt = Point3D { x: 0.0, y: 0.0, z: 1.0 };

    let mat = RotMatrix::new(degree_to_radian(90.0));
    pt.rotate_over_x(&mat);
    assert!(f64::abs(pt.x-0.0) < 1e-10);
    assert!(f64::abs(pt.y-1.0) < 1e-10);
    assert!(f64::abs(pt.z-0.0) < 1e-10);
}

pub struct EqToHorizCvt {
    lst:     f64, // local sidereal time
    sin_lat: f64, // sin(observer.latitude)
    cos_lat: f64, // cos(observer.latitude)
}

impl EqToHorizCvt {
    pub fn new(observer: &Observer, utc_time: &NaiveDateTime,) -> Self {
        Self {
            lst:     calc_sidereal_time(utc_time) + observer.longitude,
            sin_lat: f64::sin(observer.latitude),
            cos_lat: f64::cos(observer.latitude),
        }
    }

    pub fn eq_to_horiz(&self, eq: &EqCoord) -> HorizCoord {
        let h = self.lst - eq.ra;
        let cos_h = f64::cos(h);
        let az = f64::atan2(
            f64::sin(h),
            cos_h * self.sin_lat - f64::tan(eq.dec) * self.cos_lat
        );
        let alt = f64::asin(
            self.sin_lat * f64::sin(eq.dec) + self.cos_lat * f64::cos(eq.dec) * cos_h
        );
        HorizCoord { alt, az }
    }

    pub fn horiz_to_eq(&self, horiz: &HorizCoord) -> EqCoord {
        let cos_az = f64::cos(horiz.az);
        let h = f64::atan2(
            f64::sin(horiz.az),
            cos_az * self.sin_lat + f64::tan(horiz.alt) * self.cos_lat
        );
        let dec = f64::asin(
            self.sin_lat * f64::sin(horiz.alt) -
            self.cos_lat * f64::cos(horiz.alt) * cos_az
        );
        let mut ra = self.lst - h;
        while ra < 0.0 { ra += 2.0 * PI; }
        while ra >= 2.0 * PI { ra -= 2.0 * PI; }
        EqCoord { ra, dec }
    }
}

#[test]
fn test_eq_to_horiz_cvt() {
    fn test(eq: &EqCoord) {
        let date = chrono::NaiveDate::from_ymd_opt(2015, 9, 5).unwrap();
        let time = chrono::NaiveTime::from_hms_milli_opt(11, 23, 15, 0).unwrap();
        let utc_time = chrono::NaiveDateTime::new(date, time);
        let observer = Observer {
            latitude:  degree_to_radian(11.0),
            longitude: degree_to_radian(56.0),
        };
        let cvt = EqToHorizCvt::new(&observer, &utc_time);
        let horiz = cvt.eq_to_horiz(eq);
        let new_eq = cvt.horiz_to_eq(&horiz);
        assert!(f64::abs(eq.ra - new_eq.ra) < 1e-10);
    }

    test(&EqCoord {
        ra: degree_to_radian(12.0),
        dec: degree_to_radian(42.0),
    });

    test(&EqCoord {
        ra: degree_to_radian(200.0),
        dec: degree_to_radian(42.0),
    });

    test(&EqCoord {
        ra: degree_to_radian(200.0),
        dec: degree_to_radian(-42.0),
    });

    test(&EqCoord {
        ra: degree_to_radian(12.0),
        dec: degree_to_radian(-42.0),
    });

}

pub struct Rect {
    pub left:   f64,
    pub top:    f64,
    pub right:  f64,
    pub bottom: f64,
}

pub struct ScreenInfo {
    pub rect:      Rect,
    pub tolerance: Rect,
    pub main_size: f64,
    pub center_x:  f64,
    pub center_y:  f64,
    pub dpmm_x:    f64,
    pub dpmm_y:    f64,
}

impl ScreenInfo {
    pub fn new(da: &gtk::DrawingArea) -> Self {
        let da_size = da.allocation();
        let width = da_size.width() as f64;
        let height = da_size.height() as f64;
        let main_size = 0.5 * f64::max(width, height);

        let (dpmm_x, dpmm_y) = gtk_utils::get_widget_dpmm(da)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));

        let tolerance = Rect {
            left: -20.0 * dpmm_x,
            top: -20.0 * dpmm_y,
            right: width + 20.0 * dpmm_x,
            bottom: height + 20.0 * dpmm_y,
        };
        let rect = Rect {
            left: 0.0,
            top: 0.0,
            right: width,
            bottom: height,
        };
        Self {
            rect,
            tolerance,
            main_size,
            center_x: 0.5 * width,
            center_y: 0.5 * height,
            dpmm_x,
            dpmm_y,
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

impl Point2D {
    pub fn distance(pt1: &Point2D, pt2: &Point2D) -> f64 {
        let diff_x = pt1.x - pt2.x;
        let diff_y = pt1.y - pt2.y;
        f64::sqrt(diff_x * diff_x + diff_y * diff_y)
    }
}

pub struct HorizToScreenCvt<'a> {
    vp:           &'a ViewPoint,
    vp_alt_mat:   RotMatrix,
    vp_n_alt_mat: RotMatrix,
}

impl<'a> HorizToScreenCvt<'a> {
    pub fn new(vp: &'a ViewPoint) -> Self {
        Self {
            vp,
            vp_alt_mat:   RotMatrix::new(vp.crd.alt),
            vp_n_alt_mat: RotMatrix::new(-vp.crd.alt),
        }
    }

    pub fn horiz_to_sphere(&self, pt: &HorizCoord) -> Point3D {
        let pt_with_vp_az = HorizCoord { alt: pt.alt, az: pt.az - self.vp.crd.az };
        let mut result = pt_with_vp_az.to_sphere_pt();
        result.rotate_over_x(&self.vp_n_alt_mat);
        result
    }

    pub fn sphere_to_screen(&self, pt: &Point3D, screen: &ScreenInfo) -> Point2D {
        let mul = self.vp.mag_factor * screen.main_size / (pt.z + 1.0);
        let x = mul * pt.x + screen.center_x;
        let y = -mul * pt.y + screen.center_y;
        Point2D { x, y }
    }

    pub fn screen_to_horiz(
        &self,
        pt:     &Point2D,
        screen: &ScreenInfo
    ) -> Option<HorizCoord> {
        let div = self.vp.mag_factor * screen.main_size;
        let x = (pt.x - screen.center_x) / div;
        let y = (-pt.y + screen.center_y) / div;
        let (cross_crd1, cross_crd2) = Self::calc_sphere_and_line_cross(
            x, x,
            y, y,
            1.0, 0.0
        )?;
        let mut crd = if cross_crd1.z > cross_crd2.z {
            cross_crd1
        } else {
            cross_crd2
        };
        if crd.z < 0.0 {
            return None;
        }
        crd.rotate_over_x(&self.vp_alt_mat);
        let mut az_alt_rot_crd = crd.to_horiz_crd();
        az_alt_rot_crd.az += self.vp.crd.az;

        Some(az_alt_rot_crd)
    }

    fn calc_sphere_and_line_cross(
        ax: f64, bx: f64,
        ay: f64, by: f64,
        az: f64, bz: f64,
    ) -> Option<(Point3D, Point3D)> {
        let a = ax*ax + ay*ay + az*az;
        let b = 2.0 * (ax*bx + ay*by + az*bz);
        let c = bx*bx + by*by + bz*bz - 1.0;
        let d = b*b - 4.0 * a * c;
        if d < 0.0 {
            return None;
        }
        let t1 = (-b + d.sqrt()) / (2.0 * a);
        let t2 = (-b - d.sqrt()) / (2.0 * a);
        let crd1 = Point3D {
            x: ax * t1 + bx,
            y: ay * t1 + by,
            z: az * t1 + bz,
        };
        let crd2 = Point3D {
            x: ax * t2 + bx,
            y: ay * t2 + by,
            z: az * t2 + bz,
        };
        Some((crd1, crd2))
    }
}

pub struct Line2D {
    pub crd1: Point2D,
    pub crd2: Point2D,
}

impl Line2D {
    pub fn intersection(line1: &Line2D, line2: &Line2D) -> Option<Point2D> {
        let ax1 = line1.crd1.x;
        let bx1 = line1.crd2.x - line1.crd1.x;
        let ay1 = line1.crd1.y;
        let by1 = line1.crd2.y - line1.crd1.y;
        let ax2 = line2.crd1.x;
        let bx2 = line2.crd2.x - line2.crd1.x;
        let ay2 = line2.crd1.y;
        let by2 = line2.crd2.y - line2.crd1.y;

        let (t1, t2) = linear_solve2(
            bx1, -bx2, ax2 - ax1,
            by1, -by2, ay2 - ay1
        )?;

        if t1 < 0.0 || t1 > 1.0 || t2 < 0.0 || t2 > 1.0 {
            return None;
        }

        Some(Point2D {
            x: ax1 + bx1 * t1,
            y: ay1 + by1 * t1,
        })
    }
}

#[test]
fn test_2d_lines_intersection() {
    assert_eq!(
        Line2D::intersection(
            &Line2D {
                crd1: Point2D { x: -1.0, y: 0.0 },
                crd2: Point2D { x:  1.0, y: 0.0 },
            },
            &Line2D {
                crd1: Point2D { x: 0.0, y: -1.0 },
                crd2: Point2D { x: 0.0, y:  1.0 },
            }
        ),
        Some(Point2D{x: 0.0, y: 0.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                crd1: Point2D { x: -1.0, y:  8.0 },
                crd2: Point2D { x:  4.0, y: -2.0 },
            },
            &Line2D {
                crd1: Point2D { x:  4.0, y:  5.0 },
                crd2: Point2D { x: -2.0, y: -4.0 },
            }
        ),
        Some(Point2D{x: 2.0, y: 2.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                crd1: Point2D { x: 0.0, y: 8.0 },
                crd2: Point2D { x: 2.0, y: 3.0 },
            },
            &Line2D {
                crd1: Point2D { x: 3.0, y: 1.0 },
                crd2: Point2D { x: 2.0, y: 3.0 },
            }
        ),
        Some(Point2D{x: 2.0, y: 3.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                crd1: Point2D { x: 0.0, y: 8.0 },
                crd2: Point2D { x: 2.0, y: 3.0 },
            },
            &Line2D {
                crd1: Point2D { x: 3.0, y: 1.0 },
                crd2: Point2D { x: 1.0, y: 5.0 },
            }
        ),
        Some(Point2D{x: 2.0, y: 3.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                crd1: Point2D { x: 0.0, y: 8.0 },
                crd2: Point2D { x: 2.0, y: 3.0 },
            },
            &Line2D {
                crd1: Point2D { x: 3.0, y: 1.0 },
                crd2: Point2D { x: 2.0, y: 5.0 },
            }
        ),
        None
    );
}

pub fn calc_max_star_magnitude_for_painting(mag_factor: f64) -> f32 {
    linear_interpolate(
        mag_factor.log10(),
        MIN_MAG_FACTOR.log10(),
        MAX_MAG_FACTOR.log10(),
        4.0,
        20.0,
    ) as f32
}

pub fn radian_to_degree(radian: f64) -> f64 {
    180.0 * radian / PI
}

pub fn degree_to_radian(degree: f64) -> f64 {
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
    let julain_day = calc_julian_day(&dt.date()) as f64;
    let hour = dt.hour() as f64;
    let min = dt.minute() as f64;
    let mut sec = dt.second() as f64;
    let msecs = (dt.nanosecond() / 1_000_000) as f64;
    sec += msecs / 1000.0;
    julain_day + (hour - 12.0) / 24.0 + min / 1440.0 + sec / 86400.0
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