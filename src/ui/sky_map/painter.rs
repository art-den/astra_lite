use std::{cell::RefCell, collections::HashSet, f64::consts::PI, rc::Rc};
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use gtk::{cairo, gdk_pixbuf, pango};
use itertools::{izip, Itertools};
use serde::{Deserialize, Serialize};
use crate::{utils::math::linear_interpolate, sky_math::math::*};
use super::{consts::*, data::*, perspective_painter::PerspectivePainter, utils::*};

const GRID_TEXT_FONT_SIZE: f64 = 0.8;
const WORD_SIZE_FONT_SIZE: f64 = 2.0;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[derive(Clone)]
pub struct ViewPoint {
    pub crd:        HorizCoord,
    pub mag_factor: f64, // magnification factor
}

impl ViewPoint {
    pub fn new() -> Self {
        let crd = HorizCoord {
            alt: degree_to_radian(21.31),
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
pub struct PlateSolvedImage {
    pub image: gdk_pixbuf::Pixbuf,
    pub coord: EqCoord,
    pub horiz_angle: f64,
    pub vert_angle: f64,
    pub rot_angle: f64,
    pub time: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HorizonGlowConfig {
    pub visible: bool,
    pub angle:   f64,
    pub color:   Color,
}

impl Default for HorizonGlowConfig {
    fn default() -> Self {
        Self {
            visible: true,
            angle:   10.0, // degrees
            color:   Color { r: 0.25, g: 0.3, b: 0.3, a: 1.0 },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EqGridConfig {
    pub visible:    bool,
    pub line_color: Color,
    pub text_color: Color,
}

impl Default for EqGridConfig {
    fn default() -> Self {
        Self {
            visible: true,
            line_color: Color { r: 0.0, g: 0.0, b: 0.7, a: 1.0 },
            text_color: Color { r: 0.6, g: 0.6, b: 0.6, a: 1.0 },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PaintConfig {
    pub high_quality: bool,
    pub filter:       ItemsToShow,
    pub max_dso_mag:  f32,
    pub horizon_glow: HorizonGlowConfig,
    pub eq_grid:      EqGridConfig,
}

impl Default for PaintConfig {
    fn default() -> Self {
        let mut result = Self {
            high_quality: true,
            filter:       ItemsToShow::all(),
            max_dso_mag:  10.0,
            horizon_glow: HorizonGlowConfig::default(),
            eq_grid:      EqGridConfig::default(),
        };

        if !cfg!(target_arch = "x86_64") {
            result.high_quality = false;
        }

        result
    }
}

impl PaintConfig {
    fn get_antialias(&self) -> gtk::cairo::Antialias {
        if self.high_quality {
            gtk::cairo::Antialias::Fast
        } else {
            gtk::cairo::Antialias::None
        }
    }
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

pub struct PaintArgs<'a> {
    pub sky_map:     &'a Option<Rc<SkyMap>>,
    pub selection:   &'a Option<SkymapObject>,
    pub tele_pos:    &'a Option<EqCoord>,
    pub cam_frame:   &'a Option<CameraFrame>,
    pub plate_solve: &'a Option<PlateSolvedImage>,
    pub observer:    &'a Observer,
    pub utc_time:    &'a NaiveDateTime,
    pub config:      &'a PaintConfig,
    pub view_point:  &'a ViewPoint,
    pub screen:      &'a Screen,
    pub cairo:       &'a gtk::cairo::Context,
    pub layout:      &'a pango::Layout,
}

pub struct SkyMapPainter {
    item_painter:  ItemPainter,
    dso_ellipse:   DsoEllipse,
    visible_zones: HashSet<SkyZoneKey>,
    persp_pnt:     RefCell<PerspectivePainter>,
}

impl SkyMapPainter {
    pub fn new() -> Self {
        Self {
            item_painter:  ItemPainter::new(),
            dso_ellipse:   DsoEllipse::new(),
            visible_zones: HashSet::new(),
            persp_pnt:     RefCell::new(PerspectivePainter::new()),
        }
    }

    pub fn paint(&mut self, args: PaintArgs) -> anyhow::Result<()> {
        let eq_sphere_cvt = EqToSphereCvt::new(
            args.observer.longitude,
            args.observer.latitude,
            args.utc_time,
        );

        let sphere_scr_cvt = SphereToScreenCvt::new(&args.view_point.crd);

        args.cairo.set_antialias(gtk::cairo::Antialias::None);
        args.cairo.set_source_rgb(0.0, 0.0, 0.0);
        args.cairo.paint()?;

        let pxls_per_rad = self.calc_pixels_per_radian(args.screen, args.view_point.mag_factor);

        let j2000 = j2000_time();
        let epoch_cvt = EpochCvt::new(&j2000, &args.utc_time);

        let ctx = PaintCtx {
            cairo: args.cairo,
            layout: args.layout,
            config: args.config,
            screen: args.screen,
            view_point: args.view_point,
            pxls_per_rad,
            epoch_cvt: &epoch_cvt,
            eq_sphere_cvt: &eq_sphere_cvt,
            sphere_scr_cvt: &sphere_scr_cvt,
        };

        // Optionally plate solved image
        self.paint_platesolved_image(args.plate_solve, &ctx)?;

        // Equatorial grid
        if args.config.eq_grid.visible {
            let mut font = ctx.layout.font_description().unwrap();
            font.set_size((GRID_TEXT_FONT_SIZE * ctx.screen.font_size()) as _);
            ctx.layout.set_font_description(Some(&font));

            self.paint_eq_grid(&ctx, false)?;
            self.paint_eq_grid(&ctx, true)?;
        }

        if let Some(sky_map) = args.sky_map {
            // DSO objects
            self.paint_dso_items(sky_map, &ctx, PainterMode::Objects)?;

            let star_painter_params = self.get_star_painter_params(&ctx);

            // Stars objects
            if args.config.filter.contains(ItemsToShow::STARS) {
                self.fill_visible_zones(&ctx);

                self.paint_stars(
                    sky_map,
                    &star_painter_params,
                    &ctx,
                    PainterMode::Objects
                )?;
            }

            // DSO names
            let mut font = ctx.layout.font_description().unwrap();
            font.set_size(ctx.screen.font_size() as _);
            ctx.layout.set_font_description(Some(&font));

            self.paint_dso_items(sky_map, &ctx, PainterMode::Names)?;

            // Stars names

            let mut font = ctx.layout.font_description().unwrap();
            font.set_size(ctx.screen.font_size() as _);
            ctx.layout.set_font_description(Some(&font));

            if args.config.filter.contains(ItemsToShow::STARS) {
                self.paint_stars(
                    sky_map,
                    &star_painter_params,
                    &ctx,
                    PainterMode::Names
                )?;
            }
        }

        // Horizon glow
        if args.config.horizon_glow.visible {
            self.paint_horizon_glow(&ctx)?;
        }

        // Ground

        let mut font = ctx.layout.font_description().unwrap();
        font.set_size((WORD_SIZE_FONT_SIZE * ctx.screen.font_size()) as _);
        ctx.layout.set_font_description(Some(&font));

        self.paint_ground(&ctx)?;

        // Selected object
        self.paint_selection(args.selection, &ctx)?;

        // Optionally telescope position
        self.paint_telescope_position(args.tele_pos, &ctx)?;

        // Optionally camera frame

        let mut font = ctx.layout.font_description().unwrap();
        font.set_size(ctx.screen.font_size() as _);
        ctx.layout.set_font_description(Some(&font));

        self.paint_camera_frame(args.cam_frame, &ctx)?;

        Ok(())
    }

    fn fill_visible_zones(&mut self, ctx: &PaintCtx) {
        self.visible_zones.clear();

        let center_crd = ctx.view_point.crd.to_sphere_pt();
        let center_eq_crd = ctx.eq_sphere_cvt.sphere_to_eq(&center_crd);
        let center_zone_key = SkyZoneKey::from_coord(center_eq_crd.ra, center_eq_crd.dec);
        self.visible_zones.insert(center_zone_key);
        for ra_key in 0..SkyZoneKey::RA_COUNT {
            for dec_key in 0..SkyZoneKey::DEC_COUNT {
                let key = SkyZoneKey::from_indices(ra_key as u16, dec_key as u16);
                if key == center_zone_key {
                    continue;
                }
                let vis_test_obj = ZoneVisibilityTestObject {
                    coords: key.to_coords(),
                };
                let is_visible = self.item_painter.paint(&vis_test_obj, ctx, false).unwrap_or_default();
                if is_visible {
                    self.visible_zones.insert(key);
                }
            }
        }
    }

    fn calc_pixels_per_radian(
        &self,
        screen:     &Screen,
        mag_factor: f64,
    ) -> f64 {
        const ANGLE_DIFF: f64 = 2.0 * PI / (360.0 * 60.0);
        let mut pt = Point3D { x: 0.0, y: 0.0, z: 1.0 };
        let crd1 = screen.sphere_to_screen(&pt, mag_factor);
        pt.rotate_over_x_mat(&RotMatrix::new(ANGLE_DIFF));
        let crd2 = screen.sphere_to_screen(&pt, mag_factor);
        Point2D::distance(&crd1, &crd2) / ANGLE_DIFF
    }

    fn paint_dso_items(
        &mut self,
        sky_map:    &SkyMap,
        ctx:        &PaintCtx,
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
                        coord: dso_object.crd.to_eq(),
                        use_now_epoch: true
                    };

                    let is_visible_on_screen = self.item_painter.paint(
                        &test_visiblity,
                        ctx,
                        false,
                    )?;

                    // Paint ellipse of object
                    if is_visible_on_screen {
                        self.paint_dso_ellipse(dso_object, ctx)?;
                    }
                }
                PainterMode::Names => {
                    let name_painter = DsoNamePainter(dso_object);
                    self.item_painter.paint(
                        &name_painter,
                        ctx,
                        false
                    )?;
                }
            }
        }

        Ok(())
    }

    fn paint_dso_ellipse(
        &mut self,
        dso_object: &DsoItem,
        ctx:        &PaintCtx,
    ) -> anyhow::Result<()> {
        let maj_axis = dso_object.maj_axis.unwrap_or_default();
        let min_axis = dso_object.min_axis.unwrap_or(maj_axis);
        let maj_axis = arcmin_to_radian(maj_axis as f64);
        let min_axis = arcmin_to_radian(min_axis as f64);

        let min_axis_value = 2.0 * ctx.screen.dpmm_x() / ctx.pxls_per_rad;
        let maj_axis = maj_axis.max(min_axis_value);
        let min_axis = min_axis.max(min_axis_value);

        let angle = dso_object.angle.unwrap_or_default() as f64 + PI * 0.5;
        let obj_dec = dso_object.crd.dec();
        let obj_ra = dso_object.crd.ra();
        let dec_rot = RotMatrix::new(0.5 * PI - obj_dec);
        let ra_rot = RotMatrix::new(PI / 2.0 -obj_ra);
        const ELLIPSE_PTS_COUNT: usize = 66;
        let a = 0.5 * maj_axis;
        let b = 0.5 * min_axis;
        self.dso_ellipse.points.clear();
        for i in 0..ELLIPSE_PTS_COUNT {
            let az = 2.0 * PI * i as f64 / ELLIPSE_PTS_COUNT as f64;
            let sin_az = f64::sin(az);
            let cos_az = f64::cos(az);
            let alt = a * b / f64::sqrt(a * a * sin_az * sin_az + b * b * cos_az * cos_az);
            let crd = EqCoord { dec: 0.5 * PI - alt, ra: az - angle };
            let mut pt = crd.to_sphere_pt();
            pt.rotate_over_x_mat(&dec_rot);
            pt.rotate_over_z_mat(&ra_rot);
            let crd = EqCoord::from_sphere_pt(&pt);
            self.dso_ellipse.points.push(crd);
        }
        let mut line_width = 0.01 * f64::max(maj_axis, min_axis) * ctx.pxls_per_rad;
        line_width = line_width.max(1.0);
        line_width = line_width.min(5.0 * ctx.screen.dpmm_x());

        self.dso_ellipse.line_width = line_width;
        self.dso_ellipse.dso_type = dso_object.obj_type;
        self.item_painter.paint(&self.dso_ellipse, ctx, false)?;

        Ok(())
    }

    fn get_star_painter_params(&self, ctx: &PaintCtx) -> StarPainterParams {
        let max_size = 7.0 * ctx.screen.dpmm_x();
        let slow_grow_size = 3.0 * ctx.screen.dpmm_x();
        let light_size_k = 0.3 * ctx.screen.dpmm_x();
        let min_bright_size = 1.5 * ctx.screen.dpmm_x();
        let max_mag_value = calc_max_star_magnitude_for_painting(ctx.view_point.mag_factor);

        StarPainterParams {
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
        options:    &StarPainterParams,
        ctx:        &PaintCtx,
        mode:       PainterMode,
    ) -> anyhow::Result<bool> {
        let star_painter = StarPainter {
            mode,
            star: star_data,
            name,
            bayer,
            options,
        };
        let star_is_painted = self.item_painter.paint(&star_painter, ctx, false)?;
        Ok(star_is_painted)
    }

    fn paint_stars(
        &mut self,
        sky_map: &SkyMap,
        params:  &StarPainterParams,
        ctx:     &PaintCtx,
        mode:    PainterMode,
    ) -> anyhow::Result<()> {
        ctx.cairo.set_antialias(ctx.config.get_antialias());

        let max_mag_value = calc_max_star_magnitude_for_painting(ctx.view_point.mag_factor);
        let max_mag = ObjMagnitude::new(max_mag_value);
        let stars = sky_map.stars();
        let mut _stars_count = 0_usize;
        let mut _stars_painted_count = 0_usize;
        for (zone_key, zone) in stars.zones() {
            if !self.visible_zones.contains(zone_key) {
                continue;
            }

            if mode == PainterMode::Objects {
                for star in zone.stars() {
                    if star.data.mag > max_mag {
                        continue;
                    }
                    let star_is_painted = self.paint_star(
                        &star.data, "", "",
                        params, ctx, mode,
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
                    params, ctx, mode,
                )?;
                _stars_count += 1;
                if star_is_painted { _stars_painted_count += 1; }
            }
        }

        Ok(())
    }

    fn paint_eq_grid(&mut self, ctx: &PaintCtx, text: bool) -> anyhow::Result<()> {
        if !text {
            ctx.cairo.set_line_width(1.0);
            ctx.cairo.set_antialias(ctx.config.get_antialias());
            let c = &ctx.config.eq_grid.line_color;
            ctx.cairo.set_source_rgba(c.r, c.g, c.b, c.a);
        }
        else
        {
            let c = &ctx.config.eq_grid.text_color;
            ctx.cairo.set_source_rgba(c.r, c.g, c.b, c.a);
        }

        ctx.layout.set_text("#");
        let text_offset = 0.5 * ctx.layout.pixel_size().1 as f64;

        const DEC_STEP: i32 = 10; // degree
        for i in -90/DEC_STEP..90/DEC_STEP {
            let dec1 = degree_to_radian((DEC_STEP * i) as f64);
            let dec2 = degree_to_radian((DEC_STEP * (i + 1)) as f64);
            for j in 0..24 {
                let ra = hour_to_radian(j as f64);
                let item = EqGridItem {
                    tp: EqGridItemType::Ra,
                    ra1: ra,
                    ra2: ra,
                    dec1, dec2,
                    text,
                    text_offset,
                };
                self.item_painter.paint(&item, ctx, false)?;
            }
        }
        for j in 0..24 {
            let ra1 = hour_to_radian(j as f64);
            let ra2 = hour_to_radian((j + 1) as f64);
            for i in -90/DEC_STEP..90/DEC_STEP {
                let dec = degree_to_radian((DEC_STEP * i) as f64);
                let item = EqGridItem {
                    tp: EqGridItemType::Dec,
                    dec1: dec,
                    dec2: dec,
                    ra1, ra2,
                    text,
                    text_offset,
                };
                self.item_painter.paint(&item, ctx, false)?;
            }
        }

        if !text {
            ctx.cairo.stroke()?;
        }

        Ok(())
    }

    fn paint_ground(&mut self, ctx: &PaintCtx) -> anyhow::Result<()> {
        let ground = Ground { view_point: ctx.view_point };
        self.item_painter.paint(&ground, ctx, false)?;
        let world_sides = [
            WorldSide { az:   0.0, text: "N",  alpha: 1.0 },
            WorldSide { az:  45.0, text: "NE", alpha: 0.5 },
            WorldSide { az:  90.0, text: "E",  alpha: 1.0 },
            WorldSide { az: 135.0, text: "SE", alpha: 0.5 },
            WorldSide { az: 180.0, text: "S",  alpha: 1.0 },
            WorldSide { az: 225.0, text: "SW", alpha: 0.5 },
            WorldSide { az: 270.0, text: "W",  alpha: 1.0 },
            WorldSide { az: 315.0, text: "NW", alpha: 0.5 },
        ];
        for world_side in world_sides {
            self.item_painter.paint(&world_side, ctx, false)?;
        }
        Ok(())
    }

    fn paint_horizon_glow(&mut self, ctx: &PaintCtx) -> anyhow::Result<()> {
        const STEP: i32 = 2;
        let angle = degree_to_radian(ctx.config.horizon_glow.angle);

        ctx.cairo.set_antialias(gtk::cairo::Antialias::None);

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
            self.item_painter.paint(&item, ctx, false)?;
        }

        Ok(())
    }

    fn paint_selection(
        &mut self,
        selection: &Option<SkymapObject>,
        ctx:       &PaintCtx,
    ) -> anyhow::Result<()> {
        let Some(selection) = selection else { return Ok(()); };
        let size = 12.0 * ctx.screen.dpmm_x();
        let thickness = 1.0 * ctx.screen.dpmm_x();
        let crd = selection.crd();
        let selection_painter = SelectionPainter { crd, size, thickness };
        self.item_painter.paint(&selection_painter, ctx, true)?;
        Ok(())
    }

    fn paint_telescope_position(
        &mut self,
        tele_pos:   &Option<EqCoord>,
        ctx:        &PaintCtx,
    ) -> anyhow::Result<()> {
        let Some(telescope_pos) = tele_pos else { return Ok(()); };
        let painter = TelescopePosPainter {
            crd: *telescope_pos,
        };
        self.item_painter.paint(&painter, ctx, true)?;
        Ok(())
    }

    fn calc_rect_coords(
        center_crd: &EqCoord,
        horiz_angle: f64,
        vert_angle: f64,
        rot_angle: f64,
    ) -> [EqCoord; 4] {
        let dec_rot = RotMatrix::new(0.5 * PI - center_crd.dec);
        let ra_rot = RotMatrix::new(PI / 2.0 - center_crd.ra);

        let h = 0.5 * horiz_angle;
        let v = 0.5 * vert_angle;
        let len = f64::sqrt(h * h + v * v);

        let angle = f64::atan2(vert_angle, horiz_angle);
        let angles = [ angle, PI-angle, PI + angle, 2.0*PI - angle ];
        let mut coords = [EqCoord {dec: 0.0, ra: 0.0}; 4];
        for (a, crd) in izip!(angles, &mut coords) {
            let eq_crd = EqCoord { dec: 0.5 * PI - len, ra: a - rot_angle };
            let mut pt = eq_crd.to_sphere_pt();
            pt.rotate_over_x_mat(&dec_rot);
            pt.rotate_over_z_mat(&ra_rot);
            *crd = EqCoord::from_sphere_pt(&pt);
        }
        coords
    }

    fn paint_camera_frame(
        &mut self,
        cam_frame: &Option<CameraFrame>,
        ctx:       &PaintCtx,
    ) -> anyhow::Result<()> {
        if let Some(cam_frame) = cam_frame {
            let center_crd = ctx.view_point.crd.to_sphere_pt();
            let center_crd = ctx.eq_sphere_cvt.sphere_to_eq(&center_crd);

            let coords = Self::calc_rect_coords(
                &center_crd,
                cam_frame.horiz_angle,
                cam_frame.vert_angle,
                cam_frame.rot_angle
            );

            let painter = CameraFramePainter { name: &cam_frame.name, coords };
            self.item_painter.paint(&painter, ctx, false)?;
        }

        Ok(())
    }

    fn paint_platesolved_image(
        &mut self,
        ps_image: &Option<PlateSolvedImage>,
        ctx:      &PaintCtx,
    ) -> anyhow::Result<()> {
        let Some(ps_image) = ps_image else { return Ok(()); };
        let coords = Self::calc_rect_coords(
            &ps_image.coord,
            ps_image.horiz_angle,
            ps_image.vert_angle,
            ps_image.rot_angle
        );
        let painter = PlateSolvedImagePainter {
            image: ps_image.image.clone(),
            time: ps_image.time,
            coords,
            persp_pnt: &self.persp_pnt,
        };
        self.item_painter.paint(&painter, ctx, false)?;
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
    cairo:          &'a gtk::cairo::Context,
    layout:         &'a pango::Layout,
    config:         &'a PaintConfig,
    screen:         &'a Screen,
    view_point:     &'a ViewPoint,
    pxls_per_rad:   f64,
    epoch_cvt:      &'a EpochCvt,
    eq_sphere_cvt:  &'a EqToSphereCvt,
    sphere_scr_cvt: &'a SphereToScreenCvt,
}

trait Item {
    fn use_now_epoch(&self) -> bool { false }
    fn points_count(&self) -> usize;
    fn point_crd(&self, index: usize) -> PainterCrd;
    fn paint(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> anyhow::Result<()> { Ok(()) }
}

struct ItemPainter {
    points_3d:     Vec<Point3D>,
    points_screen: Vec<Point2D>,
}

impl ItemPainter {
    fn new() -> Self {
        Self {
            points_3d:     Vec::new(),
            points_screen: Vec::new(),
        }
    }

    fn paint(
        &mut self,
        obj:         &dyn Item,
        ctx:         &PaintCtx,
        under_horiz: bool,
    ) -> anyhow::Result<bool> {
        let points_count = obj.points_count();
        let use_now_epoch = obj.use_now_epoch();

        self.points_3d.clear();
        let mut obj_is_visible = false;
        for i in 0..points_count {
            let mut invisible = false;

            let crd_sphere = match obj.point_crd(i) {
                PainterCrd::Horiz(horiz) => horiz.to_sphere_pt(),
                PainterCrd::Eq(eq) => {
                    let mut sphere_pt = eq.to_sphere_pt();
                    if use_now_epoch {
                        sphere_pt = ctx.epoch_cvt.convert_pt(&sphere_pt);
                    }
                    ctx.eq_sphere_cvt.apply(&mut sphere_pt);
                    sphere_pt
                }
            };

            invisible |= !under_horiz && crd_sphere.x < 0.0;

            let crd_vp = ctx.sphere_scr_cvt.apply_viewpoint(&crd_sphere);

            invisible |= crd_vp.z < -0.5;

            if !invisible {
                obj_is_visible = true;
            }

            self.points_3d.push(crd_vp);
        }
        if !obj_is_visible {
            return Ok(false);
        }

        let scr_tolerance = ctx.screen.tolerance();

        // 3d coordinates -> screen coordinates
        self.points_screen.clear();
        obj_is_visible = false;
        for pt in &self.points_3d {
            let pt_s = ctx.screen.sphere_to_screen(pt, ctx.view_point.mag_factor);
            if !obj_is_visible
            && scr_tolerance.left < pt_s.x && pt_s.x < scr_tolerance.right
            && scr_tolerance.top < pt_s.y && pt_s.y < scr_tolerance.bottom {
                obj_is_visible = true;
            }
            self.points_screen.push(pt_s);
        }

        // check if 2d lines is crossing by screen boundaries
        if !obj_is_visible && self.points_screen.len() >= 2 {
            let top_line = scr_tolerance.top_line();
            let bottom_line = scr_tolerance.bottom_line();
            let left_line = scr_tolerance.left_line();
            let right_line = scr_tolerance.right_line();
            obj_is_visible =
                self.points_screen
                    .iter()
                    .circular_tuple_windows()
                    .any(|(&crd1, &crd2)| {
                        let line = Line2D { pt1: crd1, pt2: crd2 };
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

impl<'a> Item for DsoNamePainter<'a> {
    fn use_now_epoch(&self) -> bool {
        true
    }

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
        ctx.cairo.set_source_rgba(1.0, 1.0, 1.0, 0.7);
        for item in &self.0.names {
            ctx.layout.set_text(item.text());
            ctx.cairo.move_to(crd.x, y);
            pangocairo::show_layout(ctx.cairo, ctx.layout);
            let (_, height) = ctx.layout.pixel_size();
            y += 0.8 * height as f64;
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

impl Item for DsoEllipse {
    fn use_now_epoch(&self) -> bool {
        true
    }

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

        ctx.cairo.set_antialias(ctx.config.get_antialias());
        ctx.cairo.stroke()?;

        ctx.cairo.set_dash(&[], 0.0);

        Ok(())
    }
}

// Paint outline

impl Item for Outline {
    fn use_now_epoch(&self) -> bool {
        true
    }

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
        ctx.cairo.set_antialias(ctx.config.get_antialias());
        ctx.cairo.stroke()?;
        Ok(())
    }
}

// Paint star

struct StarPainterParams {
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
    options: &'a StarPainterParams,
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
        let star_mag = self.star.mag.get();
        let (light, light_with_gamma) = self.calc_light(star_mag);
        if light_with_gamma < 0.1 { return Ok(()); }
        let (r, g, b) = Self::get_rgb_for_star_bv(self.star.bv.get());
        let pt = &points[0];
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
        } else if diam <= ctx.screen.dpmm_x() {
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
        let mut pt = points[0];
        let mut paint_text = |text, light_with_gamma| -> anyhow::Result<()> {
            let mut light_with_gamma = light_with_gamma;
            if light_with_gamma < 0.5 { return Ok(()); }
            light_with_gamma -= 0.5;
            light_with_gamma *= 2.0;

            ctx.layout.set_text(text);
            let (_, height) = ctx.layout.pixel_size();
            let t_height = height as f64;

            ctx.cairo.set_source_rgba(
                r, g, b,
                light_with_gamma as f64,
            );
            ctx.cairo.move_to(
                pt.x + 0.5 * diam - 0.1 * t_height,
                pt.y + 0.5 * diam - t_height
            );
            pangocairo::show_layout(ctx.cairo, ctx.layout);
            pt.y += 0.8 * t_height;
            Ok(())
        };

        if !self.name.is_empty() {
            paint_text(self.name, light_with_gamma)?;
        }

        if !self.bayer.is_empty() {
            paint_text(&self.bayer, 0.5 * light_with_gamma)?;
        }

        Ok(())
    }
}

impl<'a> Item for StarPainter<'a> {
    fn use_now_epoch(&self) -> bool {
        true
    }

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

enum EqGridItemType { Ra, Dec }

struct EqGridItem {
    tp:   EqGridItemType,
    dec1: f64,
    dec2: f64,
    ra1:  f64,
    ra2:  f64,
    text: bool,
    text_offset: f64,
}

impl EqGridItem {
    const POINTS_CNT: usize = 5;
}

impl Item for EqGridItem {
    fn points_count(&self) -> usize {
        Self::POINTS_CNT
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        match self.tp {
            EqGridItemType::Ra => PainterCrd::Eq(EqCoord{
                ra: self.ra1,
                dec: linear_interpolate(
                    index as f64,
                    0.0,
                    (Self::POINTS_CNT-1) as f64,
                    self.dec1,
                    self.dec2
                )
            }),
            EqGridItemType::Dec => PainterCrd::Eq(EqCoord{
                ra: linear_interpolate(
                    index as f64,
                    0.0,
                    (Self::POINTS_CNT-1) as f64,
                    self.ra1,
                    self.ra2
                ),
                dec: self.dec1
            }),
        }
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        if !self.text {
            let mut first = true;
            for pt in points {
                if first {
                    ctx.cairo.move_to(pt.x, pt.y);
                    first = false;
                } else {
                    ctx.cairo.line_to(pt.x, pt.y);
                }
            }
        } else {
            let scr_rect = ctx.screen.rect();
            let screen_left = scr_rect.left_line();
            let screen_right = scr_rect.right_line();
            let screen_top = scr_rect.top_line();
            let screen_bottom = scr_rect.bottom_line();
            for (pt1, pt2) in points.iter().tuple_windows() {
                let line = Line2D { pt1: pt1.clone(), pt2: pt2.clone() };
                let paint_text = |mut x, y, adjust_right| -> anyhow::Result<()> {
                    let text = match  self.tp {
                        EqGridItemType::Ra => format!("{:.0}h", radian_to_hour(self.ra1)),
                        EqGridItemType::Dec => format!("{:.0}°", radian_to_degree(self.dec1)),
                    };
                    ctx.layout.set_text(&text);
                    let (width, height) = ctx.layout.pixel_size();
                    if adjust_right {
                        x -= width as f64;
                    }
                    ctx.cairo.move_to(x, y - 0.5 * height as f64);
                    pangocairo::show_layout(ctx.cairo, ctx.layout);
                    Ok(())
                };

                if let Some(is) = Line2D::intersection(&line, &screen_top) {
                    paint_text(is.x, is.y + self.text_offset, false)?;
                } else if let Some(is) = Line2D::intersection(&line, &screen_bottom) {
                    paint_text(is.x, is.y - self.text_offset, false)?;
                } else if let Some(is) = Line2D::intersection(&line, &screen_left) {
                    paint_text(is.x + 0.5 * self.text_offset, is.y, false)?;
                } else if let Some(is) = Line2D::intersection(&line, &screen_right) {
                    paint_text(is.x - 0.5 * self.text_offset, is.y, true)?;
                }
            }
        }

        Ok(())
    }
}

// Paint ground

struct Ground<'a> {
    view_point: &'a ViewPoint,
}

impl<'a> Ground<'a> {
    const ANGLE_STEP: usize = 5;
}


impl<'a> Item for Ground<'a> {
    fn points_count(&self) -> usize {
        360 / Self::ANGLE_STEP
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Horiz(HorizCoord {
            alt: 0.0,
            az: PI * (index * Self::ANGLE_STEP) as f64 / 180.0
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

            let scr_rect = ctx.screen.rect();
            if min_x > scr_rect.left {
                min_x = scr_rect.left;
            }
            if min_y > scr_rect.top {
                min_y = scr_rect.top;
            }
            if max_x < scr_rect.right {
                max_x = scr_rect.right;
            }
            if max_y < scr_rect.bottom {
                max_y = scr_rect.bottom;
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

impl<'a> Item for WorldSide<'a> {
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
        ctx.layout.set_text(&self.text);
        let (width, height) = ctx.layout.pixel_size();
        ctx.cairo.move_to(
            points[0].x - 0.5 * width as f64,
            points[0].y - 0.5 * height as f64
        );
        ctx.cairo.set_source_rgba(0.8, 0.0, 0.0, self.alpha);
        pangocairo::show_layout(ctx.cairo, ctx.layout);
        Ok(())
    }
}

// Paint horizon glow

struct HorizonGlowItem {
    coords: [HorizCoord; 4],
}

impl Item for HorizonGlowItem {
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
        let color = &ctx.config.horizon_glow.color;
        gradient.add_color_stop_rgba(0.0, color.r, color.g, color.b, 0.0);
        gradient.add_color_stop_rgba(1.0, color.r, color.g, color.b, color.a);
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

impl Item for ZoneVisibilityTestObject {
    fn use_now_epoch(&self) -> bool {
        true
    }

    fn points_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Eq(self.coords[index].clone())
    }
}

struct PointVisibilityTestObject {
    coord: EqCoord,
    use_now_epoch: bool,
}

impl Item for PointVisibilityTestObject {
    fn use_now_epoch(&self) -> bool {
        self.use_now_epoch
    }

    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(self.coord.clone())
    }
}

struct TelescopePosPainter {
    crd: EqCoord,
}

impl Item for TelescopePosPainter {
    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(self.crd.clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt = &points[0];
        let line_size = 40.0 * ctx.screen.dpmm_x();
        ctx.cairo.set_line_width(1.0);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_antialias(ctx.config.get_antialias());
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

impl Item for SelectionPainter {
    fn use_now_epoch(&self) -> bool {
        true
    }

    fn points_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> PainterCrd {
        PainterCrd::Eq(self.crd.clone())
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let pt = &points[0];
        ctx.cairo.set_antialias(ctx.config.get_antialias());
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

 impl<'a> Item for CameraFramePainter<'a> {
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
        ctx.cairo.set_antialias(ctx.config.get_antialias());
        ctx.cairo.close_path();
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.1, 1.0));
        ctx.cairo.stroke()?;

        let pt1 = &points[0];
        let pt2 = &points[1];
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);

        paint_text_under_line(ctx.cairo, ctx.layout, pt1, pt2, self.name)?;

        Ok(())
    }
}

fn paint_text_under_line(
    cairo:  &cairo::Context,
    layout: &pango::Layout,
    pt1:    &Point2D,
    pt2:    &Point2D,
    text:   &str
) -> anyhow::Result<()> {
    let dx = pt2.x - pt1.x;
    let dy = pt2.y - pt1.y;
    let len = f64::sqrt(dx * dx + dy * dy);
    layout.set_text(text);
    let (width, height) = layout.pixel_size();
    if width as f64 <= len {
        let angle = f64::atan2(dy, dx);
        cairo.move_to(pt1.x, pt1.y);
        cairo.save()?;
        cairo.rotate(angle);
        cairo.rel_move_to(0.0, -height as f64);
        pangocairo::show_layout(cairo, layout);
        cairo.restore()?;
    }

    Ok(())
}

struct PlateSolvedImagePainter<'a> {
    image:     gdk_pixbuf::Pixbuf,
    time:      DateTime<Utc>,
    coords:    [EqCoord; 4],
    persp_pnt: &'a RefCell<PerspectivePainter>,
}

impl<'a> Item for PlateSolvedImagePainter<'a> {
    fn points_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> PainterCrd {
        PainterCrd::Eq(self.coords[index])
    }

    fn paint(&self, ctx: &PaintCtx, points: &[Point2D]) -> anyhow::Result<()> {
        let mut persp_pnt = self.persp_pnt.borrow_mut();
        persp_pnt.paint(
            &ctx.cairo,
            &self.image,
            points[0].x as i32, points[0].y as i32,
            points[1].x as i32, points[1].y as i32,
            points[2].x as i32, points[2].y as i32,
            points[3].x as i32, points[3].y as i32,
        )?;

        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }

        ctx.cairo.set_antialias(ctx.config.get_antialias());
        ctx.cairo.close_path();
        ctx.cairo.set_source_rgb(0.5, 0.5, 0.5);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.1, 1.0));
        ctx.cairo.stroke()?;

        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        let pt1 = &points[0];
        let pt2 = &points[1];
        let local_time: DateTime<Local> = DateTime::from(self.time);
        let text = format!(
            "Plate solve {}",
            local_time.format("%Y-%m-%d %H:%M:%S").to_string()
        );
        paint_text_under_line(ctx.cairo, ctx.layout, pt1, pt2, &text)?;

        Ok(())
    }
}