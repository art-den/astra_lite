use std::{f64::consts::PI, rc::Rc};
use bitflags::bitflags;
use chrono::NaiveDateTime;
use gtk::cairo;
use itertools::Itertools;
use crate::utils::math::linear_interpolate;
use super::{utils::*, data::*};

const STAR_LIGHT_MIN_VISIBLE: f32 = 0.1;

#[derive(Clone)]
pub struct ViewPoint {
    pub crd: HorizCoord,
    pub mag_factor: f64, // magnification factor
}

impl ViewPoint {
    pub fn new() -> Self {
        let crd = HorizCoord {
            alt: 20.0 * PI / 180.0,
            az:  0.0,
        };
        Self {
            crd,
            mag_factor: 1.0,
        }
    }
}

bitflags! {
    pub struct PaintFlags: u32 {
        const PAINT_OUTLINES = 1 << 0;
        const PAINT_STARS    = 1 << 1;
        const PAINT_CLUSTERS = 1 << 2;
        const PAINT_NEBULAS  = 1 << 3;
        const PAINT_GALAXIES = 1 << 4;
    }
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
    pub flags:        PaintFlags,
    pub max_dso_mag:  f32,
    pub horizon_glow: HorizonGlowPaintConfig,
}

impl Default for PaintConfig {
    fn default() -> Self {
        Self {
            flags:        PaintFlags::all(),
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

        if let Some(sky_map) = sky_map {
            self.paint_dso(sky_map, &eq_hor_cvt, &ctx, &hor_3d_cvt)?;
        }

        self.paint_eq_grid(cairo, &eq_hor_cvt, &ctx, &hor_3d_cvt)?;

        if config.flags.contains(PaintFlags::PAINT_STARS) { if let Some(sky_map) = sky_map {
            self.paint_stars(sky_map, cairo, &eq_hor_cvt, &ctx, &hor_3d_cvt)?;
        }}

        if config.horizon_glow.enabled {
            self.paint_horizon_glow(cairo, &eq_hor_cvt, &ctx, &hor_3d_cvt)?;
        }

        self.paint_ground(cairo, &eq_hor_cvt, &ctx, &hor_3d_cvt, view_point)?;

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
            &ctx,
            PainterStage::Objects
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

    fn paint_dso(
        &mut self,
        sky_map:    &SkyMap,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        let mut do_paint_stage = |stage| -> anyhow::Result<()> {
            for dso_object in sky_map.objects() {
                let mag = dso_object.mag.get();
                if mag.is_nan() || mag > ctx.config.max_dso_mag {
                    continue;
                }
                use DsoType::*;
                let visible = match dso_object.obj_type {
                    Star | DoubleStar =>
                        ctx.config.flags.contains(PaintFlags::PAINT_STARS),
                    Galaxy | GalaxyPair | GalaxyTriplet | GroupOfGalaxies =>
                        ctx.config.flags.contains(PaintFlags::PAINT_GALAXIES),
                    StarCluster | AssociationOfStars =>
                        ctx.config.flags.contains(PaintFlags::PAINT_CLUSTERS),
                    PlanetaryNebula | DarkNebula | EmissionNebula | Nebula |
                    ReflectionNebula | SupernovaRemnant | HIIIonizedRegion =>
                        ctx.config.flags.contains(PaintFlags::PAINT_NEBULAS),
                    StarClusterAndNebula =>
                        ctx.config.flags.contains(PaintFlags::PAINT_NEBULAS) ||
                        ctx.config.flags.contains(PaintFlags::PAINT_CLUSTERS),
                    _ => false,
                };

                if visible {
                    match stage {
                        PainterStage::Objects => {
                            let test_visiblity = PointVisibilityTestObject { coord: EqCoord {
                                ra: dso_object.crd.ra(),
                                dec: dso_object.crd.dec(),
                            }};

                            let is_visible_on_screen = self.obj_painter.paint(
                                &test_visiblity,
                                &eq_hor_cvt,
                                &hor_3d_cvt,
                                ctx,
                                PainterStage::TestObjVsible
                            )?;

                            // Paint ellipse of object
                            if stage == PainterStage::Objects && is_visible_on_screen {
                                self.paint_dso_ellipse(eq_hor_cvt, ctx, hor_3d_cvt, dso_object)?;
                            }
                        }
                        PainterStage::Names => {
                            self.obj_painter.paint(dso_object, &eq_hor_cvt, &hor_3d_cvt, ctx, stage)?;
                        }
                        _ => {}
                    }
                }
            }
            if ctx.config.flags.contains(PaintFlags::PAINT_OUTLINES) {
                for outline in sky_map.outlines() {
                    self.obj_painter.paint(outline, &eq_hor_cvt, &hor_3d_cvt, ctx, stage)?;
                }
            }
            Ok(())
        };

        ctx.cairo.set_font_size(3.0 * ctx.screen.dpmm_y);
        do_paint_stage(PainterStage::Objects)?;
        do_paint_stage(PainterStage::Names)?;

        Ok(())
    }

    fn paint_dso_ellipse(
        &mut self,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
        dso_object: &DsoItem,
    ) -> anyhow::Result<()> {
        let maj_axis = dso_object.maj_axis.unwrap_or_default();
        let min_axis = dso_object.min_axis.unwrap_or(maj_axis);
        let maj_axis = 2.0 * PI * maj_axis as f64 / (360.0 * 60.0);
        let min_axis = 2.0 * PI * min_axis as f64 / (360.0 * 60.0);

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
        self.obj_painter.paint(&self.dso_ellipse, &eq_hor_cvt, &hor_3d_cvt, ctx, PainterStage::Objects)?;

        Ok(())
    }

    fn paint_stars(
        &mut self,
        sky_map:    &SkyMap,
        cairo:      &gtk::cairo::Context,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        cairo.set_antialias(gtk::cairo::Antialias::Fast);

        let center_eq_crd = eq_hor_cvt.horiz_to_eq(&ctx.view_point.crd);
        let center_zone_key = Stars::get_key_for_coord(center_eq_crd.ra, center_eq_crd.dec);

        let max_mag_value = calc_max_star_magnitude_for_painting(ctx.view_point.mag_factor);
        let max_mag = ObjMagnitude::new(max_mag_value);

        let max_size = 7.0 * ctx.screen.dpmm_x;
        let slow_grow_size = 0.2 * max_size;
        let light_size_k = 0.3 * ctx.screen.dpmm_x;
        let min_bright_size = 1.5 * ctx.screen.dpmm_x;

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
                    PainterStage::TestObjVsible
                ).unwrap_or_default()
            };

            if !zone_is_visible {
                continue;
            }

            let mut paint_star = |data: &StarData, name: &str| -> anyhow::Result<bool> {
                let star_painter = StarPainter {
                    data,
                    name,
                    max_mag_value,
                    max_size,
                    slow_grow_size,
                    light_size_k,
                    min_bright_size
                };
                let star_is_painted = self.obj_painter.paint(
                    &star_painter,
                    &eq_hor_cvt,
                    &hor_3d_cvt,
                    ctx,
                    PainterStage::ObjectsAndNames
                )?;

                Ok(star_is_painted)
            };

            for star in zone.stars() {
                if star.data.mag > max_mag {
                    continue;
                }
                let star_is_painted = paint_star(&star.data, "")?;
                _stars_count += 1;
                if star_is_painted { _stars_painted_count += 1; }
            }

            for star in zone.named_stars() {
                if star.data.mag > max_mag {
                    continue;
                }
                let star_is_painted = paint_star(&star.data, &star.name)?;
                _stars_count += 1;
                if star_is_painted { _stars_painted_count += 1; }
            }
        }

        Ok(())
    }

    fn paint_eq_grid(
        &mut self,
        cairo:      &gtk::cairo::Context,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
    ) -> anyhow::Result<()> {
        cairo.set_source_rgba(0.0, 0.0, 1.0, 0.7);
        cairo.set_line_width(1.0);
        cairo.set_antialias(gtk::cairo::Antialias::Fast);

        const DEC_STEP: i32 = 10; // degree
        const RA_STEP: i32 = 20; // degree
        const STEP: i32 = 5;
        for i in -90/STEP..90/STEP {
            let dec1 = PI * (STEP * i) as f64 / 180.0;
            let dec2 = PI * (STEP * (i + 1)) as f64 / 180.0;
            for j in 0..(360/RA_STEP) {
                let ra = PI * (RA_STEP * j) as f64 / 180.0;
                let dec_line = EqGridItem { dec1, dec2, ra1: ra, ra2: ra };
                self.obj_painter.paint(
                    &dec_line,
                    &eq_hor_cvt,
                    &hor_3d_cvt,
                    ctx,
                    PainterStage::Objects
                )?;
            }
        }
        for j in 0..(360/STEP) {
            let ra1 = PI * (STEP * j) as f64 / 180.0;
            let ra2 = PI * (STEP * (j + 1)) as f64 / 180.0;
            for i in -90/DEC_STEP..90/DEC_STEP {
                let dec = PI * (DEC_STEP * i) as f64 / 180.0;
                let ra_line = EqGridItem { dec1: dec, dec2: dec, ra1, ra2 };
                self.obj_painter.paint(
                    &ra_line,
                    &eq_hor_cvt,
                    &hor_3d_cvt,
                    ctx,
                    PainterStage::Objects
                )?;
            }
        }
        Ok(())
    }

    fn paint_ground(
        &mut self,
        cairo:      &gtk::cairo::Context,
        eq_hor_cvt: &EqToHorizCvt,
        ctx:        &PaintCtx,
        hor_3d_cvt: &HorizToScreenCvt,
        view_point: &ViewPoint,
    ) -> anyhow::Result<()> {
        let ground = Ground { view_point };
        self.obj_painter.paint(
            &ground,
            &eq_hor_cvt,
            &hor_3d_cvt,
            ctx,
            PainterStage::Objects
        )?;
        let world_sides = [
            WorldSide { az:   0.0 * PI / 180.0, text: "S",  alpha: 1.0 },
            WorldSide { az:  45.0 * PI / 180.0, text: "SE", alpha: 0.5 },
            WorldSide { az:  90.0 * PI / 180.0, text: "E",  alpha: 1.0 },
            WorldSide { az: 135.0 * PI / 180.0, text: "NE", alpha: 0.5 },
            WorldSide { az: 180.0 * PI / 180.0, text: "N",  alpha: 1.0 },
            WorldSide { az: 225.0 * PI / 180.0, text: "NW", alpha: 0.5 },
            WorldSide { az: 270.0 * PI / 180.0, text: "W",  alpha: 1.0 },
            WorldSide { az: 315.0 * PI / 180.0, text: "SW", alpha: 0.5 },
        ];
        cairo.set_font_size(6.0 * ctx.screen.dpmm_y);
        for world_side in world_sides {
            self.obj_painter.paint(
                &world_side,
                &eq_hor_cvt,
                &hor_3d_cvt,
                ctx,
                PainterStage::Names
            )?;
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
        let angle = ctx.config.horizon_glow.angle * PI / 180.0;

        cairo.set_antialias(gtk::cairo::Antialias::None);

        for i in 0..(360/STEP) {
            let az1 = PI * (STEP * i) as f64 / 180.0;
            let az2 = PI * (STEP * (i+1)) as f64 / 180.0;
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
                PainterStage::Objects
            )?;
        }

        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PainterStage {
    Objects,
    Names,
    ObjectsAndNames ,
    TestObjVsible,
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
    fn paint_object(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> anyhow::Result<()> { Ok(()) }
    fn paint_name(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> anyhow::Result<()> { Ok(()) }
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
        stage:      PainterStage,
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

        match stage {
            PainterStage::Objects =>
                obj.paint_object(ctx, &self.points_screen)?,
            PainterStage::Names =>
                obj.paint_name(ctx, &self.points_screen)?,
            PainterStage::ObjectsAndNames => {
                obj.paint_object(ctx, &self.points_screen)?;
                obj.paint_name(ctx, &self.points_screen)?;
            },
            PainterStage::TestObjVsible =>
                return Ok(true),
        }
        Ok(true)
    }
}

// Paint DSP item

impl ItemPainter for DsoItem {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(EqCoord {
            ra: self.crd.ra(),
            dec: self.crd.dec()
        })
    }

    fn paint_name(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let crd = &points[0];
        let mut y = crd.y;
        let text_height = ctx.cairo.font_extents()?.height();
        ctx.cairo.set_source_rgba(1.0, 1.0, 1.0, 0.7);
        for item in &self.names {
            ctx.cairo.move_to(crd.x, y);
            ctx.cairo.show_text(&item.name)?;
            y += text_height;
        }
        Ok(())
    }
}

// Paint ellipse around DSO object

struct DsoEllipse {
    points: Vec<EqCoord>,
    line_width: f64,
    dso_type: DsoType,
}

impl DsoEllipse {
    fn new() -> Self {
        Self {
            points: Vec::new(),
            line_width: 1.0,
            dso_type: DsoType::Galaxy
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.close_path();

        match self.dso_type {
            DsoType::StarCluster => {
                ctx.cairo.set_dash(&[3.0 * self.line_width], 0.0);
                ctx.cairo.set_line_width(3.0 * self.line_width);
                ctx.cairo.set_source_rgba(1.0, 1.0, 0.0, 0.8);
            },

            DsoType::Galaxy => {
                ctx.cairo.set_source_rgba(0.0, 1.0, 0.0, 0.8);
                ctx.cairo.set_line_width(self.line_width);
            },

            DsoType::PlanetaryNebula => {
                ctx.cairo.set_source_rgba(1.0, 0.0, 1.0, 0.8);
                ctx.cairo.set_line_width(self.line_width);
            },

            DsoType::DarkNebula |
            DsoType::EmissionNebula |
            DsoType::Nebula |
            DsoType::ReflectionNebula |
            DsoType::HIIIonizedRegion => {
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
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

    fn paint_name(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> anyhow::Result<()> {
        Ok(())
    }
}

// Paint star
struct StarPainter<'a> {
    data: &'a StarData,
    name: &'a str,
    max_size: f64,
    max_mag_value: f32,
    slow_grow_size: f64,
    light_size_k: f64,
    min_bright_size: f64,
}

type RgbTuple = (f64, f64, f64);

impl<'a> StarPainter<'a> {
    fn calc_light(&self, star_mag: f32) -> (f32, f32) {
        let light = f32::powf(2.0, 0.4 * (self.max_mag_value - star_mag)) - 1.0;
        let light_with_gamma = light.powf(0.7).min(1.0);
        (light, light_with_gamma)
    }

    fn calc_size(&self, light: f32) -> f64 {
        let mut size = (self.light_size_k * light as f64).max(1.0);

        if self.data.mag.get() < 1.0 {
            size = size.max(self.min_bright_size)
        }

        if size > self.slow_grow_size {
            size -= self.slow_grow_size;
            size /= 5.0;
            size += self.slow_grow_size;
        }
        if size > self.max_size {
            size = self.max_size;
        }
        size
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
}

impl<'a> ItemPainter for StarPainter<'a> {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(EqCoord {
            dec: self.data.crd.dec(),
            ra: self.data.crd.ra(),
        })
    }

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt = &points[0];
        let star_mag = self.data.mag.get();
        let (light, light_with_gamma) = self.calc_light(star_mag);
        if light_with_gamma < 0.1 { return Ok(()); }
        let (r, g, b) = Self::get_rgb_for_star_bv(self.data.bv.get());
        ctx.cairo.save()?;
        ctx.cairo.translate(pt.x, pt.y);
        let size = self.calc_size(light);
        if size <= 1.0 {
            ctx.cairo.set_source_rgb(
                size * light_with_gamma as f64 * r,
                size * light_with_gamma as f64 * g,
                size * light_with_gamma as f64 * b,
            );
            ctx.cairo.rectangle(-0.5, -0.5, 1.0, 1.0);
        } else if size <= ctx.screen.dpmm_x {
            ctx.cairo.set_source_rgb(
                light_with_gamma as f64 * r,
                light_with_gamma as f64 * g,
                light_with_gamma as f64 * b,
            );
            ctx.cairo.arc(0.0, 0.0, 0.5 * size, 0.0, 2.0 * PI);
        } else {
            let grad = cairo::RadialGradient::new(0.0, 0.0, 0.1 * size, 0.0, 0.0, 0.75 * size);
            grad.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 1.0);
            grad.add_color_stop_rgba(0.25, r, g, b, 1.0);
            grad.add_color_stop_rgba(1.0, r, g, b, 0.0);
            ctx.cairo.set_source(&grad)?;
            ctx.cairo.arc(0.0, 0.0, 0.75 * size, 0.0, 2.0 * PI);
        }
        ctx.cairo.fill()?;
        ctx.cairo.restore()?;
        Ok(())
    }

    fn paint_name(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        if !self.name.is_empty() {
            let star_mag = self.data.mag.get();
            let (light, mut light_with_gamma) = self.calc_light(star_mag);
            if light_with_gamma < 0.5 { return Ok(()); }
            light_with_gamma -= 0.5;
            light_with_gamma *= 2.0;
            let size = self.calc_size(light);
            let pt = &points[0];
            let te = ctx.cairo.text_extents(&self.name)?;
            let t_height = te.height();
            ctx.cairo.move_to(pt.x + 0.5 * size - 0.1 * t_height, pt.y + t_height + 0.5 * size - 0.1 * t_height);
            let (r, g, b) = Self::get_rgb_for_star_bv(self.data.bv.get());
            ctx.cairo.set_source_rgba(
                light_with_gamma as f64 * r,
                light_with_gamma as f64 * g,
                light_with_gamma as f64 * b,
                0.8,
            );
            ctx.cairo.show_text(self.name)?;
        }
        Ok(())
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
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
            az: self.az
        })
    }

    fn paint_object(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> anyhow::Result<()> {
        Ok(())
    }

    fn paint_name(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        ctx.cairo.arc(points[0].x, points[0].y, 4.0, 0.0, 2.0 * PI);
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        ctx.cairo.close_path();
        ctx.cairo.fill()?;
        Ok(())
    }
}
