use std::{f64::consts::PI, rc::Rc};
use chrono::NaiveDateTime;
use gtk::cairo;
use itertools::{izip, Itertools};
use crate::utils::math::linear_interpolate;
use super::{utils::*, data::*};

const STAR_LIGHT_MIN_VISIBLE: f32 = 0.1;

#[derive(Clone)]
pub struct ViewPoint {
    pub crd:        HorizCoord,
    pub mag_factor: f64, // magnification factor
}

impl ViewPoint {
    pub fn new() -> Self {
        let crd = HorizCoord {
            alt: degree_to_radian(20.0),
            az:  0.0,
        };
        Self {
            crd,
            mag_factor: 1.0,
        }
    }
}

#[derive(Clone)]
pub struct CameraFrame {
    pub name: String,
    pub horiz_angle: f64,
    pub vert_angle: f64,
    pub rot_angle: f64,
}

#[derive(Clone)]
pub struct HorizonGlowPaintConfig {
    enabled: bool,
    angle:   f64,
}

impl Default for HorizonGlowPaintConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            angle: 10.0,
        }
    }
}

#[derive(Clone)]
pub struct PaintConfig {
    pub filter:       ItemFilterFlags,
    pub max_dso_mag:  f32,
    pub horizon_glow: HorizonGlowPaintConfig,
}

impl Default for PaintConfig {
    fn default() -> Self {
        Self {
            filter:        ItemFilterFlags::all(),
            max_dso_mag:  10.0,
            horizon_glow: HorizonGlowPaintConfig::default(),
        }
    }
}

pub struct SkyMapPainter {
    obj_painter: ObjectPainter,
    dso_ellipse: DsoEllipse,
}

impl SkyMapPainter {
    pub fn new() -> Self {
        Self {
            obj_painter: ObjectPainter::new(),
            dso_ellipse: DsoEllipse::new(),
        }
    }

    pub fn paint(
        &mut self,
        sky_map:    &Option<Rc<SkyMap>>,
        selection:  &Option<SkymapObject>,
        tele_pos:   &Option<EqCoord>,
        cam_frame:  &Option<CameraFrame>,
        observer:   &Observer,
        utc_time:   &NaiveDateTime,
        config:     &PaintConfig,
        view_point: &ViewPoint,
        screen:     &ScreenInfo,
        cairo:      &gtk::cairo::Context,
    ) -> anyhow::Result<()> {
        cairo.set_antialias(gtk::cairo::Antialias::None);
        cairo.set_source_rgb(0.0, 0.0, 0.0);
        cairo.paint()?;

        let eq_hor_cvt = EqToHorizCvt::new(observer, utc_time);
        let hor_3d_cvt = HorizToScreenCvt::new(view_point);
        let pxls_per_rad = Self::calc_pixels_per_radian(screen, view_point.mag_factor);
        let ctx = PaintCtx { cairo, config, screen, view_point, pxls_per_rad };

        let star_painter_options = self.get_star_painter_options(&ctx);

        // Equatorial grid
        self.paint_eq_grid(&eq_hor_cvt, &ctx, &hor_3d_cvt)?;

        if let Some(sky_map) = sky_map {
            // DSO objects
            self.paint_dso_items(sky_map, &ctx, &eq_hor_cvt, &hor_3d_cvt, PainterMode::Objects)?;

            // Stars objects
            if config.filter.contains(ItemFilterFlags::STARS) {
                self.paint_stars(
                    sky_map,
                    &star_painter_options,
                    &ctx,
                    &eq_hor_cvt,
                    &hor_3d_cvt,
                    PainterMode::Objects
                )?;
            }

            // DSO names
            self.paint_dso_items(sky_map, &ctx, &eq_hor_cvt, &hor_3d_cvt, PainterMode::Names)?;

            // Stars names
            if config.filter.contains(ItemFilterFlags::STARS) {
                self.paint_stars(
                    sky_map,
                    &star_painter_options,
                    &ctx,
                    &eq_hor_cvt,
                    &hor_3d_cvt,
                    PainterMode::Names
                )?;
            }
        }

        // Horizon glow
        if config.horizon_glow.enabled {
            self.paint_horizon_glow(cairo, &eq_hor_cvt, &ctx, &hor_3d_cvt)?;
        }

        // Ground
        self.paint_ground(&eq_hor_cvt, &ctx, &hor_3d_cvt)?;

        // Selected object
        self.paint_selection(selection, &ctx, &eq_hor_cvt, &hor_3d_cvt)?;

        // Optionally telescope position
        self.paint_telescope_position(tele_pos, &ctx, &eq_hor_cvt, &hor_3d_cvt)?;

        // Optionally camera frame
        self.paint_camera_frame(cam_frame, &ctx, &eq_hor_cvt, &hor_3d_cvt)?;

        Ok(())
    }

    pub fn paint_eq_test(
        &mut self,
        crd:        &HorizCoord,
        observer:   &Observer,
        utc_time:   &NaiveDateTime,
        config:     &PaintConfig,
        view_point: &ViewPoint,
        screen:     &ScreenInfo,
        cairo:      &gtk::cairo::Context,
    ) -> anyhow::Result<()> {

        let eq_hor_cvt = EqToHorizCvt::new(observer, utc_time);
        let hor_3d_cvt = HorizToScreenCvt::new(view_point);
        let pxls_per_rad = Self::calc_pixels_per_radian(screen, view_point.mag_factor);
        let ctx = PaintCtx { cairo, config, screen, view_point, pxls_per_rad };

        let circle = TestHorizCircle(crd.clone());

        self.obj_painter.paint(
            &circle,
            &eq_hor_cvt,
            &hor_3d_cvt,
            &ctx
        )?;

        Ok(())
    }

    fn calc_pixels_per_radian(
        screen:     &ScreenInfo,
        mag_factor: f64,
    ) -> f64 {
        const ANGLE_DIFF: f64 = 2.0 * PI / (360.0 * 60.0);
        let mut view_point = ViewPoint::new();
        view_point.mag_factor = mag_factor;
        let cvt = HorizToScreenCvt::new(&view_point);
        let mut pt = view_point.crd.clone();
        let scrd1 = cvt.horiz_to_sphere(&pt);
        pt.az += ANGLE_DIFF;
        let scrd2 = cvt.horiz_to_sphere(&pt);
        let crd1 = cvt.sphere_to_screen(&scrd1, screen);
        let crd2 = cvt.sphere_to_screen(&scrd2, screen);
        Point2D::distance(&crd1, &crd2) / ANGLE_DIFF
    }

    fn paint_dso_items(
        &mut self,
        sky_map:    &SkyMap,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
        mode:       PainterMode,
    ) -> anyhow::Result<()> {
        for dso_object in sky_map.objects() {
            let Some(mag) = dso_object.any_magnitude() else {
                continue;
            };
            if mag.get() > ctx.config.max_dso_mag {
                continue;
            }
            let visible = dso_object.obj_type.test_filter_flag(&ctx.config.filter);
            if !visible { continue; }

            match mode {
                PainterMode::Objects => {
                    let test_visiblity = PointVisibilityTestObject {
                        coord: dso_object.crd.to_eq()
                    };

                    let is_visible_on_screen = self.obj_painter.paint(
                        &test_visiblity,
                        &eq_hor_cvt,
                        &hor_3d_cvt,
                        ctx,
                    )?;

                    // Paint ellipse of object
                    if is_visible_on_screen {
                        self.paint_dso_ellipse(dso_object, ctx, eq_hor_cvt, hor_3d_cvt)?;
                    }
                }
                PainterMode::Names => {
                    let name_painter = DsoNamePainter(dso_object);
                    self.obj_painter.paint(&name_painter, &eq_hor_cvt, &hor_3d_cvt, ctx)?;
                }
            }
        }

        Ok(())
    }

    fn paint_dso_ellipse(
        &mut self,
        dso_object: &DsoItem,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        let maj_axis = dso_object.maj_axis.unwrap_or_default();
        let min_axis = dso_object.min_axis.unwrap_or(maj_axis);
        let maj_axis = arcmin_to_radian(maj_axis as f64);
        let min_axis = arcmin_to_radian(min_axis as f64);

        let min_axis_value = 2.0 * ctx.screen.dpmm_x / ctx.pxls_per_rad;
        let maj_axis = maj_axis.max(min_axis_value);
        let min_axis = min_axis.max(min_axis_value);

        let angle = dso_object.angle.unwrap_or_default();
        let obj_dec = dso_object.crd.dec();
        let obj_ra = dso_object.crd.ra();
        let dec_rot = RotMatrix::new(0.5 * PI - obj_dec);
        let ra_rot = RotMatrix::new(PI - obj_ra);
        const ELLIPSE_PTS_COUNT: usize = 66;
        let a = 0.5 * maj_axis;
        let b = 0.5 * min_axis;
        self.dso_ellipse.points.clear();
        for i in 0..ELLIPSE_PTS_COUNT {
            let az = 2.0 * PI * i as f64 / ELLIPSE_PTS_COUNT as f64;
            let sin_az = f64::sin(az);
            let cos_az = f64::cos(az);
            let alt = a * b / f64::sqrt(a * a * sin_az * sin_az + b * b * cos_az * cos_az);
            let crd = HorizCoord { alt: 0.5 * PI - alt, az: az - angle as f64 };
            let mut pt = crd.to_sphere_pt();
            pt.rotate_over_x(&dec_rot);
            pt.rotate_over_y(&ra_rot);
            let crd = pt.to_horiz_crd();
            self.dso_ellipse.points.push(EqCoord { dec: crd.alt, ra: crd.az });
        }
        let mut line_width = 0.01 * f64::max(maj_axis, min_axis) * ctx.pxls_per_rad;
        line_width = line_width.max(1.0);
        line_width = line_width.min(5.0 * ctx.screen.dpmm_x);

        self.dso_ellipse.line_width = line_width;
        self.dso_ellipse.dso_type = dso_object.obj_type;
        self.obj_painter.paint(&self.dso_ellipse, &eq_hor_cvt, &hor_3d_cvt, ctx)?;

        Ok(())
    }

    fn get_star_painter_options(&self, ctx: &PaintCtx) -> StarPainterOptions {
        let max_size = 7.0 * ctx.screen.dpmm_x;
        let slow_grow_size = 3.0 * ctx.screen.dpmm_x;
        let light_size_k = 0.3 * ctx.screen.dpmm_x;
        let min_bright_size = 1.5 * ctx.screen.dpmm_x;
        let max_mag_value = calc_max_star_magnitude_for_painting(ctx.view_point.mag_factor);

        StarPainterOptions {
            max_mag_value,
            max_size,
            slow_grow_size,
            light_size_k,
            min_bright_size,
        }
    }

    fn paint_star(
        &mut self,
        star_data:  &StarData,
        name:       &str,
        bayer:      &str,
        options:    &StarPainterOptions,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
        mode:       PainterMode,
    ) -> anyhow::Result<bool> {
        let star_painter = StarPainter {
            mode,
            star: star_data,
            name,
            bayer,
            options,
        };
        let star_is_painted = self.obj_painter.paint(
            &star_painter,
            &eq_hor_cvt,
            &hor_3d_cvt,
            ctx,
        )?;
        Ok(star_is_painted)
    }

    fn paint_stars(
        &mut self,
        sky_map:    &SkyMap,
        options:    &StarPainterOptions,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
        mode:       PainterMode,
    ) -> anyhow::Result<()> {
        ctx.cairo.set_antialias(gtk::cairo::Antialias::Fast);
        let center_eq_crd = eq_hor_cvt.horiz_to_eq(&ctx.view_point.crd);
        let center_zone_key = Stars::get_key_for_coord(center_eq_crd.ra, center_eq_crd.dec);
        let max_mag_value = calc_max_star_magnitude_for_painting(ctx.view_point.mag_factor);
        let max_mag = ObjMagnitude::new(max_mag_value);
        let stars = sky_map.stars();
        let mut _stars_count = 0_usize;
        let mut _stars_painted_count = 0_usize;
        for (zone_key, zone) in stars.zones() {
            let zone_is_visible = if &center_zone_key == zone_key {
                // this zone is visible as center of screen points at it
                true
            } else {
                // test if zone is visible
                let vis_test_obj = ZoneVisibilityTestObject {
                    coords: zone.coords().clone(),
                };
                self.obj_painter.paint(
                    &vis_test_obj,
                    &eq_hor_cvt,
                    &hor_3d_cvt,
                    ctx,
                ).unwrap_or_default()
            };

            if !zone_is_visible {
                continue;
            }

            if mode == PainterMode::Objects {
                for star in zone.stars() {
                    if star.data.mag > max_mag {
                        continue;
                    }
                    let star_is_painted = self.paint_star(
                        &star.data, "", "",
                        options, ctx, eq_hor_cvt, hor_3d_cvt, mode,
                    )?;
                    _stars_count += 1;
                    if star_is_painted { _stars_painted_count += 1; }
                }
            }

            for star in zone.named_stars() {
                if star.data.mag > max_mag {
                    continue;
                }
                let star_is_painted = self.paint_star(
                    &star.data, &star.name, &star.bayer,
                    options, ctx, eq_hor_cvt, hor_3d_cvt, mode,
                )?;
                _stars_count += 1;
                if star_is_painted { _stars_painted_count += 1; }
            }
        }

        Ok(())
    }

    fn paint_eq_grid(
        &mut self,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        ctx.cairo.set_source_rgba(0.0, 0.0, 1.0, 0.7);
        ctx.cairo.set_line_width(1.0);
        ctx.cairo.set_antialias(gtk::cairo::Antialias::Fast);

        const DEC_STEP: i32 = 10; // degree
        const RA_STEP: i32 = 20; // degree
        const STEP: i32 = 5;
        for i in -90/STEP..90/STEP {
            let dec1 = degree_to_radian((STEP * i) as f64);
            let dec2 = degree_to_radian((STEP * (i + 1)) as f64);
            for j in 0..(360/RA_STEP) {
                let ra = degree_to_radian((RA_STEP * j) as f64);
                let dec_line = EqGridItem { dec1, dec2, ra1: ra, ra2: ra };
                self.obj_painter.paint(&dec_line, &eq_hor_cvt, &hor_3d_cvt, ctx)?;
            }
        }
        for j in 0..(360/STEP) {
            let ra1 = degree_to_radian((STEP * j) as f64);
            let ra2 = degree_to_radian((STEP * (j + 1)) as f64);
            for i in -90/DEC_STEP..90/DEC_STEP {
                let dec = degree_to_radian((DEC_STEP * i) as f64);
                let ra_line = EqGridItem { dec1: dec, dec2: dec, ra1, ra2 };
                self.obj_painter.paint(&ra_line, &eq_hor_cvt, &hor_3d_cvt, ctx)?;
            }
        }
        Ok(())
    }

    fn paint_ground(
        &mut self,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        let ground = Ground { view_point: ctx.view_point };
        self.obj_painter.paint(&ground, &eq_hor_cvt, &hor_3d_cvt, ctx)?;
        let world_sides = [
            WorldSide { az:   0.0, text: "S",  alpha: 1.0 },
            WorldSide { az:  45.0, text: "SE", alpha: 0.5 },
            WorldSide { az:  90.0, text: "E",  alpha: 1.0 },
            WorldSide { az: 135.0, text: "NE", alpha: 0.5 },
            WorldSide { az: 180.0, text: "N",  alpha: 1.0 },
            WorldSide { az: 225.0, text: "NW", alpha: 0.5 },
            WorldSide { az: 270.0, text: "W",  alpha: 1.0 },
            WorldSide { az: 315.0, text: "SW", alpha: 0.5 },
        ];
        ctx.cairo.set_font_size(6.0 * ctx.screen.dpmm_y);
        for world_side in world_sides {
            self.obj_painter.paint(&world_side, &eq_hor_cvt, &hor_3d_cvt, ctx)?;
        }
        Ok(())
    }

    fn paint_horizon_glow(
        &mut self,
        cairo:      &gtk::cairo::Context,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        const STEP: i32 = 2;
        let angle = degree_to_radian(ctx.config.horizon_glow.angle);

        cairo.set_antialias(gtk::cairo::Antialias::None);

        for i in 0..(360/STEP) {
            let az1 = degree_to_radian((STEP * i) as f64);
            let az2 = degree_to_radian((STEP * (i+1)) as f64);
            let item = HorizonGlowItem {
                coords: [
                    HorizCoord { alt: angle, az: az1 },
                    HorizCoord { alt: angle, az: az2 },
                    HorizCoord { alt:   0.0, az: az2 },
                    HorizCoord { alt:   0.0, az: az1 },
                ]
            };
            self.obj_painter.paint(
                &item,
                &eq_hor_cvt,
                &hor_3d_cvt,
                ctx,
            )?;
        }

        Ok(())
    }

    fn paint_selection(
        &mut self,
        selection:  &Option<SkymapObject>,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        let Some(selection) = selection else { return Ok(()); };
        let size = 12.0 * ctx.screen.dpmm_x;
        let thickness = 1.0 * ctx.screen.dpmm_x;
        let crd = selection.crd();
        let selection_painter = SelectionPainter { crd, size, thickness };
        self.obj_painter.paint(
            &selection_painter,
            &eq_hor_cvt,
            &hor_3d_cvt,
            ctx,
        )?;
        Ok(())
    }

    fn paint_telescope_position(
        &mut self,
        tele_pos:   &Option<EqCoord>,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        let Some(telescope_pos) = tele_pos else { return Ok(()); };
        let painter = TelescopePosPainter {
            crd: *telescope_pos,
        };
        self.obj_painter.paint(
            &painter,
            &eq_hor_cvt,
            &hor_3d_cvt,
            ctx,
        )?;
        Ok(())
    }

    fn paint_camera_frame(
        &mut self,
        cam_frame:  &Option<CameraFrame>,
        ctx:        &PaintCtx,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        if let Some(cam_frame) = cam_frame {
            let center_crd = eq_hor_cvt.horiz_to_eq(&ctx.view_point.crd);
            let dec_rot = RotMatrix::new(center_crd.dec);
            let ra_rot = RotMatrix::new(-center_crd.ra);

            let parts = [ (0.5, 0.5), (0.5, -0.5), (-0.5, -0.5), (-0.5, 0.5) ];
            let mut coords = [EqCoord {dec: 0.0, ra: 0.0}; 4];

            for ((h, v), crd) in izip!(&parts, &mut coords) {
                let h_crd = HorizCoord {
                    alt: h * cam_frame.vert_angle,
                    az: v * cam_frame.horiz_angle,
                };
                let mut pt = h_crd.to_sphere_pt();
                pt.rotate_over_x(&dec_rot);
                pt.rotate_over_y(&ra_rot);
                let h_crd = pt.to_horiz_crd();
                *crd = EqCoord { dec: h_crd.alt, ra: h_crd.az };
            }

            let painter = CameraFramePainter { name: &cam_frame.name, coords };
            self.obj_painter.paint(
                &painter,
                &eq_hor_cvt,
                &hor_3d_cvt,
                ctx,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PainterMode {
    Objects,
    Names,
}

enum PainterCrd {
    Horiz(HorizCoord),
    Eq(EqCoord),
}

struct PaintCtx<'a> {
    cairo:        &'a gtk::cairo::Context,
    config:       &'a PaintConfig,
    screen:       &'a ScreenInfo,
    view_point:   &'a ViewPoint,
    pxls_per_rad: f64,
}

trait ItemPainter {
    fn points_count(&self) -> usize;
    fn point_crd(&self, index: usize) -> PainterCrd;
    fn paint(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> anyhow::Result<()> { Ok(()) }
}

struct ObjectPainter {
    points_horiz:  Vec<HorizCoord>,
    points_3d:     Vec<Point3D>,
    points_screen: Vec<Point2D>,
}

impl ObjectPainter {
    fn new() -> Self {
        Self {
            points_horiz:  Vec::new(),
            points_3d:     Vec::new(),
            points_screen: Vec::new(),
        }
    }

    fn paint(
        &mut self,
        obj:        &dyn ItemPainter,
        eq_hor_cvt: &EqToHorizCvt,
        hor_3d_cvt: &HorizToScreenCvt,
        ctx:        &PaintCtx,
    ) -> anyhow::Result<bool> {
        let points_count = obj.points_count();

        self.points_horiz.clear();
        let mut obj_is_visible = false;
        for i in 0..points_count {
            let horiz_crd = match obj.point_crd(i) {
                PainterCrd::Horiz(horiz) => horiz,

                // equatorial coorinates -> horizontal coorinates
                PainterCrd::Eq(eq) => eq_hor_cvt.eq_to_horiz(&eq),
            };

            if horiz_crd.alt >= 0.0 {
                obj_is_visible = true;
            }

            self.points_horiz.push(horiz_crd);
        }
        if !obj_is_visible {
            return Ok(false);
        }

        // horizontal coorinates -> 3d coordinates
        // + az and alt rotating
        obj_is_visible = false;
        self.points_3d.clear();
        for pt in &self.points_horiz {
            let pt3d = hor_3d_cvt.horiz_to_sphere(pt);
            if pt3d.z > -0.3 {
                obj_is_visible = true;
            }
            self.points_3d.push(pt3d);
        }
        if !obj_is_visible {
            return Ok(false);
        }

        // 3d coordinates -> screen coordinates
        self.points_screen.clear();
        obj_is_visible = false;
        for pt in &self.points_3d {
            let pt_s = hor_3d_cvt.sphere_to_screen(pt, ctx.screen);
            if ctx.screen.tolerance.left < pt_s.x && pt_s.x < ctx.screen.tolerance.right
            && ctx.screen.tolerance.top < pt_s.y && pt_s.y < ctx.screen.tolerance.bottom {
                obj_is_visible = true;
            }
            self.points_screen.push(pt_s);
        }

        // check if 2d lines is crossing by screen boundaries
        if !obj_is_visible && self.points_screen.len() >= 2 {
            let rect = &ctx.screen.tolerance;
            let top_line = Line2D {
                crd1: Point2D { x: rect.left, y: rect.top },
                crd2: Point2D { x: rect.right, y: rect.top }
            };
            let bottom_line = Line2D {
                crd1: Point2D { x: rect.left, y: rect.bottom },
                crd2: Point2D { x: rect.right, y: rect.bottom }
            };
            let left_line = Line2D {
                crd1: Point2D { x: rect.left, y: rect.top },
                crd2: Point2D { x: rect.left, y: rect.bottom }
            };
            let right_line = Line2D {
                crd1: Point2D { x: rect.right, y: rect.top },
                crd2: Point2D { x: rect.right, y: rect.bottom }
            };
            obj_is_visible =
                self.points_screen
                    .iter()
                    .circular_tuple_windows()
                    .any(|(&crd1, &crd2)| {
                        let line = Line2D { crd1, crd2 };
                        Line2D::intersection(&line, &top_line).is_some() ||
                        Line2D::intersection(&line, &bottom_line).is_some() ||
                        Line2D::intersection(&line, &left_line).is_some() ||
                        Line2D::intersection(&line, &right_line).is_some()
                    }
                );
        }

        if !obj_is_visible {
            return Ok(false);
        }

        obj.paint(ctx, &self.points_screen)?;

        Ok(true)
    }
}

// Paint DSP item

struct DsoNamePainter<'a>(&'a DsoItem);

impl<'a> ItemPainter for DsoNamePainter<'a> {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(EqCoord {
            ra: self.0.crd.ra(),
            dec: self.0.crd.dec()
        })
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let crd = &points[0];
        let mut y = crd.y;
        let text_height = ctx.cairo.font_extents()?.height();
        ctx.cairo.set_source_rgba(1.0, 1.0, 1.0, 0.7);
        for item in &self.0.names {
            ctx.cairo.move_to(crd.x, y);
            ctx.cairo.show_text(&item.name)?;
            y += text_height;
        }
        Ok(())
    }
}

// Paint ellipse around DSO object

struct DsoEllipse {
    points:     Vec<EqCoord>,
    line_width: f64,
    dso_type:   SkyItemType,
}

impl DsoEllipse {
    fn new() -> Self {
        Self {
            points: Vec::new(),
            line_width: 1.0,
            dso_type: SkyItemType::Galaxy
        }
    }
}

impl ItemPainter for DsoEllipse {
    fn points_count(&self) -> usize {
        self.points.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Eq(self.points[index].clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.close_path();

        match self.dso_type {
            SkyItemType::StarCluster => {
                ctx.cairo.set_dash(&[3.0 * self.line_width], 0.0);
                ctx.cairo.set_line_width(3.0 * self.line_width);
                ctx.cairo.set_source_rgba(1.0, 1.0, 0.0, 0.8);
            },

            SkyItemType::Galaxy => {
                ctx.cairo.set_source_rgba(0.0, 1.0, 0.0, 0.8);
                ctx.cairo.set_line_width(self.line_width);
            },

            SkyItemType::PlanetaryNebula => {
                ctx.cairo.set_source_rgba(0.2, 0.2, 1.0, 1.0);
                ctx.cairo.set_line_width(self.line_width);
            },

            SkyItemType::DarkNebula |
            SkyItemType::EmissionNebula |
            SkyItemType::Nebula |
            SkyItemType::HIIIonizedRegion => {
                ctx.cairo.set_source_rgba(1.0, 0.0, 0.0, 0.8);
                ctx.cairo.set_line_width(self.line_width);
            },

            _ => {
                ctx.cairo.set_source_rgba(0.9, 0.9, 0.9, 0.8);
                ctx.cairo.set_line_width(self.line_width);
            },
        }

        ctx.cairo.set_antialias(gtk::cairo::Antialias::Fast);
        ctx.cairo.stroke()?;

        ctx.cairo.set_dash(&[], 0.0);

        Ok(())
    }
}

// Paint outline

impl ItemPainter for Outline {
    fn points_count(&self) -> usize {
        self.polygon.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        let pt = &self.polygon[index];
        PainterCrd::Eq(EqCoord {
            ra: pt.ra(),
            dec: pt.dec()
        })
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.close_path();
        ctx.cairo.set_source_rgba(0.5, 0.5, 0.5, 0.15);
        ctx.cairo.set_antialias(gtk::cairo::Antialias::None);
        ctx.cairo.fill_preserve()?;
        ctx.cairo.set_source_rgb(0.4, 0.4, 0.4);
        ctx.cairo.set_line_width(1.0);
        ctx.cairo.set_antialias(gtk::cairo::Antialias::Fast);
        ctx.cairo.stroke()?;
        Ok(())
    }
}

// Paint star

struct StarPainterOptions {
    max_size:        f64,
    max_mag_value:   f32,
    slow_grow_size:  f64,
    light_size_k:    f64,
    min_bright_size: f64,
}
struct StarPainter<'a> {
    star:    &'a StarData,
    mode:    PainterMode,
    name:    &'a str,
    bayer:   &'a str,
    options: &'a StarPainterOptions,
}

type RgbTuple = (f64, f64, f64);

impl<'a> StarPainter<'a> {
    fn calc_light(&self, star_mag: f32) -> (f32, f32) {
        let light = f32::powf(2.0, 0.4 * (self.options.max_mag_value - star_mag)) - 1.0;
        let light_with_gamma = light.powf(0.7);
        (light, light_with_gamma)
    }

    fn calc_diam(&self, light: f32) -> f64 {
        let mut diam = (self.options.light_size_k * light as f64).max(1.0);

        if self.star.mag.get() < 1.0 {
            diam = diam.max(self.options.min_bright_size)
        }

        if diam > self.options.slow_grow_size {
            diam -= self.options.slow_grow_size;
            diam /= 2.0;
            diam += self.options.slow_grow_size;
        }
        if diam > self.options.max_size {
            diam = self.options.max_size;
        }
        diam
    }

    fn get_rgb_for_star_bv(bv: f32) -> RgbTuple {
        const RED_V: f32 = 2.5;
        const BLUE_V: f32 = -0.3;

        const RED:    RgbTuple = (1.0, 0.4,  0.4);
        const ORANGE: RgbTuple = (1.0, 0.9,  0.6);
        const WELLOW: RgbTuple = (1.0, 1.0,  0.7);
        const WHITE:  RgbTuple = (0.9, 0.9,  0.9);
        const BLUE:   RgbTuple = (0.4, 0.66, 1.0);

        const TABLE: &[(f32, RgbTuple)] = &[
            (BLUE_V, BLUE),
            (0.0,    WHITE),
            (0.65,   WELLOW),
            (1.6,    ORANGE),
            (RED_V,  RED),
        ];

        if bv >= RED_V {
            return RED;
        } else if bv <= BLUE_V {
            return BLUE;
        }

        for ((v1, (r1, g1, b1)), (v2, (r2, g2, b2)))
        in TABLE.iter().tuple_windows() {
            if *v1 <= bv && bv <= *v2 {
                let r = linear_interpolate(bv as f64, *v1 as f64, *v2 as f64, *r1, *r2);
                let g = linear_interpolate(bv as f64, *v1 as f64, *v2 as f64, *g1, *g2);
                let b = linear_interpolate(bv as f64, *v1 as f64, *v2 as f64, *b1, *b2);
                return (r, g, b);
            }
        }

        unreachable!()
    }

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt = &points[0];
        let star_mag = self.star.mag.get();
        let (light, light_with_gamma) = self.calc_light(star_mag);
        if light_with_gamma < 0.1 { return Ok(()); }
        let (r, g, b) = Self::get_rgb_for_star_bv(self.star.bv.get());
        ctx.cairo.save()?;
        ctx.cairo.translate(pt.x, pt.y);
        let diam = self.calc_diam(light);
        if diam <= 1.0 {
            ctx.cairo.set_source_rgb(
                diam * light_with_gamma.min(1.0) as f64 * r,
                diam * light_with_gamma.min(1.0) as f64 * g,
                diam * light_with_gamma.min(1.0) as f64 * b,
            );
            ctx.cairo.rectangle(-0.5, -0.5, 1.0, 1.0);
        } else if diam <= ctx.screen.dpmm_x {
            ctx.cairo.set_source_rgb(
                light_with_gamma as f64 * r,
                light_with_gamma as f64 * g,
                light_with_gamma as f64 * b,
            );
            ctx.cairo.arc(0.0, 0.0, 0.5 * diam, 0.0, 2.0 * PI);
        } else {
            let grad = cairo::RadialGradient::new(0.0, 0.0, 0.1 * diam, 0.0, 0.0, 0.75 * diam);
            grad.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 1.0);
            grad.add_color_stop_rgba(0.25, r, g, b, 1.0);
            grad.add_color_stop_rgba(1.0, r, g, b, 0.0);
            ctx.cairo.set_source(&grad)?;
            ctx.cairo.arc(0.0, 0.0, 0.75 * diam, 0.0, 2.0 * PI);
        }
        ctx.cairo.fill()?;
        ctx.cairo.restore()?;
        Ok(())
    }

    fn paint_name(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        if self.name.is_empty() && self.bayer.is_empty() { return Ok(()); }
        let star_mag = self.star.mag.get();
        let (light, light_with_gamma) = self.calc_light(star_mag);
        let (r, g, b) = Self::get_rgb_for_star_bv(self.star.bv.get());
        let diam = self.calc_diam(light);
        let pt = &points[0];
        let paint_text = |text, index, light_with_gamma| -> anyhow::Result<()> {
            let mut light_with_gamma = light_with_gamma;
            if light_with_gamma < 0.5 { return Ok(()); }
            light_with_gamma -= 0.5;
            light_with_gamma *= 2.0;

            let te = ctx.cairo.text_extents(text)?;
            let t_height = te.height();

            ctx.cairo.set_source_rgba(
                r, g, b,
                light_with_gamma as f64,
            );
            ctx.cairo.move_to(
                pt.x + 0.5 * diam - 0.1 * t_height,
                pt.y + t_height + 0.5 * diam - 0.1 * t_height + index as f64 * 1.2 * t_height
            );
            ctx.cairo.show_text(text)?;
            Ok(())
        };

        let mut text_index = 0;
        if !self.name.is_empty() {
            paint_text(self.name, text_index, light_with_gamma)?;
            text_index += 1;
        }

        if !self.bayer.is_empty() {
            paint_text(&self.bayer, text_index, 0.5 * light_with_gamma)?;
        }

        Ok(())
    }
}

impl<'a> ItemPainter for StarPainter<'a> {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(EqCoord {
            dec: self.star.crd.dec(),
            ra: self.star.crd.ra(),
        })
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        match self.mode {
            PainterMode::Objects =>
                self.paint_object(ctx, points),
            PainterMode::Names =>
                self.paint_name(ctx, points),
        }
    }
}

// Paint equatorial grid

struct EqGridItem {
    dec1: f64,
    dec2: f64,
    ra1:  f64,
    ra2:  f64,
}

impl ItemPainter for EqGridItem {
    fn points_count(&self) -> usize {
        2
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        match index {
            0 => PainterCrd::Eq(EqCoord{ ra: self.ra1, dec: self.dec1 }),
            1 => PainterCrd::Eq(EqCoord{ ra: self.ra2, dec: self.dec2 }),
            _ => unreachable!(),
        }
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt1 = &points[0];
        let pt2 = &points[1];
        ctx.cairo.move_to(pt1.x, pt1.y);
        ctx.cairo.line_to(pt2.x, pt2.y);
        ctx.cairo.stroke()?;
        Ok(())
    }
}

// Paint ground

struct Ground<'a> {
    view_point: &'a ViewPoint,
}

const GROUND_ANGLE_STEP: usize = 5;

impl<'a> ItemPainter for Ground<'a> {
    fn points_count(&self) -> usize {
        360 / GROUND_ANGLE_STEP
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Horiz(HorizCoord {
            alt: 0.0,
            az: PI * (index * GROUND_ANGLE_STEP) as f64 / 180.0
        })
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let ground_around = self.view_point.crd.alt >= 0.0;
        if ground_around {
            const LAYER: f64 = 100.0;
            let mut min_x = points.iter().map(|p| p.x).min_by(f64::total_cmp).unwrap_or_default() - LAYER;
            let mut min_y = points.iter().map(|p| p.y).min_by(f64::total_cmp).unwrap_or_default() - LAYER;
            let mut max_x = points.iter().map(|p| p.x).max_by(f64::total_cmp).unwrap_or_default() + LAYER;
            let mut max_y = points.iter().map(|p| p.y).max_by(f64::total_cmp).unwrap_or_default() + LAYER;
            if min_x > ctx.screen.rect.left {
                min_x = ctx.screen.rect.left;
            }
            if min_y > ctx.screen.rect.top {
                min_y = ctx.screen.rect.top;
            }
            if max_x < ctx.screen.rect.right {
                max_x = ctx.screen.rect.right;
            }
            if max_y < ctx.screen.rect.bottom {
                max_y = ctx.screen.rect.bottom;
            }
            ctx.cairo.rectangle(min_x, min_y, max_x-min_x, max_y-min_y);
            ctx.cairo.close_path();
        }
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.close_path();
        if ground_around {
            ctx.cairo.set_fill_rule(gtk::cairo::FillRule::EvenOdd);
        }
        ctx.cairo.set_source_rgb(0.1, 0.1, 0.05);
        ctx.cairo.set_antialias(gtk::cairo::Antialias::None);
        ctx.cairo.fill()?;
        Ok(())
    }
}

// Paint side of the world

struct WorldSide<'a> {
    text: &'a str,
    az: f64,
    alpha: f64,
}

impl<'a> ItemPainter for WorldSide<'a> {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Horiz(HorizCoord {
            alt: 0.0,
            az: degree_to_radian(self.az)
        })
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let te = ctx.cairo.text_extents(&self.text)?;
        ctx.cairo.move_to(
            points[0].x - 0.5 * te.width(),
            points[0].y + 0.5 * te.height()
        );
        ctx.cairo.set_source_rgba(0.8, 0.0, 0.0, self.alpha);
        ctx.cairo.show_text(&self.text)?;
        Ok(())
    }
}

// Paint horizon glow

struct HorizonGlowItem {
    coords: [HorizCoord; 4],
}

impl ItemPainter for HorizonGlowItem {
    fn points_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Horiz(self.coords[index].clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let top_pt_x = 0.5 * (points[0].x + points[1].x);
        let top_pt_y = 0.5 * (points[0].y + points[1].y);
        let bottom_pt_x = 0.5 * (points[2].x + points[3].x);
        let bottom_pt_y = 0.5 * (points[2].y + points[3].y);
        let gradient = gtk::cairo::LinearGradient::new(
            top_pt_x, top_pt_y,
            bottom_pt_x, bottom_pt_y
        );
        const R: f64 = 0.25;
        const G: f64 = 0.3;
        const B: f64 = 0.3;
        gradient.add_color_stop_rgba(0.0, R, G, B, 0.0);
        gradient.add_color_stop_rgba(1.0, R, G, B, 1.0);
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.close_path();
        ctx.cairo.set_source(&gradient)?;
        ctx.cairo.fill()?;
        Ok(())
    }
}

struct ZoneVisibilityTestObject {
    coords: [EqCoord; 4],
}

impl ItemPainter for ZoneVisibilityTestObject {
    fn points_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Eq(self.coords[index].clone())
    }
}

struct PointVisibilityTestObject {
    coord: EqCoord,
}

impl ItemPainter for PointVisibilityTestObject {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(self.coord.clone())
    }
}

struct TestHorizCircle(HorizCoord);

impl ItemPainter for TestHorizCircle {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Horiz(self.0.clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        ctx.cairo.arc(points[0].x, points[0].y, 4.0, 0.0, 2.0 * PI);
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        ctx.cairo.close_path();
        ctx.cairo.fill()?;
        Ok(())
    }
}

struct TelescopePosPainter {
    crd: EqCoord,
}

impl ItemPainter for TelescopePosPainter {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(self.crd.clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt = &points[0];
        let line_size = 40.0 * ctx.screen.dpmm_x;
        ctx.cairo.set_line_width(1.0);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_antialias(cairo::Antialias::Fast);
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        ctx.cairo.move_to(pt.x - 0.5 * line_size, pt.y);
        ctx.cairo.line_to(pt.x + 0.5 * line_size, pt.y);
        ctx.cairo.move_to(pt.x, pt.y - 0.5 * line_size);
        ctx.cairo.line_to(pt.x, pt.y + 0.5 * line_size);
        ctx.cairo.stroke()?;
        ctx.cairo.arc(pt.x, pt.y, 0.25 * line_size, 0.0, 2.0 * PI);
        ctx.cairo.stroke()?;
        Ok(())
    }
}

struct SelectionPainter {
    crd:       EqCoord,
    size:      f64,
    thickness: f64,
}

 impl ItemPainter for SelectionPainter {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(self.crd.clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt = &points[0];
        ctx.cairo.set_antialias(cairo::Antialias::Fast);
        ctx.cairo.set_source_rgb(1.0, 0.0, 1.0);
        ctx.cairo.set_line_width(self.thickness);
        ctx.cairo.rectangle(
            pt.x - 0.5 * self.size,
            pt.y - 0.5 * self.size,
            self.size,
            self.size
        );
        ctx.cairo.stroke()?;
        Ok(())
    }
}

struct CameraFramePainter<'a> {
    name:    &'a str,
    coords: [EqCoord; 4],
}

 impl<'a> ItemPainter for CameraFramePainter<'a> {
    fn points_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Eq(self.coords[index].clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.set_antialias(cairo::Antialias::Fast);
        ctx.cairo.close_path();
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_line_width(1.0);
        ctx.cairo.stroke()?;

        let pt1 = &points[0];
        let pt2 = &points[1];
        let dx = pt2.x - pt1.x;
        let dy = pt2.y - pt1.y;
        let len = f64::sqrt(dx * dx + dy * dy);

        ctx.cairo.set_font_size(4.0 * ctx.screen.dpmm_y);
        let te = ctx.cairo.text_extents(&self.name)?;
        if te.width() <= len {
            let angle = f64::atan2(dy, dx);
            ctx.cairo.move_to(pt1.x, pt1.y);
            ctx.cairo.save()?;
            ctx.cairo.rotate(angle);
            let descent = te.height() + te.y_bearing();
            ctx.cairo.rel_move_to(0.0, -descent);
            ctx.cairo.show_text(&self.name)?;
            ctx.cairo.restore()?;
        }

        Ok(())
    }
}

