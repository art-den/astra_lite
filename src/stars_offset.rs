use std::f64::consts::PI;
use itertools::Itertools;

pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    fn dist_to(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        f64::sqrt(dx*dx + dy*dy)
    }
}

#[derive(Debug, Clone)]
pub struct Offset {
    pub x:     f64,
    pub y:     f64,
    pub angle: f64,
}

impl Offset {
    pub fn calculate(
        ref_points:   &[Point],
        points:       &[Point],
        image_width:  f64,
        image_height: f64
    ) -> Option<Self> {
        for (max_points_cnt, max_err, triangulate) in [
            (50,  3.0, false),
            (70,  2.0, false),
            (100, 2.0, false),
        ] {
            let result = try_calculate(
                ref_points,
                points,
                image_width,
                image_height,
                max_points_cnt,
                max_err,
                triangulate
            );
            if result.is_some() {
                return result;
            }
        }
        None
    }
}

fn try_calculate(
    ref_points:     &[Point],
    points:         &[Point],
    image_width:    f64,
    image_height:   f64,
    max_points_cnt: usize,
    max_err:        f64,
    triangulate:    bool,
) -> Option<Offset> {
    const MAX_SIMILAR_TRIANGLES_CNT: usize = 10;

    let min_triangle_len = (image_width + image_height) / 10.0;

    // Generate triangles
    let ref_triangles = generate_triangles(ref_points, max_points_cnt, min_triangle_len, false, triangulate);
    let triangles = generate_triangles(points, max_points_cnt, min_triangle_len, true, triangulate);

    // Search similar trinagles
    let max_err2 = max_err*max_err;
    let mut similar_triangles = Vec::new();
    for ref_triangle in &ref_triangles {
        let lower_res = triangles.binary_search_by(|t| cmp_f64(&t.len, &(ref_triangle.len-max_err)));
        let upper_res = triangles.binary_search_by(|t| cmp_f64(&t.len, &(ref_triangle.len+max_err)));
        let lower_index = match lower_res { Ok(v) => v, Err(v) => v };
        let upper_index = match upper_res { Ok(v) => v, Err(v) => v };
        for triangle in &triangles[lower_index..upper_index] {
            if ref_triangle.edge_len_err(triangle) < max_err2 {
                similar_triangles.push((
                    ref_triangle.angle_between(triangle),
                    ref_triangle,
                    triangle,
                    0f64, // x offset
                    0f64, // y offset
                ));
            }
        }
    }
    if similar_triangles.len() < MAX_SIMILAR_TRIANGLES_CNT {
        return None;
    }

    let mut angle_values = Vec::new();
    let mut angle_diffs = Vec::new();
    let mut x_values = Vec::new();
    let mut y_values = Vec::new();
    for _iteration in 0..10 {
        let start_iteration_count = similar_triangles.len();

        // find largest cluster of angles and average angle of cluster
        angle_values.clear();
        for (angle, ..) in &similar_triangles { angle_values.push(*angle); }
        angle_values.sort_by(cmp_f64);
        angle_diffs.clear();
        for (a1, a2) in angle_values.iter().tuple_windows() {
            angle_diffs.push(a2-a1);
        }
        let angle_diffs_pos = angle_diffs.len() / 4;
        let min_angle_diff = *angle_diffs.select_nth_unstable_by(angle_diffs_pos, cmp_f64).1;
        struct Cluster {
            start_index: usize,
            end_index: usize,
        }
        let mut clusters = Vec::new();
        let mut start_index = 0_usize;
        for (i, (a1, a2)) in angle_values.iter().tuple_windows().enumerate() {
            let angle_diff = a2 - a1;
            if angle_diff > min_angle_diff {
                if i != start_index {
                    clusters.push( Cluster { start_index, end_index: i } );
                }
                start_index = i+1;
            }
        }
        if start_index != angle_values.len()-1 {
            clusters.push(Cluster {
                start_index,
                end_index: angle_values.len()-1
            });
        }
        let (start_index, end_index) = if !clusters.is_empty() {
            let largest_cluster = clusters.iter().max_by_key(|cl| cl.end_index - cl.start_index).unwrap();
            (largest_cluster.start_index, largest_cluster.end_index)
        } else {
            (0, angle_values.len()-1)
        };
        let for_angle_values = &angle_values[start_index..=end_index];
        let angle = for_angle_values.iter().sum::<f64>() / for_angle_values.len() as f64;

        // filter similar_triangles by angle
        const ANGLE_ERR: f64 = 1.0 * PI / 180.0;
        let min_angle = angle - ANGLE_ERR;
        let max_angle = angle + ANGLE_ERR;
        similar_triangles.retain(|(angle, ..)| *angle > min_angle && *angle < max_angle);
        if similar_triangles.len() < MAX_SIMILAR_TRIANGLES_CNT {
            return None;
        }

        // Caluclate x and y offset for similar_triangles and median values
        let center_x = (image_width - 1.0) / 2.0;
        let center_y = (image_height - 1.0) / 2.0;
        x_values.clear();
        y_values.clear();
        for (_, ref_tr, tr, x_offs, y_offs) in &mut similar_triangles {
            let ref_tr_center = ref_tr.center();
            let tr_center = tr.center();
            let tr_rotated = rotate_point(tr_center.x, tr_center.y, center_x, center_y, -angle);
            *x_offs = tr_rotated.x - ref_tr_center.x;
            *y_offs = tr_rotated.y - ref_tr_center.y;
            x_values.push(*x_offs);
            y_values.push(*y_offs);
        }
        let x_median_pos = x_values.len() / 2;
        let x_median = *x_values.select_nth_unstable_by(x_median_pos, cmp_f64).1;
        let y_median_pos = x_values.len() / 2;
        let y_median = *y_values.select_nth_unstable_by(y_median_pos, cmp_f64).1;

        // filter similar_triangles by median x and y offset
        let min_x_offs = x_median - max_err/2.0;
        let max_x_offs = x_median + max_err/2.0;
        let min_y_offs = y_median - max_err/2.0;
        let max_y_offs = y_median + max_err/2.0;
        similar_triangles.retain(|&(_, _, _, x_offs, y_offs)| {
            x_offs > min_x_offs && x_offs < max_x_offs &&
            y_offs > min_y_offs && y_offs < max_y_offs
        });
        if similar_triangles.len() < MAX_SIMILAR_TRIANGLES_CNT {
            return None;
        }

        // Exit from iteration if no changes
        if start_iteration_count == similar_triangles.len() {
            break;
        }
    }
    let count = similar_triangles.len() as f64;
    let aver_angle = similar_triangles.iter().map(|(angle, ..)| *angle).sum::<f64>() / count;
    let aver_x_offs = similar_triangles.iter().map(|(_, _, _, x_offs, _)| *x_offs).sum::<f64>() / count;
    let aver_y_offs = similar_triangles.iter().map(|(_, _, _, _, y_offs)| *y_offs).sum::<f64>() / count;

    Some(Offset {
        angle: aver_angle,
        x: aver_x_offs,
        y: aver_y_offs,
    })
}

struct Triangle<'a> {
    points:    [&'a Point; 3],
    edge_lens: [f64; 3],
    len:       f64,
}

impl<'a> Triangle<'a> {
    fn edge_len_err(&self, other: &Triangle) -> f64 {
        let diff1 = self.edge_lens[0] - other.edge_lens[0];
        let diff2 = self.edge_lens[1] - other.edge_lens[1];
        let diff3 = self.edge_lens[2] - other.edge_lens[2];
        diff1 * diff1 + diff2 * diff2 * diff3 * diff3
    }

    fn angle_between(&self, other: &Triangle) -> f64 {
        let calc_angle = |idx1: usize, idx2:usize| -> f64 {
            let self_angle = correct_angle(f64::atan2(
                self.points[idx2].y - self.points[idx1].y,
                self.points[idx2].x - self.points[idx1].x
            ));
            let other_angle = correct_angle(f64::atan2(
                other.points[idx2].y - other.points[idx1].y,
                other.points[idx2].x - other.points[idx1].x
            ));
            correct_angle(other_angle - self_angle)
        };
        let angle1 = calc_angle(0, 1);
        let angle2 = calc_angle(1, 2);
        let angle3 = calc_angle(2, 0);
        (angle1 + angle2 + angle3) / 3.0
    }

    fn center(&self) -> Point {
        Point {
            x: (self.points[0].x + self.points[1].x + self.points[2].x) / 3.0,
            y: (self.points[0].y + self.points[1].y + self.points[2].y) / 3.0,
        }
    }
}

fn generate_triangles(
    points:           &[Point],
    max_points_cnt:   usize,
    min_triangle_len: f64,
    also_rotated:     bool,
    _triangulate:     bool // TODO: add trinagulation!!!
) -> Vec<Triangle> {
    fn add_triangles<'a>(
        result:       &mut Vec<Triangle<'a>>,
        p1:           &'a Point,
        p2:           &'a Point,
        p3:           &'a Point,
        min_len:      f64,
        also_rotated: bool,
    ) {
        let len1 = p1.dist_to(p2);
        let len2 = p2.dist_to(p3);
        let len3 = p3.dist_to(p1);
        let total_len = len1 + len2 + len3;
        if total_len < min_len {
            return;
        }
        let c_x = (p1.x + p2.x + p3.x) / 3.0;
        let c_y = (p1.y + p2.y + p3.y) / 3.0;
        let mut points = [
            (f64::atan2(p1.y - c_y, p1.x - c_x), p1, len1),
            (f64::atan2(p2.y - c_y, p2.x - c_x), p2, len2),
            (f64::atan2(p3.y - c_y, p3.x - c_x), p3, len3),
        ];
        points.sort_by(|(a1, ..), (a2, ..)| cmp_f64(a1, a2));
        result.push(Triangle {
            points:    [p1, p2, p3],
            edge_lens: [len1, len2, len3],
            len: total_len,
        });
        if also_rotated {
            result.push(Triangle {
                points:    [p2, p3, p1],
                edge_lens: [len2, len3, len1],
                len: total_len,
            });
            result.push(Triangle {
                points:    [p3, p1, p2],
                edge_lens: [len3, len1, len2],
                len: total_len,
            });
        }
    }
    let max_points = points.len().min(max_points_cnt);
    let mut result = Vec::new();
    for i in 0..max_points {
        for j in i+1..max_points {
            for k in j+1..max_points {
                add_triangles(
                    &mut result,
                    &points[i],
                    &points[j],
                    &points[k],
                    min_triangle_len,
                    also_rotated
                );
            }
        }
    }
    result.sort_by(|t1, t2| { cmp_f64(&t1.len, &t2.len) });
    result
}

fn correct_angle(angle: f64) -> f64 {
    if      angle < -PI { angle + 2.0 * PI }
    else if angle > PI  { angle - 2.0 * PI }
    else                { angle }
}

fn cmp_f64(v1: &f64, v2: &f64) -> core::cmp::Ordering {
    if      *v1 < *v2 { core::cmp::Ordering::Less }
    else if *v1 > *v2 { core::cmp::Ordering::Greater }
    else              { core::cmp::Ordering::Equal }
}

fn rotate_point(x: f64, y: f64, x0: f64, y0: f64, angle: f64) -> Point {
    let dx = x - x0;
    let dy = y - y0;
    let cos_a = f64::cos(angle);
    let sin_a = f64::sin(angle);
    Point {
        x: x0 + dx * cos_a - dy * sin_a,
        y: y0 + dy * cos_a + dx * sin_a
    }
}
