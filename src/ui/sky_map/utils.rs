use crate::utils::math::linear_solve2;
use super::math::RotMatrix;

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