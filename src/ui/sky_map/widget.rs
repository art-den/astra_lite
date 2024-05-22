use std::{cell::RefCell, rc::Rc};
use chrono::{NaiveDateTime, Utc};
use gtk::{gdk, glib::{self, clone}, prelude::*};
use super::{consts::*, data::*, painter::*, utils::*};

struct MousePressedData {
    hcrd:  HorizCoord,
    point: (f64, f64),
    vp:    ViewPoint,
}

pub struct SkymapWidget {
    skymap:     RefCell<Option<Rc<SkyMap>>>,
    evt_box:    gtk::EventBox,
    draw_area:  gtk::DrawingArea,
    view_point: RefCell<ViewPoint>,
    config:     RefCell<PaintConfig>,
    mpress:     RefCell<Option<MousePressedData>>,
    observer:   RefCell<Observer>,
    time:       RefCell<NaiveDateTime>,
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
            skymap:     RefCell::new(None),
            evt_box:    evt_box.clone(),
            draw_area:  da.clone(),
            view_point: RefCell::new(ViewPoint::new()),
            config:     RefCell::new(PaintConfig::default()),
            mpress:     RefCell::new(None),
            observer:   RefCell::new(Observer::default()),
            time:       RefCell::new(Utc::now().naive_utc()),
        });

        da.connect_draw(
            clone!(@weak widget => @default-return glib::Propagation::Stop,
            move |da, ctx| {
                let skymap = widget.skymap.borrow();
                let vp = widget.view_point.borrow();
                let config = widget.config.borrow();
                let observer = widget.observer.borrow();
                let time = widget.time.borrow();

                let mut painter = SkyMapPainter::new();
                let screen = ScreenInfo::new(da);

                let timer = std::time::Instant::now();
                painter.paint(&skymap, &observer, &time, &config, &vp, &screen, ctx).unwrap();
                let paint_time = timer.elapsed().as_secs_f64();

                let fps = if paint_time != 0.0 { 1.0/paint_time } else { f64::NAN };
                let fps_str = format!("x{:.1}, {:.1} FPS", vp.mag_factor, fps);
                ctx.set_font_size(screen.dpmm_y * 3.0);
                let te = ctx.text_extents(&fps_str).unwrap();
                ctx.move_to(1.0, 1.0 + te.height());
                ctx.set_source_rgb(1.0, 1.0, 1.0);
                ctx.show_text(&fps_str).unwrap();

                glib::Propagation::Stop
            })
        );

        evt_box.connect_button_press_event(
            clone!(@weak widget => @default-return glib::Propagation::Stop,
            move |_, event| {
                let mut mpress = widget.mpress.borrow_mut();
                let point = event.coords().unwrap_or_default();
                let (x, y) = event.coords().unwrap_or_default();
                let vp = widget.view_point.borrow();
                let si = ScreenInfo::new(&widget.draw_area);
                let cvt = HorizToScreenCvt::new(&*vp);
                let Some(hcrd) = cvt.screen_to_sphere(&Point2D {x, y}, &si) else {
                    return glib::Propagation::Stop;
                };
                *mpress = Some(MousePressedData {
                    hcrd,
                    point,
                    vp: widget.view_point.borrow().clone(),
                });
                glib::Propagation::Stop
            }
        ));

        evt_box.connect_motion_notify_event(
            clone!(@weak widget => @default-return glib::Propagation::Stop,
            move |_, event| {

                let mpress = widget.mpress.borrow();
                let Some(mpress) = &*mpress else {
                    return glib::Propagation::Proceed;
                };

                let (x, y) = event.coords().unwrap_or_default();

                let si = ScreenInfo::new(&widget.draw_area);
                let cvt = HorizToScreenCvt::new(&mpress.vp);
                let Some(hcrd) = cvt.screen_to_sphere(&Point2D {x, y}, &si) else {
                    return glib::Propagation::Stop;
                };

                let mut vp = widget.view_point.borrow_mut();
                vp.crd.az = mpress.vp.crd.az + mpress.hcrd.az - hcrd.az;

                vp.crd.alt = mpress.vp.crd.alt + mpress.hcrd.alt - hcrd.alt;
                vp.crd.alt = vp.crd.alt.min(MAX_ALT).max(MIN_ALT);

                widget.draw_area.queue_draw();
                glib::Propagation::Stop
            }
        ));

        evt_box.connect_button_release_event(
        clone!(@weak widget => @default-return glib::Propagation::Stop,
            move |_, _| {
                *widget.mpress.borrow_mut() = None;
                glib::Propagation::Stop
            }
        ));

        evt_box.set_events(
            gdk::EventMask::SCROLL_MASK |
            gdk::EventMask::POINTER_MOTION_MASK
        );
        evt_box.connect_scroll_event(
            clone!(@weak widget => @default-return glib::Propagation::Stop,
                move |_, event| {
                    if event.event_type() != gdk::EventType::Scroll {
                        return glib::Propagation::Stop;
                    }
                    let mut vp = widget.view_point.borrow_mut();
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
                        widget.draw_area.queue_draw();
                    }
                    glib::Propagation::Stop
                }
            )
        );

        widget
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

    pub fn set_time(&self, time: NaiveDateTime) {
        *self.time.borrow_mut() = time;
        self.draw_area.queue_draw();
    }

    pub fn set_paint_config(&self, config: &PaintConfig) {
        *self.config.borrow_mut() = config.clone();
        self.draw_area.queue_draw();
    }
}
