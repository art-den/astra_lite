use std::{cell::RefCell, collections::HashSet, f64::consts::PI, rc::Rc};
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use gtk::{cairo, gdk_pixbuf, pango};
use itertools::{izip, Itertools};
use serde::{Deserialize, Serialize};
use crate::{sky_math::math::*, utils::math::{angles_mean, linear_interpolate, point_in_polygon}};
use super::{consts::*, data::*, perspective_painter::PerspectivePainter, utils::*};

const GRID_TEXT_FONT_SCALE: f64 = 0.8;
const COMPASS_FONT_SCALE: f64 = 2.0;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Rgba {
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
pub struct CameraFov {
    pub name: String,
    pub horiz_fov: f64,
    pub vert_fov: f64,
    pub rot_angle: f64,
}

#[derive(Clone)]
pub struct PlateSolvedImage {
    pub image:      gdk_pixbuf::Pixbuf,
    pub center_crd: EqCoord,
    pub horiz_fov:  f64,
    pub vert_fov:   f64,
    pub rotation:   f64,
    pub time:       DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HorizonGlowConfig {
    pub visible:    bool,
    pub height_deg: f64,
    pub color:      Rgba,
}

impl Default for HorizonGlowConfig {
    fn default() -> Self {
        Self {
            visible:    true,
            height_deg: 10.0, // degrees
            color:      Rgba { r: 0.25, g: 0.3, b: 0.3, a: 1.0 },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EqGridConfig {
    pub visible:    bool,
    pub line_color: Rgba,
    pub text_color: Rgba,
}

impl Default for EqGridConfig {
    fn default() -> Self {
        Self {
            visible: true,
            line_color: Rgba { r: 0.0, g: 0.0, b: 0.7, a: 1.0 },
            text_color: Rgba { r: 0.6, g: 0.6, b: 0.6, a: 1.0 },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PaintConfig {
    pub high_quality: bool,
    pub items_filter: ItemsToShow,
    pub max_dso_mag:  f32,
    pub horizon_glow: HorizonGlowConfig,
    pub eq_grid:      EqGridConfig,
}

impl Default for PaintConfig {
    fn default() -> Self {
        let mut result = Self {
            high_quality: true,
            items_filter: ItemsToShow::all(),
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
    fn antialias(&self) -> gtk::cairo::Antialias {
        if self.high_quality {
            gtk::cairo::Antialias::Fast
        } else {
            gtk::cairo::Antialias::None
        }
    }
}

pub fn max_painted_star_mag(mag_factor: f64) -> f32 {
    linear_interpolate(
        mag_factor.log10(),
        MIN_MAG_FACTOR.log10(),
        MAX_MAG_FACTOR.log10(),
        4.0,
        20.0,
    ) as f32
}

pub struct PaintRequest<'a> {
    pub sky_map:     &'a Option<Rc<SkyMap>>,
    pub selection:   &'a Option<SkymapObject>,
    pub tele_pos:    &'a Option<EqCoord>,
    pub cam_frame:   &'a Option<CameraFov>,
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
    item_painter:         ItemPainter,
    dso_outline_scratch:  DsoEllipse,
    visible_zones:        HashSet<SkyZoneKey>, // for stars
    persp_painter:        RefCell<PerspectivePainter>,
    constellation_bounds: ConstellationBoundsPainter,
    horizon_glow:         HorizonGlowPainter,
}

impl SkyMapPainter {
    pub fn new() -> Self {
        Self {
            item_painter:         ItemPainter::new(),
            dso_outline_scratch:  DsoEllipse::new(),
            visible_zones:        HashSet::new(),
            persp_painter:        RefCell::new(PerspectivePainter::new()),
            constellation_bounds: ConstellationBoundsPainter::new(),
            horizon_glow:         HorizonGlowPainter::new(),
        }
    }

    pub fn paint(&mut self, request: PaintRequest) -> eyre::Result<()> {
        let eq_sphere_cvt = EqToSphereCvt::new(
            request.observer.longitude,
            request.observer.latitude,
            request.utc_time,
        );

        let sphere_scr_cvt = SphereToScreenCvt::new(&request.view_point.crd);

        request.cairo.set_antialias(gtk::cairo::Antialias::None);
        request.cairo.set_source_rgb(0.0, 0.0, 0.0);
        request.cairo.paint()?;

        let pxls_per_rad = self.pxls_per_rad_at_view_center(request.screen, request.view_point.mag_factor);

        let j2000 = j2000_time();
        let epoch_cvt = EpochCvt::new(&j2000, request.utc_time);

        let ctx = PaintCtx {
            cairo: request.cairo,
            layout: request.layout,
            config: request.config,
            screen: request.screen,
            view_point: request.view_point,
            pxls_per_rad,
            epoch_cvt: &epoch_cvt,
            eq_sphere_cvt: &eq_sphere_cvt,
            sphere_scr_cvt: &sphere_scr_cvt,
        };

        // Optionally plate solved image
        self.paint_plate_solved_image(request.plate_solve, &ctx)?;

        // Equatorial grid

        if request.config.eq_grid.visible {
            let mut font = ctx.layout.font_description().unwrap();
            font.set_size((GRID_TEXT_FONT_SCALE * ctx.screen.font_size()) as _);
            ctx.layout.set_font_description(Some(&font));

            self.paint_eq_grid(&ctx, false)?;
            self.paint_eq_grid(&ctx, true)?;
        }

        if let Some(sky_map) = request.sky_map {
            // Current constellation bounds

            if request.config.items_filter.contains(ItemsToShow::CONSTS) {
                self.constellation_bounds.paint(&mut self.item_painter, sky_map, &ctx)?;
            }

            // DSO objects

            self.paint_dso_items(sky_map, &ctx, PaintPass::Objects)?;

            // Constellations

            if request.config.items_filter.contains(ItemsToShow::CONSTS) {
                self.paint_constellation_lines(sky_map, &ctx)?;
            }

            // Star objects

            let star_painter_params = self.star_paint_params(&ctx);
            if request.config.items_filter.contains(ItemsToShow::STARS) {
                self.fill_visible_zones(&ctx);

                self.paint_stars(
                    sky_map,
                    &star_painter_params,
                    &ctx,
                    PaintPass::Objects
                )?;
            }

            // Constellation names

            if request.config.items_filter.contains(ItemsToShow::CONSTS) {
                self.paint_constellation_names(sky_map, &ctx)?;
            }

            // DSO names

            self.paint_dso_items(sky_map, &ctx, PaintPass::Names)?;

            // Star names

            if request.config.items_filter.contains(ItemsToShow::STARS) {
                self.paint_stars(
                    sky_map,
                    &star_painter_params,
                    &ctx,
                    PaintPass::Names
                )?;
            }
        }

        // Horizon glow

        if request.config.horizon_glow.visible {
            self.horizon_glow.paint(&mut self.item_painter, &ctx)?;
        }

        // Ground

        self.paint_ground(&ctx)?;

        // Selected object

        self.paint_selection(request.selection, &ctx)?;

        // Optionally telescope position

        self.paint_telescope_position(request.tele_pos, &ctx)?;

        // Optionally camera frame

        let mut font = ctx.layout.font_description().unwrap();
        font.set_size(ctx.screen.font_size() as _);
        ctx.layout.set_font_description(Some(&font));

        self.paint_camera_frame(request.cam_frame, &ctx)?;

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
                let probe = ZoneCullingProbe {
                    coords: key.to_coords(),
                };
                let is_visible = self.item_painter.paint_if_visible(&probe, ctx, false).unwrap_or_default();
                if is_visible {
                    self.visible_zones.insert(key);
                }
            }
        }
    }

    fn pxls_per_rad_at_view_center(
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
        sky_map: &SkyMap,
        ctx:     &PaintCtx,
        pass:    PaintPass,
    ) -> eyre::Result<()> {
        if pass == PaintPass::Names {
            let mut font = ctx.layout.font_description().unwrap();
            font.set_size(ctx.screen.font_size() as _);
            ctx.layout.set_font_description(Some(&font));
        }

        for dso_object in sky_map.objects() {
            let Some(mag) = dso_object.any_magnitude() else {
                continue;
            };
            if mag.get() > ctx.config.max_dso_mag {
                continue;
            }
            let visible = dso_object.obj_type.test_filter_flag(&ctx.config.items_filter);
            if !visible { continue; }

            match pass {
                PaintPass::Objects => {
                    let probe = PointCullingProbe {
                        coord: dso_object.crd.to_eq(),
                        use_now_epoch: true
                    };

                    let is_visible_on_screen = self.item_painter.paint_if_visible(
                        &probe,
                        ctx,
                        false,
                    )?;

                    // Paint ellipse around object
                    if is_visible_on_screen {
                        self.paint_dso_ellipse(dso_object, ctx)?;
                    }
                }
                PaintPass::Names => {
                    let name_painter = DsoNamePainter(dso_object);
                    self.item_painter.paint_if_visible(
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
    ) -> eyre::Result<()> {
        let maj_axis = dso_object.maj_axis.unwrap_or_default();
        let min_axis = dso_object.min_axis.unwrap_or(maj_axis);
        let maj_axis = arcmin_to_radian(maj_axis as f64);
        let min_axis = arcmin_to_radian(min_axis as f64);

        let min_axis_value = 2.0 * ctx.screen.dpmm_x() / ctx.pxls_per_rad;
        let maj_axis = maj_axis.max(min_axis_value);
        let min_axis = min_axis.max(min_axis_value);

        // +PI/2 lands the ellipse major axis on the catalog position angle
        let angle = dso_object.angle.unwrap_or_default() as f64 + PI * 0.5;

        let frame = EqLocalFrame::new(
            dso_object.crd.ra(),
            dso_object.crd.dec(),
            ctx.epoch_cvt,
            ctx.eq_sphere_cvt,
        );

        self.dso_outline_scratch.dso_type = dso_object.obj_type;
        self.dso_outline_scratch.set_shape(
            &frame,
            maj_axis,
            min_axis,
            angle,
            ctx.pxls_per_rad,
            5.0 * ctx.screen.dpmm_x(),
        );

        self.item_painter.paint_if_visible(&self.dso_outline_scratch, ctx, false)?;

        Ok(())
    }

    fn star_paint_params(&self, ctx: &PaintCtx) -> StarPainterParams {
        let max_diam = 7.0 * ctx.screen.dpmm_x();
        let grow_slow_thresh = 3.0 * ctx.screen.dpmm_x();
        let diam_per_light = 0.3 * ctx.screen.dpmm_x();
        let bright_min_diam = 1.5 * ctx.screen.dpmm_x();
        let max_mag_value = max_painted_star_mag(ctx.view_point.mag_factor);

        StarPainterParams {
            max_mag_value,
            max_diam,
            grow_slow_thresh,
            diam_per_light,
            bright_min_diam,
        }
    }

    fn paint_constellation_lines(
        &mut self,
        sky_map: &SkyMap,
        ctx:     &PaintCtx,
    ) -> eyre::Result<()> {
        let constellations = sky_map.constellations();
        // Two style buckets: common lines first, current constellation on top
        for is_current in [false, true] {
            ctx.cairo.set_antialias(ctx.config.antialias());
            ctx.cairo.set_dash(&[], 0.0);
            if is_current {
                ctx.cairo.set_source_rgba(0.3, 0.6, 0.3, 0.5);
                ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.7, 1.0));
            } else {
                ctx.cairo.set_source_rgba(0.45, 0.45, 1.0, 0.5);
                ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.5, 1.0));
            }
            let mut any = false;
            for (id, constellation) in constellations {
                if (Some(*id) == self.constellation_bounds.center_constellation_id) != is_current {
                    continue;
                }
                for polyline in &constellation.lines {
                    let Some(points) = self.item_painter.project_eq_polyline(polyline, ctx) else {
                        continue;
                    };
                    ctx.cairo.move_to(points[0].x, points[0].y);
                    for pt in &points[1..] {
                        ctx.cairo.line_to(pt.x, pt.y);
                    }
                    any = true;
                }
            }
            if any {
                ctx.cairo.stroke()?;
            }
        }
        Ok(())
    }

    fn paint_constellation_names(
        &mut self,
        sky_map: &SkyMap,
        ctx:     &PaintCtx,
    ) -> eyre::Result<()> {
        let mut font = ctx.layout.font_description().unwrap();
        font.set_size((2.0 * ctx.screen.font_size()) as _);
        ctx.layout.set_font_description(Some(&font));

        let constellations = sky_map.constellations();
        for (id, constellation) in constellations {
            let all_ra = constellation.lines.iter().flat_map(|l| l.iter().map(|pt| pt.ra));
            let all_dec = constellation.lines.iter().flat_map(|l| l.iter().map(|pt| pt.dec));
            let avg_ra = angles_mean(all_ra);
            let avg_dec = angles_mean(all_dec);
            let painter = ConstellationNamePainter {
                ra: avg_ra,
                dec: avg_dec,
                name: &constellation.name,
                is_current: Some(*id) == self.constellation_bounds.center_constellation_id,
            };
            self.item_painter.paint_if_visible(&painter, ctx, false)?;
        }
        Ok(())
    }

    fn paint_star(
        &mut self,
        star_data: &StarData,
        name:      &str,
        bayer:     &str,
        params:    &StarPainterParams,
        ctx:       &PaintCtx,
        pass:      PaintPass,
    ) -> eyre::Result<bool> {
        let star_painter = StarPainter {
            pass,
            star: star_data,
            name,
            bayer,
            params,
        };
        let star_is_painted = self.item_painter.paint_if_visible(&star_painter, ctx, false)?;
        Ok(star_is_painted)
    }

    fn paint_stars(
        &mut self,
        sky_map: &SkyMap,
        params:  &StarPainterParams,
        ctx:     &PaintCtx,
        mode:    PaintPass,
    ) -> eyre::Result<()> {
        if mode == PaintPass::Names {
            let mut font = ctx.layout.font_description().unwrap();
            font.set_size(ctx.screen.font_size() as _);
            ctx.layout.set_font_description(Some(&font));
        }

        ctx.cairo.set_antialias(ctx.config.antialias());

        let max_mag_value = max_painted_star_mag(ctx.view_point.mag_factor);
        let max_mag = ObjMagnitude::new(max_mag_value);
        let stars = sky_map.stars();
        let mut _stars_count = 0_usize;
        let mut _stars_painted_count = 0_usize;
        for (zone_key, zone) in stars.zones() {
            if !self.visible_zones.contains(zone_key) {
                continue;
            }

            // Zones are sorted by ascending magnitude, so only a visible prefix
            // is walked; iterate it back to front to paint faint stars first
            if mode == PaintPass::Objects {
                let stars = zone.stars();
                let visible_count = stars.partition_point(|star| star.data.mag <= max_mag);
                for star in stars[..visible_count].iter().rev() {
                    let star_is_painted = self.paint_star(
                        &star.data, "", "",
                        params, ctx, mode,
                    )?;
                    _stars_count += 1;
                    if star_is_painted { _stars_painted_count += 1; }
                }
            }

            let nstars = zone.named_stars();
            let visible_count = nstars.partition_point(|star| star.data.mag <= max_mag);
            for star in nstars[..visible_count].iter().rev() {
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

    fn paint_eq_grid(&mut self, ctx: &PaintCtx, is_label: bool) -> eyre::Result<()> {
        if !is_label {
            ctx.cairo.set_line_width(1.0);
            ctx.cairo.set_antialias(ctx.config.antialias());
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
                    kind: EqGridItemKind::Ra,
                    ra1: ra,
                    ra2: ra,
                    dec1, dec2,
                    is_label,
                    text_offset,
                };
                self.item_painter.paint_if_visible(&item, ctx, false)?;
            }
        }
        for j in 0..24 {
            let ra1 = hour_to_radian(j as f64);
            let ra2 = hour_to_radian((j + 1) as f64);
            for i in -90/DEC_STEP..90/DEC_STEP {
                let dec = degree_to_radian((DEC_STEP * i) as f64);
                let item = EqGridItem {
                    kind: EqGridItemKind::Dec,
                    dec1: dec,
                    dec2: dec,
                    ra1, ra2,
                    is_label,
                    text_offset,
                };
                self.item_painter.paint_if_visible(&item, ctx, false)?;
            }
        }

        if !is_label {
            ctx.cairo.stroke()?;
        }

        Ok(())
    }

    fn paint_ground(&mut self, ctx: &PaintCtx) -> eyre::Result<()> {
        let mut font = ctx.layout.font_description().unwrap();
        font.set_size((COMPASS_FONT_SCALE * ctx.screen.font_size()) as _);
        ctx.layout.set_font_description(Some(&font));

        let ground = Ground { view_point: ctx.view_point };
        self.item_painter.paint_if_visible(&ground, ctx, false)?;
        let compass_labels = [
            CompassLabel { az:   0.0, text: "N",  alpha: 1.0 },
            CompassLabel { az:  45.0, text: "NE", alpha: 0.5 },
            CompassLabel { az:  90.0, text: "E",  alpha: 1.0 },
            CompassLabel { az: 135.0, text: "SE", alpha: 0.5 },
            CompassLabel { az: 180.0, text: "S",  alpha: 1.0 },
            CompassLabel { az: 225.0, text: "SW", alpha: 0.5 },
            CompassLabel { az: 270.0, text: "W",  alpha: 1.0 },
            CompassLabel { az: 315.0, text: "NW", alpha: 0.5 },
        ];
        for compass_label in compass_labels {
            self.item_painter.paint_if_visible(&compass_label, ctx, false)?;
        }
        Ok(())
    }

    fn paint_selection(
        &mut self,
        selection: &Option<SkymapObject>,
        ctx:       &PaintCtx,
    ) -> eyre::Result<()> {
        let Some(selection) = selection else { return Ok(()); };
        let size = 12.0 * ctx.screen.dpmm_x();
        let thickness = 1.0 * ctx.screen.dpmm_x();
        let crd = selection.crd();
        let selection_painter = SelectionPainter { crd, size, thickness };
        self.item_painter.paint_if_visible(&selection_painter, ctx, true)?;
        Ok(())
    }

    fn paint_telescope_position(
        &mut self,
        tele_pos:   &Option<EqCoord>,
        ctx:        &PaintCtx,
    ) -> eyre::Result<()> {
        let Some(telescope_pos) = tele_pos else { return Ok(()); };
        let painter = TelescopeCrosshairPainter {
            crd: *telescope_pos,
        };
        self.item_painter.paint_if_visible(&painter, ctx, true)?;
        Ok(())
    }

    fn calc_frame_corners(
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
        cam_frame: &Option<CameraFov>,
        ctx:       &PaintCtx,
    ) -> eyre::Result<()> {
        if let Some(cam_frame) = cam_frame {
            let center_crd = ctx.view_point.crd.to_sphere_pt();
            let center_crd = ctx.eq_sphere_cvt.sphere_to_eq(&center_crd);

            let coords = Self::calc_frame_corners(
                &center_crd,
                cam_frame.horiz_fov,
                cam_frame.vert_fov,
                cam_frame.rot_angle
            );

            let painter = CameraFramePainter { name: &cam_frame.name, coords };
            self.item_painter.paint_if_visible(&painter, ctx, false)?;
        }

        Ok(())
    }

    fn paint_plate_solved_image(
        &mut self,
        ps_image: &Option<PlateSolvedImage>,
        ctx:      &PaintCtx,
    ) -> eyre::Result<()> {
        let Some(ps_image) = ps_image else { return Ok(()); };
        let coords = Self::calc_frame_corners(
            &ps_image.center_crd,
            ps_image.horiz_fov,
            ps_image.vert_fov,
            ps_image.rotation
        );
        let painter = PlateSolvedImagePainter {
            image: ps_image.image.clone(),
            time: ps_image.time,
            coords,
            persp_painter: &self.persp_painter,
        };
        self.item_painter.paint_if_visible(&painter, ctx, false)?;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum PaintPass {
    Objects,
    Names,
}

enum ItemCrd {
    Horiz(HorizCoord),
    Eq(EqCoord),
    /// Point already in the observer's sphere coordinates: neither epoch nor
    /// observer conversion is applied to it afterwards
    SpherePt(Point3D),
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

trait MapItem {
    fn needs_now_epoch_conversion(&self) -> bool;
    fn point_count(&self) -> usize;
    fn point_crd(&self, index: usize) -> ItemCrd;
    fn paint_projected(&self, _ctx: &PaintCtx, _points: &[Point2D]) -> eyre::Result<()> { Ok(()) }
}

struct ItemPainter {
    vp_points:     Vec<Point3D>,
    screen_points: Vec<Point2D>,
}

impl ItemPainter {
    fn new() -> Self {
        Self {
            vp_points:     Vec::new(),
            screen_points: Vec::new(),
        }
    }

    fn paint_if_visible(
        &mut self,
        obj:                 &dyn MapItem,
        ctx:                 &PaintCtx,
        allow_below_horizon: bool,
    ) -> eyre::Result<bool> {
        let points_count = obj.point_count();
        let use_now_epoch = obj.needs_now_epoch_conversion();

        self.vp_points.clear();
        let mut obj_is_visible = false;
        for i in 0..points_count {
            let mut invisible = false;

            let sphere_pt = match obj.point_crd(i) {
                ItemCrd::Horiz(horiz) => horiz.to_sphere_pt(),
                ItemCrd::SpherePt(pt) => pt,
                ItemCrd::Eq(eq) => {
                    let mut sphere_pt = eq.to_sphere_pt();
                    if use_now_epoch {
                        sphere_pt = ctx.epoch_cvt.convert_pt(&sphere_pt);
                    }
                    ctx.eq_sphere_cvt.apply(&mut sphere_pt);
                    sphere_pt
                }
            };

            invisible |= !allow_below_horizon && sphere_pt.x < 0.0;

            let crd_vp = ctx.sphere_scr_cvt.apply_viewpoint(&sphere_pt);

            invisible |= crd_vp.z < -0.5;

            if !invisible {
                obj_is_visible = true;
            }

            self.vp_points.push(crd_vp);
        }
        if !obj_is_visible {
            return Ok(false);
        }

        let scr_tolerance = ctx.screen.tolerance();

        // 3d coordinates -> screen coordinates
        self.screen_points.clear();
        obj_is_visible = false;
        for pt in &self.vp_points {
            let pt_s = ctx.screen.sphere_to_screen(pt, ctx.view_point.mag_factor);
            if !obj_is_visible
            && scr_tolerance.left < pt_s.x && pt_s.x < scr_tolerance.right
            && scr_tolerance.top < pt_s.y && pt_s.y < scr_tolerance.bottom {
                obj_is_visible = true;
            }
            self.screen_points.push(pt_s);
        }

        // check if 2D lines cross screen boundaries
        if !obj_is_visible && !self.crosses_view_bounds(ctx) {
            return Ok(false);
        }

        obj.paint_projected(ctx, &self.screen_points)?;

        Ok(true)
    }

    /// Fallback visibility test: a segment of the projected figure crosses
    /// the screen boundary. Uses `points_screen`
    fn crosses_view_bounds(&self, ctx: &PaintCtx) -> bool {
        if self.screen_points.len() < 2 {
            return false;
        }
        let scr_tolerance = ctx.screen.tolerance();
        let top_line = scr_tolerance.top_line();
        let bottom_line = scr_tolerance.bottom_line();
        let left_line = scr_tolerance.left_line();
        let right_line = scr_tolerance.right_line();
        self.screen_points
            .iter()
            .circular_tuple_windows()
            .any(|(&crd1, &crd2)| {
                let line = Line2D { pt1: crd1, pt2: crd2 };
                Line2D::intersection(&line, &top_line).is_some() ||
                Line2D::intersection(&line, &bottom_line).is_some() ||
                Line2D::intersection(&line, &left_line).is_some() ||
                Line2D::intersection(&line, &right_line).is_some()
            }
        )
    }

    /// Projects an equatorial polyline (precession to now epoch applied) to
    /// screen coordinates without the `&dyn Item` overhead.
    /// Returns None if the polyline is culled, see `fill_screen_pts()`
    fn project_eq_polyline(&mut self, pts: &[EqCoord], ctx: &PaintCtx) -> Option<&[Point2D]> {
        self.vp_points.clear();
        let mut sphere_visible = false;
        for eq in pts {
            let mut sphere_pt = eq.to_sphere_pt();
            sphere_pt = ctx.epoch_cvt.convert_pt(&sphere_pt);
            ctx.eq_sphere_cvt.apply(&mut sphere_pt);
            let crd_vp = ctx.sphere_scr_cvt.apply_viewpoint(&sphere_pt);
            if sphere_pt.x >= 0.0 && crd_vp.z >= -0.5 {
                sphere_visible = true;
            }
            self.vp_points.push(crd_vp);
        }
        if !sphere_visible {
            return None;
        }
        self.to_screen_pts(ctx)
    }

    /// Projects points already in the observer's sphere coordinates (see
    /// `PainterCrd::SpherePt`). Returns None if the figure is culled,
    /// see `fill_screen_pts()`
    fn project_sphere_pts(&mut self, pts: &[Point3D], ctx: &PaintCtx) -> Option<&[Point2D]> {
        self.vp_points.clear();
        let mut sphere_visible = false;
        for sphere_pt in pts {
            let crd_vp = ctx.sphere_scr_cvt.apply_viewpoint(sphere_pt);
            if sphere_pt.x >= 0.0 && crd_vp.z >= -0.5 {
                sphere_visible = true;
            }
            self.vp_points.push(crd_vp);
        }
        if !sphere_visible {
            return None;
        }
        self.to_screen_pts(ctx)
    }

    /// Second stage of the projection: viewpoint-space points from
    /// `points_3d` to screen coordinates in `points_screen`, with the common
    /// visibility tests. Unlike `paint()`, also culls figures with non-finite
    /// projections. Returns None if the figure is culled
    fn to_screen_pts(&mut self, ctx: &PaintCtx) -> Option<&[Point2D]> {
        self.screen_points.clear();
        let mut screen_visible = false;
        let scr_tolerance = ctx.screen.tolerance();
        for crd_vp in &self.vp_points {
            let pt_s = ctx.screen.sphere_to_screen(crd_vp, ctx.view_point.mag_factor);
            if !f64::is_finite(pt_s.x) || !f64::is_finite(pt_s.y) {
                // point at the projection antipode: skip the whole figure
                return None;
            }
            if !screen_visible
            && scr_tolerance.left < pt_s.x && pt_s.x < scr_tolerance.right
            && scr_tolerance.top < pt_s.y && pt_s.y < scr_tolerance.bottom {
                screen_visible = true;
            }
            self.screen_points.push(pt_s);
        }
        if !screen_visible && !self.crosses_view_bounds(ctx) {
            return None;
        }
        Some(&self.screen_points)
    }
}

struct HorizonGlowPainter {
    // Cached sphere grid (bottom/top point per azimuth column) and projected
    // corners of the visible azimuth bands, reused per frame
    grid:       Vec<Point3D>,
    grid_angle: f64,
    corners:    Vec<Point2D>,
}

impl HorizonGlowPainter {
    // Horizon glow azimuth resolution, degrees
    const AZ_STEP_DEG: f64 = 2.0;
    // Constant-alpha sub-bands replacing the per-band gradient
    const ALPHA_SUB_BANDS: usize = 32;

    fn new() -> Self {
        Self {
            grid:       Vec::new(),
            // NaN != any angle, so the grid is built on the first call
            grid_angle: f64::NAN,
            corners:    Vec::new(),
        }
    }

    fn paint(&mut self, item_painter: &mut ItemPainter, ctx: &PaintCtx) -> eyre::Result<()> {
        let angle = degree_to_radian(ctx.config.horizon_glow.height_deg);
        // The UI angle is at least 1 degree, a zero one means the glow is off
        if angle <= 0.0 {
            return Ok(());
        }
        self.rebuild_grid(angle);

        let color = &ctx.config.horizon_glow.color;
        ctx.cairo.set_antialias(gtk::cairo::Antialias::None);
        ctx.cairo.set_fill_rule(gtk::cairo::FillRule::Winding);

        self.corners.clear();

        // Project the corners of every azimuth band once per frame and keep
        // the visible bands for the sub-band pass
        let cols = self.grid.len() / 2;
        for i in 0..cols - 1 {
            // Corner order: top of both columns first, then the bottoms,
            // see the grid layout in `rebuild_grid()`
            let corners = [
                self.grid[i * 2 + 1].clone(),
                self.grid[(i + 1) * 2 + 1].clone(),
                self.grid[(i + 1) * 2].clone(),
                self.grid[i * 2].clone(),
            ];
            let Some(scr) = item_painter.project_sphere_pts(&corners, ctx) else {
                continue;
            };
            self.corners.extend_from_slice(scr);
        }
        if self.corners.is_empty() {
            return Ok(());
        }

        // Split each visible band into horizontal sub-bands of constant alpha:
        // solid fills instead of a gradient per band. Lerping between the
        // projected band edges matches the old gradient's approximation, which
        // interpolated linearly between the top and bottom edge midpoints
        let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
        // t=0 is the outer edge (alpha 0), t=1 is the horizon (full alpha):
        // sample the alpha of each step at its midpoint
        for k in 0..Self::ALPHA_SUB_BANDS {
            let t0 = k as f64 / Self::ALPHA_SUB_BANDS as f64;
            let t1 = (k + 1) as f64 / Self::ALPHA_SUB_BANDS as f64;
            let alpha = color.a * (k as f64 + 0.5) / Self::ALPHA_SUB_BANDS as f64;
            ctx.cairo.set_source_rgba(color.r, color.g, color.b, alpha);
            for band in self.corners.as_chunks::<4>().0 {
                let (top1, top2, bottom2, bottom1) = (band[0], band[1], band[2], band[3]);
                ctx.cairo.move_to(lerp(top1.x, bottom1.x, t0), lerp(top1.y, bottom1.y, t0));
                ctx.cairo.line_to(lerp(top2.x, bottom2.x, t0), lerp(top2.y, bottom2.y, t0));
                ctx.cairo.line_to(lerp(top2.x, bottom2.x, t1), lerp(top2.y, bottom2.y, t1));
                ctx.cairo.line_to(lerp(top1.x, bottom1.x, t1), lerp(top1.y, bottom1.y, t1));
                ctx.cairo.close_path();
            }
            ctx.cairo.fill()?;
        }

        Ok(())
    }

    fn rebuild_grid(&mut self, angle: f64) {
        if self.grid_angle == angle {
            return;
        }
        // Grid layout: per azimuth column, a horizon point then a top point
        let cols = (360.0 / Self::AZ_STEP_DEG) as usize + 1;
        self.grid.clear();
        self.grid.reserve(cols * 2);
        for i in 0..cols {
            let az = degree_to_radian(Self::AZ_STEP_DEG * i as f64);
            self.grid.push(HorizCoord { alt: 0.0,  az  }.to_sphere_pt());
            self.grid.push(HorizCoord { alt: angle, az }.to_sphere_pt());
        }
        self.grid_angle = angle;
    }
}

struct ConstellationBoundsPainter {
    // Projected bounds polygons and their spans in it, reused per frame
    buf:   Vec<[f64; 2]>,
    spans: Vec<(u8, u32, u32)>,
    // Constellation whose bounds contain the screen center, found per frame
    // and reused by the constellation lines and names painters
    center_constellation_id: Option<u8>,
}

impl ConstellationBoundsPainter {
    fn new() -> Self {
        Self {
            buf:         Vec::new(),
            spans:       Vec::new(),
            center_constellation_id: None,
        }
    }

    fn paint(
        &mut self,
        item_painter: &mut ItemPainter,
        sky_map:      &SkyMap,
        ctx:          &PaintCtx,
    ) -> eyre::Result<()> {
        let constellations = sky_map.constellations();

        // Project each bounds polygon once; the projection is reused for both
        // the screen center test and the batched painting
        self.center_constellation_id = None;
        self.buf.clear();
        self.spans.clear();
        let screen_center = [ctx.screen.center_x, ctx.screen.center_y];
        for (id, constellation) in constellations {
            for polygon in &constellation.bounds {
                let Some(points) = item_painter.project_eq_polyline(polygon, ctx) else {
                    continue;
                };
                let (start, len) = (self.buf.len(), points.len());
                for pt in points {
                    self.buf.push([pt.x, pt.y]);
                }
                self.spans.push((*id, start as u32, len as u32));
                if self.center_constellation_id.is_none()
                && point_in_polygon(&self.buf[start..start + len], screen_center) {
                    self.center_constellation_id = Some(*id);
                }
            }
        }

        ctx.cairo.set_antialias(ctx.config.antialias());

        // Polygons of all other constellations share one path and one stroke.
        // Note: at wide view the shared borders of adjacent constellations were
        // previously stroked by both polygons (double alpha ~0.51), now they are
        // drawn once with the nominal 0.3 - the layer became uniformly fainter
        ctx.cairo.set_source_rgba(0.2, 0.5, 0.2, 0.3);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.5, 1.0));
        let mut any = false;
        for &(id, start, len) in &self.spans {
            if Some(id) == self.center_constellation_id {
                continue;
            }
            self.add_polygon(ctx, start, len);
            any = true;
        }
        if any {
            ctx.cairo.stroke()?;
        }

        // Current constellation polygons: fill and dashed stroke on top
        if let Some(cur_id) = self.center_constellation_id {
            ctx.cairo.set_fill_rule(gtk::cairo::FillRule::Winding);
            ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.7, 1.0));
            ctx.cairo.set_dash(&[6.0, 6.0], 0.0);
            for &(id, start, len) in &self.spans {
                if id != cur_id {
                    continue;
                }
                self.add_polygon(ctx, start, len);
                ctx.cairo.set_source_rgba(0.3, 0.6, 0.3, 0.3);
                ctx.cairo.fill_preserve()?;
                ctx.cairo.set_source_rgb(0.4, 0.7, 0.4);
                ctx.cairo.stroke()?;
            }
            // Do not leak the dash pattern to the layers that do not set it
            ctx.cairo.set_dash(&[], 0.0);
        }

        Ok(())
    }

    // Adds the spanned polygon to the current cairo path
    fn add_polygon(&self, ctx: &PaintCtx, start: u32, len: u32) {
        let pts = &self.buf[start as usize..(start + len) as usize];
        ctx.cairo.move_to(pts[0][0], pts[0][1]);
        for pt in &pts[1..] {
            ctx.cairo.line_to(pt[0], pt[1]);
        }
        ctx.cairo.close_path();
    }
}

// Paint DSO item

struct DsoNamePainter<'a>(&'a DsoItem);

impl MapItem for DsoNamePainter<'_> {
    fn needs_now_epoch_conversion(&self) -> bool {
        true
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Eq(EqCoord {
            ra: self.0.crd.ra(),
            dec: self.0.crd.dec()
        })
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
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

/// Object local frame (axis 2 points at the object) in the observer's sphere
/// coordinates. The conversions are linear, so the images of the basis stand
/// for the whole chain: points built from them need no epoch conversion
struct EqLocalFrame {
    axes: [Point3D; 3],
}

impl EqLocalFrame {
    fn new(
        ra:             f64,
        dec:            f64,
        epoch_cvt:      &EpochCvt,
        eq_sphere_cvt:  &EqToSphereCvt,
    ) -> Self {
        let dec_rot = RotMatrix::new(0.5 * PI - dec);
        let ra_rot = RotMatrix::new(PI / 2.0 - ra);

        let mut axes = [
            Point3D { x: 1.0, y: 0.0, z: 0.0 },
            Point3D { x: 0.0, y: 1.0, z: 0.0 },
            Point3D { x: 0.0, y: 0.0, z: 1.0 },
        ];
        for axis in axes.iter_mut() {
            axis.rotate_over_x_mat(&dec_rot);
            axis.rotate_over_z_mat(&ra_rot);
            *axis = epoch_cvt.convert_pt(axis);
            eq_sphere_cvt.apply(axis);
        }

        Self { axes }
    }
}

struct DsoEllipse {
    /// Already in the now-epoch observer sphere coordinates
    points:     Vec<Point3D>,
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

    /// Rebuilds the outline: the angular axes in radians (any of them may be the
    /// longer one) are oriented by `angle` in the local frame; `pxls_per_rad` and
    /// `max_line_width` affect only the line thickness
    fn set_shape(
        &mut self,
        frame:          &EqLocalFrame,
        maj_axis:       f64,
        min_axis:       f64,
        angle:          f64,
        pxls_per_rad:   f64,
        max_line_width: f64,
    ) {
        let a = 0.5 * maj_axis;
        let b = 0.5 * min_axis;

        self.points.clear();
        let pts_count = Self::segments_count(a, b, pxls_per_rad);
        Self::fill_points(frame, a, b, angle, pts_count, &mut self.points);

        let mut line_width = 0.01 * f64::max(maj_axis, min_axis) * pxls_per_rad;
        line_width = line_width.max(1.0);
        self.line_width = f64::min(line_width, max_line_width);
    }

    /// Angular radius of the ellipse at the polar angle `az` of the object local
    /// frame, given as its sine and cosine
    fn radius_at_dir(a: f64, b: f64, sin_az: f64, cos_az: f64) -> f64 {
        a * b / f64::sqrt(a * a * sin_az * sin_az + b * b * cos_az * cos_az)
    }

    /// Segments for a polygon sagitta of ~0.25 px at the ends of the major axis,
    /// where the curvature is the highest: sagitta ~ 2 * PI^2 * A / N^2, with A
    /// the axis length in px. Very large objects hit the cap and get a bigger one
    fn segments_count(a: f64, b: f64, pxls_per_rad: f64) -> usize {
        const MAX_PTS_COUNT: usize = 66;
        const MIN_PTS_COUNT: usize = 8;
        const SAGITTA_PX: f64 = 0.25;

        let maj_axis_pxls = f64::max(a, b) * pxls_per_rad;
        let count = f64::ceil(PI * f64::sqrt(maj_axis_pxls / (2.0 * SAGITTA_PX))) as usize;
        usize::clamp(count, MIN_PTS_COUNT, MAX_PTS_COUNT)
    }

    /// Outline points in the observer's sphere coordinates
    fn fill_points(
        frame:     &EqLocalFrame,
        a:         f64,
        b:         f64,
        angle:     f64,
        pts_count: usize,
        points:    &mut Vec<Point3D>,
    ) {
        let x_ax = &frame.axes[0];
        let y_ax = &frame.axes[1];
        let z_ax = &frame.axes[2];

        let (sin_angle, cos_angle) = f64::sin_cos(angle);

        for i in 0..pts_count {
            let t = 2.0 * PI * i as f64 / pts_count as f64;
            let (sin_t, cos_t) = f64::sin_cos(t);

            // Sampling by the ellipse parameter, not by the polar angle, puts the
            // points where the curvature is the highest, at the major axis ends
            let u = a * cos_t;
            let v = b * sin_t;
            let len = f64::sqrt(u * u + v * v);
            let inv_len = 1.0 / len;

            // radius_at_dir() of the (u, v) direction is |(u, v)|, so the outline stays
            // the catalog ellipse
            let alt = Self::radius_at_dir(a, b, v * inv_len, u * inv_len);
            let (sin_alt, cos_alt) = f64::sin_cos(alt);

            // (w1, w2) is the point direction turned by `angle`, k scales it by sin(alt)
            let w1 = u * cos_angle + v * sin_angle;
            let w2 = v * cos_angle - u * sin_angle;
            let k = sin_alt * inv_len;

            points.push(Point3D {
                x: cos_alt * z_ax.x + k * (w1 * x_ax.x + w2 * y_ax.x),
                y: cos_alt * z_ax.y + k * (w1 * x_ax.y + w2 * y_ax.y),
                z: cos_alt * z_ax.z + k * (w1 * x_ax.z + w2 * y_ax.z),
            });
        }
    }
}

impl MapItem for DsoEllipse {
    fn needs_now_epoch_conversion(&self) -> bool {
        // Points are built in the now-epoch observer frame already
        false
    }

    fn point_count(&self) -> usize {
        self.points.len()
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        ItemCrd::SpherePt(self.points[index].clone())
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
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

        ctx.cairo.set_antialias(ctx.config.antialias());
        ctx.cairo.stroke()?;

        ctx.cairo.set_dash(&[], 0.0);

        Ok(())
    }
}

// Paint outline

impl MapItem for Outline {
    fn needs_now_epoch_conversion(&self) -> bool {
        true
    }

    fn point_count(&self) -> usize {
        self.polygon.len()
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        let pt = &self.polygon[index];
        ItemCrd::Eq(EqCoord {
            ra: pt.ra(),
            dec: pt.dec()
        })
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
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
        ctx.cairo.set_antialias(ctx.config.antialias());
        ctx.cairo.stroke()?;
        Ok(())
    }
}

// Paint star

struct StarPainterParams {
    max_diam:         f64,
    max_mag_value:    f32,
    grow_slow_thresh: f64,
    diam_per_light:   f64,
    bright_min_diam:  f64,
}
struct StarPainter<'a> {
    star:   &'a StarData,
    pass:   PaintPass,
    name:   &'a str,
    bayer:  &'a str,
    params: &'a StarPainterParams,
}

type RgbTuple = (f64, f64, f64);

impl StarPainter<'_> {
    fn calc_light(&self, star_mag: f32) -> (f32, f32) {
        let light = f32::powf(2.0, 0.4 * (self.params.max_mag_value - star_mag)) - 1.0;
        let light_with_gamma = light.powf(0.7);
        (light, light_with_gamma)
    }

    fn calc_diam(&self, light: f32) -> f64 {
        let mut diam = (self.params.diam_per_light * light as f64).max(1.0);

        if self.star.mag.get() < 1.0 {
            diam = diam.max(self.params.bright_min_diam)
        }

        if diam > self.params.grow_slow_thresh {
            diam -= self.params.grow_slow_thresh;
            diam /= 2.0;
            diam += self.params.grow_slow_thresh;
        }
        if diam > self.params.max_diam {
            diam = self.params.max_diam;
        }
        diam
    }

    fn rgb_for_bv(bv: f32) -> RgbTuple {
        const RED_BV: f32 = 2.5;
        const BLUE_BV: f32 = -0.3;

        const RED:    RgbTuple = (1.0, 0.4,  0.4);
        const ORANGE: RgbTuple = (1.0, 0.9,  0.6);
        const YELLOW: RgbTuple = (1.0, 1.0,  0.7);
        const WHITE:  RgbTuple = (0.9, 0.9,  0.9);
        const BLUE:   RgbTuple = (0.4, 0.66, 1.0);

        const TABLE: &[(f32, RgbTuple)] = &[
            (BLUE_BV, BLUE),
            (0.0,    WHITE),
            (0.65,   YELLOW),
            (1.6,    ORANGE),
            (RED_BV,  RED),
        ];

        if bv >= RED_BV {
            return RED;
        } else if bv <= BLUE_BV {
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

    fn paint_object(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        let star_mag = self.star.mag.get();
        let (light, light_with_gamma) = self.calc_light(star_mag);
        if light_with_gamma < 0.1 { return Ok(()); }
        let (r, g, b) = Self::rgb_for_bv(self.star.bv.get());
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

    fn paint_name(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        if self.name.is_empty() && self.bayer.is_empty() { return Ok(()); }
        let star_mag = self.star.mag.get();
        let (light, light_with_gamma) = self.calc_light(star_mag);
        let (r, g, b) = Self::rgb_for_bv(self.star.bv.get());
        let diam = self.calc_diam(light);
        let mut pt = points[0];
        let mut paint_text = |text, light_with_gamma| -> eyre::Result<()> {
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
            paint_text(self.bayer, 0.5 * light_with_gamma)?;
        }

        Ok(())
    }
}

impl MapItem for StarPainter<'_> {
    fn needs_now_epoch_conversion(&self) -> bool {
        true
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Eq(EqCoord {
            dec: self.star.crd.dec(),
            ra: self.star.crd.ra(),
        })
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        match self.pass {
            PaintPass::Objects =>
                self.paint_object(ctx, points),
            PaintPass::Names =>
                self.paint_name(ctx, points),
        }
    }
}

// Paint equatorial grid

enum EqGridItemKind { Ra, Dec }

struct EqGridItem {
    kind: EqGridItemKind,
    dec1: f64,
    dec2: f64,
    ra1:  f64,
    ra2:  f64,
    is_label: bool,
    text_offset: f64,
}

impl EqGridItem {
    const POINTS_CNT: usize = 5;
}

impl MapItem for EqGridItem {
    fn needs_now_epoch_conversion(&self) -> bool {
        false
    }

    fn point_count(&self) -> usize {
        Self::POINTS_CNT
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        match self.kind {
            EqGridItemKind::Ra => ItemCrd::Eq(EqCoord{
                ra: self.ra1,
                dec: linear_interpolate(
                    index as f64,
                    0.0,
                    (Self::POINTS_CNT-1) as f64,
                    self.dec1,
                    self.dec2
                )
            }),
            EqGridItemKind::Dec => ItemCrd::Eq(EqCoord{
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

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        if !self.is_label {
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
                let line = Line2D { pt1: *pt1, pt2: *pt2 };
                let paint_text = |mut x, y, adjust_right| -> eyre::Result<()> {
                    let text = match  self.kind {
                        EqGridItemKind::Ra => format!("{:.0}h", radian_to_hour(self.ra1)),
                        EqGridItemKind::Dec => format!("{:.0}°", radian_to_degree(self.dec1)),
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

impl Ground<'_> {
    const AZ_STEP_DEG: usize = 5;
}

impl MapItem for Ground<'_> {
    fn needs_now_epoch_conversion(&self) -> bool {
        false
    }

    fn point_count(&self) -> usize {
        360 / Self::AZ_STEP_DEG
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        ItemCrd::Horiz(HorizCoord {
            alt: 0.0,
            az: PI * (index * Self::AZ_STEP_DEG) as f64 / 180.0
        })
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        let ground_around = self.view_point.crd.alt >= 0.0;
        if ground_around {
            const RECT_EXTEND_PX: f64 = 100.0;
            let mut min_x = points.iter().map(|p| p.x).min_by(f64::total_cmp).unwrap_or_default() - RECT_EXTEND_PX;
            let mut min_y = points.iter().map(|p| p.y).min_by(f64::total_cmp).unwrap_or_default() - RECT_EXTEND_PX;
            let mut max_x = points.iter().map(|p| p.x).max_by(f64::total_cmp).unwrap_or_default() + RECT_EXTEND_PX;
            let mut max_y = points.iter().map(|p| p.y).max_by(f64::total_cmp).unwrap_or_default() + RECT_EXTEND_PX;

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

struct CompassLabel<'a> {
    text: &'a str,
    az: f64,
    alpha: f64,
}

impl MapItem for CompassLabel<'_> {
    fn needs_now_epoch_conversion(&self) -> bool {
        false
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Horiz(HorizCoord {
            alt: 0.0,
            az: degree_to_radian(self.az)
        })
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        ctx.layout.set_text(self.text);
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

struct ZoneCullingProbe {
    coords: [EqCoord; 4],
}

impl MapItem for ZoneCullingProbe {
    fn needs_now_epoch_conversion(&self) -> bool {
        true
    }

    fn point_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        ItemCrd::Eq(self.coords[index])
    }
}

struct PointCullingProbe {
    coord: EqCoord,
    use_now_epoch: bool,
}

impl MapItem for PointCullingProbe {
    fn needs_now_epoch_conversion(&self) -> bool {
        self.use_now_epoch
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Eq(self.coord)
    }
}

struct TelescopeCrosshairPainter {
    crd: EqCoord,
}

impl MapItem for TelescopeCrosshairPainter {
    fn needs_now_epoch_conversion(&self) -> bool {
        false
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Eq(self.crd)
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        let pt = &points[0];
        let line_size = 40.0 * ctx.screen.dpmm_x();
        ctx.cairo.set_line_width(1.0);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_antialias(ctx.config.antialias());
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

impl MapItem for SelectionPainter {
    fn needs_now_epoch_conversion(&self) -> bool {
        true
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Eq(self.crd)
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        let pt = &points[0];
        ctx.cairo.set_antialias(ctx.config.antialias());
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

 impl MapItem for CameraFramePainter<'_> {
    fn needs_now_epoch_conversion(&self) -> bool {
        false
    }

    fn point_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        ItemCrd::Eq(self.coords[index])
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        ctx.cairo.move_to(points[0].x, points[0].y);
        for pt in &points[1..] {
            ctx.cairo.line_to(pt.x, pt.y);
        }
        ctx.cairo.set_antialias(ctx.config.antialias());
        ctx.cairo.close_path();
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);
        ctx.cairo.set_dash(&[], 0.0);
        ctx.cairo.set_line_width(f64::max(ctx.screen.dpmm_x() * 0.1, 1.0));
        ctx.cairo.stroke()?;

        let pt1 = &points[0];
        let pt2 = &points[1];
        ctx.cairo.set_source_rgb(1.0, 1.0, 1.0);

        paint_text_along_line(ctx.cairo, ctx.layout, pt1, pt2, self.name)?;

        Ok(())
    }
}

fn paint_text_along_line(
    cairo:  &cairo::Context,
    layout: &pango::Layout,
    pt1:    &Point2D,
    pt2:    &Point2D,
    text:   &str
) -> eyre::Result<()> {
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
    image:         gdk_pixbuf::Pixbuf,
    time:          DateTime<Utc>,
    coords:        [EqCoord; 4],
    persp_painter: &'a RefCell<PerspectivePainter>,
}

impl MapItem for PlateSolvedImagePainter<'_> {
    fn needs_now_epoch_conversion(&self) -> bool {
        false
    }

    fn point_count(&self) -> usize {
        self.coords.len()
    }

    fn point_crd(&self, index: usize) -> ItemCrd {
        ItemCrd::Eq(self.coords[index])
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        let mut persp_pnt = self.persp_painter.borrow_mut();
        persp_pnt.paint(
            ctx.cairo,
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

        ctx.cairo.set_antialias(ctx.config.antialias());
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
            "Plate solved {}",
            local_time.format("%Y-%m-%d %H:%M:%S")
        );
        paint_text_along_line(ctx.cairo, ctx.layout, pt1, pt2, &text)?;

        Ok(())
    }
}

struct ConstellationNamePainter<'a> {
    ra:         f64,
    dec:        f64,
    name:       &'a str,
    is_current: bool,
}

impl<'a> MapItem for ConstellationNamePainter<'a> {
    fn needs_now_epoch_conversion(&self) -> bool {
        true
    }

    fn point_count(&self) -> usize {
        1
    }

    fn point_crd(&self, _index: usize) -> ItemCrd {
        ItemCrd::Eq(EqCoord { dec: self.dec, ra: self.ra })
    }

    fn paint_projected(&self, ctx: &PaintCtx, points: &[Point2D]) -> eyre::Result<()> {
        ctx.layout.set_text(self.name);
        let (width, height) = ctx.layout.pixel_size();
        ctx.cairo.move_to(
            points[0].x - 0.5 * width as f64,
            points[0].y - 0.5 * height as f64
        );
        if self.is_current {
            ctx.cairo.set_source_rgb(0.4, 0.7, 0.4);
        } else {
            ctx.cairo.set_source_rgb(0.2, 0.2, 0.6);
        }
        pangocairo::show_layout(ctx.cairo, ctx.layout);

        Ok(())
    }
}
