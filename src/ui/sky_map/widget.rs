use std::{cell::RefCell, f64::consts::PI, rc::Rc, time::Duration};
use chrono::{NaiveDateTime, Utc};
use gtk::{gdk, cairo, glib::{self, clone}, prelude::*};
use crate::utils::math::linear_interpolate;

use super::{consts::*, data::*, painter::*, utils::*};

struct MousePressedData {
    hcrd:  HorizCoord,
    point: (f64, f64),
    vp:    ViewPoint,
}

struct AnimatedGotoCrdData {
    start_crd:  EqCoord,
    end_crd:    EqCoord,
    stage:      usize,
    max_stages: usize,
}

pub struct SkymapWidget {
    skymap:          RefCell<Option<Rc<SkyMap>>>,
    evt_box:         gtk::EventBox,
    draw_area:       gtk::DrawingArea,
    view_point:      RefCell<ViewPoint>,
    config:          RefCell<PaintConfig>,
    mpress:          RefCell<Option<MousePressedData>>,
    observer:        RefCell<Observer>,
    time:            RefCell<NaiveDateTime>,
    selected_obj:    RefCell<Option<SkymapObject>>,
    center_crd:      RefCell<Option<EqCoord>>,
    telescope_pos:   RefCell<Option<EqCoord>>,
    camera_frame:    RefCell<Option<CameraFrame>>,
    ani_goto_data:   RefCell<Option<AnimatedGotoCrdData>>,
    select_handlers: RefCell<Vec<Box<dyn Fn(Option<SkymapObject>)>>>,
}

impl SkymapWidget {
    pub fn new() -> Rc<Self> {
        let evt_box = gtk::EventBox::builder()
            .visible(true)
            .build();
        let da = gtk::DrawingArea::builder()
            .visible(true)
            .parent(&evt_box)
            .build();

        let widget = Rc::new(Self {
            skymap:          RefCell::new(None),
            evt_box:         evt_box.clone(),
            draw_area:       da.clone(),
            view_point:      RefCell::new(ViewPoint::new()),
            config:          RefCell::new(PaintConfig::default()),
            mpress:          RefCell::new(None),
            observer:        RefCell::new(Observer::default()),
            time:            RefCell::new(Utc::now().naive_utc()),
            selected_obj:    RefCell::new(None),
            center_crd:      RefCell::new(None),
            telescope_pos:   RefCell::new(None),
            camera_frame:    RefCell::new(None),
            ani_goto_data:   RefCell::new(None),
            select_handlers: RefCell::new(Vec::new()),
        });

        widget.connect_event_handlers();

        widget
    }

    pub fn add_obj_sel_handler(
        &self,
        obj_sel_handler: impl Fn(Option<SkymapObject>) + 'static
    ) {
        self.select_handlers.borrow_mut().push(Box::new(obj_sel_handler));
    }

    pub fn get_widget(&self) -> &gtk::Widget {
        self.evt_box.upcast_ref::<gtk::Widget>()
    }

    pub fn set_skymap(&self, skymap: &Rc<SkyMap>) {
        *self.skymap.borrow_mut() = Some(Rc::clone(skymap));
        self.draw_area.queue_draw();
    }

    pub fn set_observer(&self, observer: &Observer) {
        *self.observer.borrow_mut() = observer.clone();
        self.draw_area.queue_draw();
    }

    pub fn time(&self) -> NaiveDateTime {
        *self.time.borrow()
    }

    pub fn set_paint_config(
        &self,
        time:          &NaiveDateTime,
        config:        &PaintConfig,
        telescope_pos: &Option<EqCoord>,
        camera_frame:  &Option<CameraFrame>,
    ) {
        *self.time.borrow_mut() = time.clone();
        *self.config.borrow_mut() = config.clone();
        *self.telescope_pos.borrow_mut() = telescope_pos.clone();
        *self.camera_frame.borrow_mut() = camera_frame.clone();

        if self.ani_goto_data.borrow().is_some() {
            return;
        }

        if let Some(center_crd) = &*self.center_crd.borrow() {
            let observer = self.observer.borrow();
            let cvt = EqToHorizCvt::new(&*observer, &time);
            self.view_point.borrow_mut().crd = cvt.eq_to_horiz(center_crd);
        }

        self.draw_area.queue_draw();
    }

    fn connect_event_handlers(self: &Rc<Self>) {
        self.draw_area.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Stop,
            move |da, ctx| {
                self_.handler_draw(da, ctx)
            })
        );

        self.evt_box.connect_button_press_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Stop,
            move |_, event| {
                self_.handler_button_press(event)
            }
        ));

        self.evt_box.connect_motion_notify_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Stop,
            move |_, event| {
                self_.hanler_motion_notify(event)
            }
        ));

        self.evt_box.connect_button_release_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Stop,
            move |_, _| {
                *self_.mpress.borrow_mut() = None;
                glib::Propagation::Stop
            }
        ));

        self.evt_box.connect_scroll_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Stop,
                move |_, event| {
                    self_.handler_scroll(event)
                }
            )
        );

        self.evt_box.set_events(
            gdk::EventMask::SCROLL_MASK |
            gdk::EventMask::POINTER_MOTION_MASK |
            gdk::EventMask::BUTTON_PRESS_MASK
        );
    }

    fn handler_button_press(self: &Rc<Self>, event: &gdk::EventButton) -> glib::Propagation {
        if event.button() == gdk::ffi::GDK_BUTTON_PRIMARY as u32 {
            match event.event_type() {
                gdk::EventType::ButtonPress =>
                    self.start_map_drag(event),
                gdk::EventType::DoubleButtonPress =>
                    self.select_object(event),
                _ => {},
            };
        }
        glib::Propagation::Proceed
    }

    fn start_map_drag(&self, event: &gdk::EventButton) {
        let Some((x, y)) = event.coords() else { return; };
        let vp = self.view_point.borrow();
        let si = ScreenInfo::new(&self.draw_area);
        let cvt = HorizToScreenCvt::new(&*vp);
        let Some(hcrd) = cvt.screen_to_horiz(&Point2D {x, y}, &si) else { return; };
        *self.mpress.borrow_mut() = Some(MousePressedData {
            hcrd,
            point: (x, y),
            vp: self.view_point.borrow().clone(),
        });
    }

    pub fn widget_crd_to_eq(&self, x: f64, y: f64) -> Option<EqCoord> {
        let vp = self.view_point.borrow();
        let cvt = HorizToScreenCvt::new(&*vp);
        let si = ScreenInfo::new(&self.draw_area);
        let Some(hcrd) = cvt.screen_to_horiz(&Point2D {x, y}, &si) else { return None; };
        let observer = self.observer.borrow();
        let time = self.time.borrow();
        let cvt = EqToHorizCvt::new(&observer, &time);
        Some(cvt.horiz_to_eq(&hcrd))
    }

    fn select_object(self: &Rc<Self>, event: &gdk::EventButton) {
        let Some((x, y)) = event.coords() else { return; };
        let Some(skymap) = &*self.skymap.borrow() else { return; };
        let Some(eq_crd) = self.widget_crd_to_eq(x, y) else { return; };
        let config = self.config.borrow();
        let vp = self.view_point.borrow();
        let max_stars_mag = calc_max_star_magnitude_for_painting(vp.mag_factor);
        let selected_obj = skymap.get_nearest(&eq_crd, config.max_dso_mag, max_stars_mag, &config.filter);

        if let Some(selected_obj) = &selected_obj {
            self.animated_goto_coord(&selected_obj.crd());
        }

        let select_handlers = self.select_handlers.borrow();
        for handler in &*select_handlers {
            handler(selected_obj.clone());
        }
        *self.selected_obj.borrow_mut() = selected_obj;
    }

    pub fn set_selected_object(self: &Rc<Self>, obj: Option<&SkymapObject>) {
        *self.selected_obj.borrow_mut() = obj.cloned();
        if let Some(selected_obj) = &obj {
            self.animated_goto_coord(&selected_obj.crd());
        }
    }

    fn hanler_motion_notify(&self, event: &gdk::EventMotion) -> glib::Propagation {
        let Some(mpress) = &*self.mpress.borrow() else {
            return glib::Propagation::Proceed;
        };
        let Some((x, y)) = event.coords() else {
            return glib::Propagation::Proceed;
        };
        let si = ScreenInfo::new(&self.draw_area);
        let cvt = HorizToScreenCvt::new(&mpress.vp);
        let Some(hcrd) = cvt.screen_to_horiz(&Point2D {x, y}, &si) else {
            return glib::Propagation::Stop;
        };
        let mut vp = self.view_point.borrow_mut();
        vp.crd.az = mpress.vp.crd.az + mpress.hcrd.az - hcrd.az;
        vp.crd.alt = mpress.vp.crd.alt + mpress.hcrd.alt - hcrd.alt;
        vp.crd.alt = vp.crd.alt.min(MAX_ALT).max(MIN_ALT);
        *self.center_crd.borrow_mut() = None;
        self.draw_area.queue_draw();
        glib::Propagation::Stop
    }

    fn handler_draw(&self, da: &gtk::DrawingArea, ctx: &cairo::Context) -> glib::Propagation {
        let skymap = self.skymap.borrow();
        let vp = self.view_point.borrow();
        let config = self.config.borrow();
        let observer = self.observer.borrow();
        let time = self.time.borrow();
        let mut painter = SkyMapPainter::new();
        let screen = ScreenInfo::new(da);
        let selection = self.selected_obj.borrow();
        let telescope_pos = self.telescope_pos.borrow();
        let camera_frame = self.camera_frame.borrow();
        let timer = std::time::Instant::now();
        _ = painter.paint(
            &skymap, &selection, &telescope_pos, &camera_frame,
            &observer, &time,
            &config, &vp, &screen, ctx
        );
        let paint_time = timer.elapsed().as_secs_f64();
        let fps = if paint_time != 0.0 { 1.0/paint_time } else { f64::NAN };
        let fps_str = format!("x{:.1}, {:.1} FPS", vp.mag_factor, fps);
        ctx.set_font_size(screen.dpmm_y * 3.0);
        let te = ctx.text_extents(&fps_str).unwrap();
        ctx.move_to(1.0, 1.0 + te.height());
        ctx.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        ctx.show_text(&fps_str).unwrap();
        glib::Propagation::Stop
    }

    fn handler_scroll(&self, event: &gdk::EventScroll) -> glib::Propagation {
        if event.event_type() != gdk::EventType::Scroll {
            return glib::Propagation::Stop;
        }
        let mut vp = self.view_point.borrow_mut();
        let mut mag_factor = vp.mag_factor;
        match event.direction() {
            gdk::ScrollDirection::Up =>
                mag_factor *= MAX_FACTOR_STEP,
            gdk::ScrollDirection::Down =>
                mag_factor /= MAX_FACTOR_STEP,
            _ => {},
        }

        mag_factor = mag_factor
            .min(MAX_MAG_FACTOR)
            .max(MIN_MAG_FACTOR);

        if mag_factor != vp.mag_factor {
            vp.mag_factor = mag_factor;
            self.draw_area.queue_draw();
        }
        glib::Propagation::Stop
    }

    fn animated_goto_coord(self: &Rc<Self>, coord: &EqCoord) {
        *self.center_crd.borrow_mut() = None;

        let already_started = self.ani_goto_data.borrow().is_some();
        let widget_width = self.draw_area.allocated_width() as f64;
        let widget_height = self.draw_area.allocated_height() as f64;
        let Some(mut start_coord) = self.widget_crd_to_eq(0.5 * widget_width, 0.5 * widget_height) else {
            return;
        };

        // Correct start coord for shotter path from start_coord to coord
        while start_coord.ra - coord.ra > PI {
            start_coord.ra -= 2.0 * PI;
        }
        while start_coord.ra - coord.ra < -PI {
            start_coord.ra += 2.0 * PI;
        }

        *self.ani_goto_data.borrow_mut() = Some(AnimatedGotoCrdData {
            start_crd:  start_coord,
            end_crd:    coord.clone(),
            stage:      0,
            max_stages: 10,
        });

        if !already_started {
            glib::timeout_add_local(
                Duration::from_millis(50),
                clone!(@weak self as self_ => @default-return glib::ControlFlow::Break,
                move || {
                    let mut ani_goto_data = self_.ani_goto_data.borrow_mut();
                    let has_to_stop = if let Some(ani_goto_data) = &mut *ani_goto_data {
                        let ra = linear_interpolate(
                            ani_goto_data.stage as f64,
                            0.0,
                            ani_goto_data.max_stages as f64,
                            ani_goto_data.start_crd.ra,
                            ani_goto_data.end_crd.ra
                        );
                        let dec = linear_interpolate(
                            ani_goto_data.stage as f64,
                            0.0,
                            ani_goto_data.max_stages as f64,
                            ani_goto_data.start_crd.dec,
                            ani_goto_data.end_crd.dec
                        );
                        let observer = self_.observer.borrow();
                        let time = self_.time.borrow();
                        let cvt = EqToHorizCvt::new(&observer, &time);
                        let horiz_coord = cvt.eq_to_horiz(&EqCoord { dec, ra });
                        self_.view_point.borrow_mut().crd = horiz_coord;
                        let has_to_stop = ani_goto_data.stage == ani_goto_data.max_stages;
                        if !has_to_stop {
                            ani_goto_data.stage += 1;
                        } else {
                            *self_.center_crd.borrow_mut() = Some(ani_goto_data.end_crd.clone());
                        }
                        self_.draw_area.queue_draw();
                        has_to_stop
                    } else {
                        true
                    };
                    match has_to_stop {
                        false => glib::ControlFlow::Continue,
                        true => {
                            *ani_goto_data = None;
                            glib::ControlFlow::Break
                        }
                    }
                }
            ));
        }
    }
}
