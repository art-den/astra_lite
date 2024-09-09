use gtk::prelude::*;
use crate::{ui::gtk_utils::{self, DEFAULT_DPMM}, utils::math::linear_solve2};
use super::math::*;

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


pub fn sphere_to_screen(
    pt:         &Point3D,
    screen:     &ScreenInfo,
    mag_factor: f64
) -> Point2D {
    let pt = Point3D { x: pt.y, y: pt.x, z: pt.z };
    let mul = mag_factor * screen.main_size / (pt.z + 1.0);
    let x = mul * pt.x + screen.center_x;
    let y = -mul * pt.y + screen.center_y;
    Point2D { x, y }
}

pub fn screen_to_sphere(
    pt:         &Point2D,
    screen:     &ScreenInfo,
    mag_factor: f64
) -> Option<Point3D> {
    let div = mag_factor * screen.main_size;
    let x = (pt.x - screen.center_x) / div;
    let y = (-pt.y + screen.center_y) / div;
    let (cross_crd1, cross_crd2) = calc_sphere_and_line_cross(
        x, x,
        y, y,
        1.0, 0.0
    )?;
    let crd = if cross_crd1.z > cross_crd2.z {
        cross_crd1
    } else {
        cross_crd2
    };
    if crd.z < 0.0 {
        return None;
    }

    Some(Point3D {x: crd.y, y: crd.x, z: crd.z})
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

pub struct Rect {
    pub left:   f64,
    pub top:    f64,
    pub right:  f64,
    pub bottom: f64,
}

impl Rect {
    pub fn top_line(&self) -> Line2D {
        Line2D {
            pt1: Point2D { x: self.left, y: self.top },
            pt2: Point2D { x: self.right, y: self.top }
        }
    }

    pub fn bottom_line(&self) -> Line2D {
        Line2D {
            pt1: Point2D { x: self.left, y: self.bottom },
            pt2: Point2D { x: self.right, y: self.bottom }
        }
    }

    pub fn left_line(&self) -> Line2D {
        Line2D {
            pt1: Point2D { x: self.left, y: self.top },
            pt2: Point2D { x: self.left, y: self.bottom }
        }
    }

    pub fn right_line(&self) -> Line2D {
        Line2D {
            pt1: Point2D { x: self.right, y: self.top },
            pt2: Point2D { x: self.right, y: self.bottom }
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

    pub fn rotate(&mut self, mat: &RotMatrix) {
        mat.rotate(&mut self.x, &mut self.y);
    }
}

pub struct Line2D {
    pub pt1: Point2D,
    pub pt2: Point2D,
}

impl Line2D {
    pub fn intersection(line1: &Line2D, line2: &Line2D) -> Option<Point2D> {
        let ax1 = line1.pt1.x;
        let bx1 = line1.pt2.x - line1.pt1.x;
        let ay1 = line1.pt1.y;
        let by1 = line1.pt2.y - line1.pt1.y;
        let ax2 = line2.pt1.x;
        let bx2 = line2.pt2.x - line2.pt1.x;
        let ay2 = line2.pt1.y;
        let by2 = line2.pt2.y - line2.pt1.y;

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
                pt1: Point2D { x: -1.0, y: 0.0 },
                pt2: Point2D { x:  1.0, y: 0.0 },
            },
            &Line2D {
                pt1: Point2D { x: 0.0, y: -1.0 },
                pt2: Point2D { x: 0.0, y:  1.0 },
            }
        ),
        Some(Point2D{x: 0.0, y: 0.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                pt1: Point2D { x: -1.0, y:  8.0 },
                pt2: Point2D { x:  4.0, y: -2.0 },
            },
            &Line2D {
                pt1: Point2D { x:  4.0, y:  5.0 },
                pt2: Point2D { x: -2.0, y: -4.0 },
            }
        ),
        Some(Point2D{x: 2.0, y: 2.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                pt1: Point2D { x: 0.0, y: 8.0 },
                pt2: Point2D { x: 2.0, y: 3.0 },
            },
            &Line2D {
                pt1: Point2D { x: 3.0, y: 1.0 },
                pt2: Point2D { x: 2.0, y: 3.0 },
            }
        ),
        Some(Point2D{x: 2.0, y: 3.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                pt1: Point2D { x: 0.0, y: 8.0 },
                pt2: Point2D { x: 2.0, y: 3.0 },
            },
            &Line2D {
                pt1: Point2D { x: 3.0, y: 1.0 },
                pt2: Point2D { x: 1.0, y: 5.0 },
            }
        ),
        Some(Point2D{x: 2.0, y: 3.0})
    );

    assert_eq!(
        Line2D::intersection(
            &Line2D {
                pt1: Point2D { x: 0.0, y: 8.0 },
                pt2: Point2D { x: 2.0, y: 3.0 },
            },
            &Line2D {
                pt1: Point2D { x: 3.0, y: 1.0 },
                pt2: Point2D { x: 2.0, y: 5.0 },
            }
        ),
        None
    );
}