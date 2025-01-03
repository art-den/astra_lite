use std::{cell::{Cell, RefCell}, collections::HashMap, rc::Rc, sync::{Arc, RwLock}};
use chrono::{prelude::*, Days, Duration, Months};
use serde::{Serialize, Deserialize};
use gtk::{cairo, gdk, glib::{self, clone}, prelude::*};
use crate::{
    core::{core::*, events::*, frame_processing::*, mode_goto::GotoConfig},
    indi::{self, degree_to_str, hour_to_str},
    options::*,
    plate_solve::PlateSolveOkResult,
    utils::{gtk_utils::{self, *}, io_utils::*},
};
use super::{sky_map::{alt_widget::paint_altitude_by_time, data::*, math::*, painter::*}, ui_main::*, ui_skymap_options::SkymapOptionsDialog, utils::*};
use super::sky_map::{data::Observer, widget::SkymapWidget};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    main_ui:  &Rc<MainUi>,
    core:     &Arc<Core>,
    options:  &Arc<RwLock<Options>>,
    indi:     &Arc<indi::Connection>,
    handlers: &mut MainUiEventHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut ui_options, MapUi::CONF_FN)?;
        Ok(())
    });

    let pan_map1 = builder.object::<gtk::Paned>("pan_map1").unwrap();
    let map_widget = SkymapWidget::new();
    pan_map1.add2(map_widget.get_widget());

    let data = Rc::new(MapUi {
        ui_options:    RefCell::new(ui_options),
        core:          Arc::clone(core),
        indi:          Arc::clone(indi),
        options:       Arc::clone(options),
        builder:       builder.clone(),
        window:        window.clone(),
        main_ui:       Rc::clone(main_ui),
        excl:          ExclusiveCaller::new(),
        skymap_data:   RefCell::new(None),
        user_time:     RefCell::new(UserTime::default()),
        prev_second:   Cell::new(0),
        paint_ts:      RefCell::new(std::time::Instant::now()),
        prev_wdt:      RefCell::new(PrevWidgetsDT::default()),
        selected_item: RefCell::new(None),
        search_result: RefCell::new(Vec::new()),
        clicked_crd:   RefCell::new(None),
        full_screen:   Cell::new(false),
        goto_started:  Cell::new(false),
        closed:        Cell::new(false),
        cam_rotation:  RefCell::new(HashMap::new()),
        ps_img:        RefCell::new(None),
        ps_result:     RefCell::new(None),
        self_:         RefCell::new(None),
        map_widget,
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_widgets();
    data.init_search_result_treeview();
    data.show_options();
    data.update_widgets_enable_state();

    data.connect_main_ui_events(handlers);
    data.connect_widgets_events();
    data.connect_core_events();

    data.set_observer_data_for_widget();
}

enum MainThreadEvent {
    Core(Event),
}

impl SkyItemType {
    fn to_str(self) -> &'static str {
        match self {
            SkyItemType::None                 => "",
            SkyItemType::Star                 => "Star",
            SkyItemType::DoubleStar           => "Double Star",
            SkyItemType::Galaxy               => "Galaxy",
            SkyItemType::StarCluster          => "Star Cluster",
            SkyItemType::PlanetaryNebula      => "Planetary Nebula",
            SkyItemType::DarkNebula           => "Dark Nebula",
            SkyItemType::EmissionNebula       => "Emission Nebula",
            SkyItemType::Nebula               => "Nebula",
            SkyItemType::ReflectionNebula     => "Reflection Nebula",
            SkyItemType::HIIIonizedRegion     => "Hii Ionized Region",
            SkyItemType::SupernovaRemnant     => "Supernova Remnant",
            SkyItemType::GalaxyPair           => "Galaxy Pair",
            SkyItemType::GalaxyTriplet        => "Galaxy Triplet",
            SkyItemType::GroupOfGalaxies      => "Group of Galaxies",
            SkyItemType::AssociationOfStars   => "Association of Stars",
            SkyItemType::StarClusterAndNebula => "Star Cluster and Nebula",
        }
    }
}


#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct UiOptions {
    paned_pos1: i32,
    pub paint:  PaintConfig,
    show_ccd:   bool,
    show_ps:    bool,
    exp_dt:     bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            paned_pos1: -1,
            paint:      PaintConfig::default(),
            show_ccd:   true,
            exp_dt:     true,
            show_ps:    true,
        }
    }
}

enum UserTime {
    Paused(NaiveDateTime),
    Active(i64/* diff in seconds with now */),
}

impl Default for UserTime {
    fn default() -> Self {
        Self::Active(0)
    }
}

impl UserTime {
    fn time(&self, local: bool) -> NaiveDateTime {
        let time = match self {
            UserTime::Paused(time) =>
                time.clone(),
            UserTime::Active(diff) => {
                let mut now = Utc::now();
                let diff_duration = chrono::Duration::seconds(*diff as i64);
                now = now.checked_add_signed(diff_duration).unwrap_or(now);
                now.naive_utc()
            },
        };
        if local {
            let local_dt = Local.from_utc_datetime(&time);
            local_dt.naive_local()
        } else {
            time
        }
    }

    fn pause(&mut self, pause: bool) {
        let already_paused = matches!(&self, UserTime::Paused(_));
        if pause == already_paused {
            return;
        }
        if pause {
            let time = self.time(false);
            *self = Self::Paused(time);
        } else {
            let time = self.time(false);
            let now = Utc::now().naive_utc();
            let diff = time - now;
            *self = Self::Active(diff.num_seconds());
        }
    }

    fn set_time(&mut self, utc_dt: NaiveDateTime) {
        match self {
            Self::Active(diff_in_seconds) => {
                let now = Utc::now().naive_utc();
                let diff = utc_dt - now;
                *diff_in_seconds = diff.num_seconds();
            },
            Self::Paused(paused_dt) => {
                *paused_dt = utc_dt;
            },
        }
    }

    fn now(&self) -> bool {
        if let Self::Active(diff_in_seconds) = &self {
            *diff_in_seconds == 0
        } else {
            false
        }
    }

    fn set_now(&mut self) {
        match self {
            Self::Active(diff_in_seconds) => {
                *diff_in_seconds = 0;
            },
            Self::Paused(paused_dt) => {
                *paused_dt = Utc::now().naive_utc();
            },
        }
    }
}

#[derive(Default)]
struct PrevWidgetsDT {
    year: i32,
    mon: i32,
    day: i32,
    hour: i32,
    min: i32,
    sec: i32,
}

struct FoundItem {
    obj: SkymapObject,
    above_horiz: bool,
}

struct MapUi {
    ui_options:    RefCell<UiOptions>,
    core:          Arc<Core>,
    indi:          Arc<indi::Connection>,
    options:       Arc<RwLock<Options>>,
    builder:       gtk::Builder,
    window:        gtk::ApplicationWindow,
    main_ui:       Rc<MainUi>,
    excl:          ExclusiveCaller,
    map_widget:    Rc<SkymapWidget>,
    skymap_data:   RefCell<Option<Rc<SkyMap>>>,
    user_time:     RefCell<UserTime>,
    prev_second:   Cell<u32>,
    paint_ts:      RefCell<std::time::Instant>, // last paint moment timestamp
    prev_wdt:      RefCell<PrevWidgetsDT>,
    selected_item: RefCell<Option<SkymapObject>>,
    search_result: RefCell<Vec<FoundItem>>,
    clicked_crd:   RefCell<Option<EqCoord>>,
    full_screen:   Cell<bool>,
    goto_started:  Cell<bool>,
    closed:        Cell<bool>,
    cam_rotation:  RefCell<HashMap<String, f64>>,
    ps_img:        RefCell<Option<gdk::gdk_pixbuf::Pixbuf>>,
    ps_result:     RefCell<Option<PlateSolveOkResult>>,
    self_:         RefCell<Option<Rc<MapUi>>>
}

impl Drop for MapUi {
    fn drop(&mut self) {
        log::info!("MapUi dropped");
    }
}

impl MapUi {
    const CONF_FN: &'static str = "ui_skymap";

    fn handler_main_ui_event(self: &Rc<Self>, event: UiEvent) {
        match event {
            UiEvent::ProgramClosing =>
                self.handler_closing(),

            UiEvent::Timer =>
                self.handler_main_timer(),

            UiEvent::TabPageChanged(page) if page == TabPage::SkyMap => {
                let mut options = self.options.write().unwrap();
                options.read_site(&self.builder);
                drop(options);

                self.check_data_loaded();
                self.set_observer_data_for_widget();
                self.update_date_time_widgets(true);
                self.update_skymap_widget(true);
                self.show_selected_objects_info();
            }

            UiEvent::FullScreen(full_screen) =>
                self.set_full_screen_mode(full_screen),

            _ => {},
        }
    }

    fn handler_closing(&self) {
        self.closed.set(true);

        self.read_ui_options_from_widgets();

        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);

        *self.self_.borrow_mut() = None;
    }

    fn init_widgets(&self) {
        let set_range = |widget_name, min, max| {
            let spb = self.builder.object::<gtk::SpinButton>(widget_name).unwrap();
            spb.set_range(min - 1.0, max + 1.0);
            spb.set_increments(1.0, 1.0);
        };

        set_range("spb_year", 0.0, 3000.0);
        set_range("spb_mon", 1.0, 12.0);
        set_range("spb_day", 1.0, 31.0);
        set_range("spb_hour", 0.0, 24.0);
        set_range("spb_min", 0.0, 60.0);
        set_range("spb_sec", 0.0, 60.0);

        let scl_max_dso_mag = self.builder.object::<gtk::Scale>("scl_max_dso_mag").unwrap();
        scl_max_dso_mag.set_range(0.0, 20.0);
        scl_max_dso_mag.set_increments(0.5, 2.0);

        let (dpimm_x, dpimm_y) = gtk_utils::get_widget_dpmm(&self.window)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        scl_max_dso_mag.set_width_request((40.0 * dpimm_x) as i32);

        let da_sm_item_graph = self.builder.object::<gtk::DrawingArea>("da_sm_item_graph").unwrap();
        da_sm_item_graph.set_height_request((30.0 * dpimm_y) as i32);
    }

    fn connect_main_ui_events(self: &Rc<Self>, handlers: &mut MainUiEventHandlers) {
        handlers.subscribe(clone!(@weak self as self_ => move |event| {
            self_.handler_main_ui_event(event);
        }));
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action   (&self.window, self, "map_play",          Self::handler_btn_play_pressed);
        gtk_utils::connect_action   (&self.window, self, "map_now",           Self::handler_btn_now_pressed);
        gtk_utils::connect_action_rc(&self.window, self, "skymap_options",    Self::handler_action_options);
        gtk_utils::connect_action_rc(&self.window, self, "sm_goto_selected",  Self::handler_goto_selected);
        gtk_utils::connect_action_rc(&self.window, self, "sm_goto_sel_solve", Self::handler_goto_sel_and_solve);
        gtk_utils::connect_action_rc(&self.window, self, "sm_goto_point",     Self::handler_goto_point);

        let connect_spin_btn_evt = |widget_name: &str| {
            let spin_btn = self.builder.object::<gtk::SpinButton>(widget_name).unwrap();
            spin_btn.connect_value_changed(clone!(@weak self as self_ => move |_| {
                self_.handler_time_changed();
            }));
        };

        connect_spin_btn_evt("spb_year");
        connect_spin_btn_evt("spb_mon");
        connect_spin_btn_evt("spb_day");
        connect_spin_btn_evt("spb_hour");
        connect_spin_btn_evt("spb_min");
        connect_spin_btn_evt("spb_sec");

        let scl_max_dso_mag = self.builder.object::<gtk::Scale>("scl_max_dso_mag").unwrap();
        scl_max_dso_mag.connect_value_changed(clone!(@weak self as self_ => move |scale| {
            self_.handler_max_magnitude_changed(scale.value());
        }));

        let connect_obj_visibility_changed = |widget_name: &str| {
            let ch = self.builder.object::<gtk::CheckButton>(widget_name).unwrap();
            ch.connect_active_notify(clone!(@weak self as self_ => move |_| {
                self_.handler_obj_visibility_changed();
            }));
        };

        connect_obj_visibility_changed("chb_show_stars");
        connect_obj_visibility_changed("chb_show_dso");
        connect_obj_visibility_changed("chb_show_galaxies");
        connect_obj_visibility_changed("chb_show_nebulas");
        connect_obj_visibility_changed("chb_show_sclusters");
        connect_obj_visibility_changed("chb_sm_show_eq_grid");
        connect_obj_visibility_changed("chb_sm_show_ccd");
        connect_obj_visibility_changed("chb_sm_show_ps");

        self.map_widget.add_obj_sel_handler(
            clone!(@weak self as self_ => move |object| {
                self_.handler_object_selected(object);
            })
        );

        let se = self.builder.object::<gtk::SearchEntry>("se_sm_search").unwrap();
        se.connect_search_changed(
            clone!(@weak self as self_ => move |se| {
                self_.handler_search_text_changed(se);
            })
        );

        let search_tv = self.builder.object::<gtk::TreeView>("tv_sm_search_result").unwrap();
        search_tv.selection().connect_changed(
            clone!( @weak self as self_ => move |selection| {
                self_.handler_search_result_selection_changed(selection);
            })
        );

        let da_sm_item_graph = self.builder.object::<gtk::DrawingArea>("da_sm_item_graph").unwrap();
        da_sm_item_graph.connect_draw(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |area, cr| {
                gtk_utils::exec_and_show_error(&self_.window, || {
                    self_.handler_draw_item_graph(area, cr)?;
                    Ok(())
                });
                glib::Propagation::Proceed
            })
        );

        self.map_widget.get_widget().connect_button_press_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
                move |_, evt| {
                    self_.handler_widget_mouse_button_press(evt)
                })
        );

        self.window.add_events(gdk::EventMask::KEY_PRESS_MASK);
        self.window.connect_key_press_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, event| {
                let nb_main = self_.builder.object::<gtk::Notebook>("nb_main").unwrap();
                if nb_main.page() == TAB_MAP as i32 {
                    return self_.handler_key_press_event(event);
                }
                glib::Propagation::Proceed
            }
        ));
    }

    fn connect_core_events(self: &Rc<Self>) {
        let (main_thread_sender, main_thread_receiver) = async_channel::unbounded();

        let sender = main_thread_sender.clone();
        self.core.event_subscriptions().subscribe(move |event| {
            sender.send_blocking(MainThreadEvent::Core(event)).unwrap();
        });

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(event) = main_thread_receiver.recv().await {
                if self_.closed.get() { return; }
                match event {
                    MainThreadEvent::Core(event) =>
                        self_.process_core_event(event),
                }
            }
        }));
    }

    fn process_core_event(&self, event: Event) {
        match event {
            Event::PlateSolve(ps_event) => {
                let mut cam_rotation = self.cam_rotation.borrow_mut();
                cam_rotation.insert(ps_event.cam_name, ps_event.result.rotation);
                drop(cam_rotation);

                *self.ps_result.borrow_mut() = Some(ps_event.result.clone());

                self.update_skymap_widget(true);
            }
            Event::FrameProcessing(sp) => {
                match (&sp.data, sp.mode_type) {
                    (FrameProcessResultData::PreviewFrame(data),
                     ModeType::Goto|ModeType::OpeningImgFile|ModeType::CapturePlatesolve) => {
                        self.create_plate_solve_preview(data);
                    },
                    _ => {},
                }
            }
            _ => {},
        }
    }

    fn show_options(&self) {
        let pan_map1 = self.builder.object::<gtk::Paned>("pan_map1").unwrap();
        let opts = self.ui_options.borrow();
        pan_map1.set_position(opts.paned_pos1);
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        ui.set_prop_bool("chb_show_stars.active", opts.paint.filter.contains(ItemsToShow::STARS));
        ui.set_prop_bool("chb_show_dso.active", opts.paint.filter.contains(ItemsToShow::DSO));
        ui.set_prop_bool("chb_show_galaxies.active", opts.paint.filter.contains(ItemsToShow::GALAXIES));
        ui.set_prop_bool("chb_show_nebulas.active", opts.paint.filter.contains(ItemsToShow::NEBULAS));
        ui.set_prop_bool("chb_show_sclusters.active", opts.paint.filter.contains(ItemsToShow::CLUSTERS));
        ui.set_prop_bool("chb_sm_show_eq_grid.active", opts.paint.eq_grid.visible);
        ui.set_prop_bool("chb_sm_show_ccd.active", opts.show_ccd);
        ui.set_prop_bool("chb_sm_show_ps.active", opts.show_ps);
        ui.set_range_value("scl_max_dso_mag", opts.paint.max_dso_mag as f64);
        ui.set_prop_bool("exp_sm_dt.expanded", opts.exp_dt);

        drop(opts);
    }

    fn read_ui_options_from_widgets(&self) {
        let pan_map1 = self.builder.object::<gtk::Paned>("pan_map1").unwrap();
        let mut opts = self.ui_options.borrow_mut();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        if !self.full_screen.get() {
            opts.paned_pos1 = pan_map1.position();
        }
        opts.paint.max_dso_mag = ui.range_value("scl_max_dso_mag") as f32;
        opts.exp_dt = ui.prop_bool("exp_sm_dt.expanded");

        Self::read_visibility_options_from_widgets(&mut opts, &ui);

        drop(opts);
    }

    fn read_visibility_options_from_widgets(opts: &mut UiOptions, ui: &gtk_utils::UiHelper) {
        opts.paint.filter.set(ItemsToShow::STARS, ui.prop_bool("chb_show_stars.active"));
        opts.paint.filter.set(ItemsToShow::DSO, ui.prop_bool("chb_show_dso.active"));
        opts.paint.filter.set(ItemsToShow::GALAXIES, ui.prop_bool("chb_show_galaxies.active"));
        opts.paint.filter.set(ItemsToShow::NEBULAS, ui.prop_bool("chb_show_nebulas.active"));
        opts.paint.filter.set(ItemsToShow::CLUSTERS, ui.prop_bool("chb_show_sclusters.active"));

        opts.paint.eq_grid.visible = ui.prop_bool("chb_sm_show_eq_grid.active");
        opts.show_ccd = ui.prop_bool("chb_sm_show_ccd.active");
        opts.show_ps = ui.prop_bool("chb_sm_show_ps.active");
    }

    fn handler_action_options(self: &Rc<Self>) {
        let dialog = SkymapOptionsDialog::new(
            self.window.upcast_ref(),
        );

        let ui_options = self.ui_options.borrow();
        dialog.show_options(&ui_options);
        drop(ui_options);

        dialog.exec(clone!(@strong self as self_, @strong dialog => move || {
            let mut ui_options = self_.ui_options.borrow_mut();
            dialog.get_options(&mut ui_options)?;
            drop(ui_options);
            let observer = self_.create_observer();
            self_.map_widget.set_observer(&observer);
            self_.update_skymap_widget(true);
            Ok(())
        }));
    }

    fn create_observer(&self) -> Observer {
        let sky_map_options = self.options.read().unwrap().site.clone();
        Observer {
            latitude: degree_to_radian(sky_map_options.latitude),
            longitude: degree_to_radian(sky_map_options.longitude),
        }
    }

    fn set_observer_data_for_widget(&self) {
        let observer = self.create_observer();
        self.map_widget.set_observer(&observer);
    }

    fn handler_main_timer(&self) {
        if self.main_ui.current_tab_page() != TabPage::SkyMap {
            return;
        }

        // Change time in widget if second is changed
        self.update_date_time_widgets(false);

        // Update map 2 times per second
        self.update_skymap_widget(false);

        self.show_selected_objects_info();
    }

    fn update_date_time_widgets(&self, force: bool) {
        self.excl.exec(|| {
            let user_time = self.user_time.borrow();
            let cur_time = user_time.time(true);
            let second = cur_time.second();
            if force || second != self.prev_second.get() {
                self.prev_second.set(second);
                self.set_time_to_widgets_impl();
            }
        });
    }

    fn update_skymap_widget(&self, force: bool) {
        let mut paint_ts = self.paint_ts.borrow_mut();
        if force || paint_ts.elapsed().as_secs_f64() > 0.5 {
            let user_time = self.user_time.borrow().time(false);
            *paint_ts = std::time::Instant::now();

            let config = self.ui_options.borrow();
            let show_ccd = config.show_ccd;
            let show_ps_image = config.show_ps;
            let paint_config = config.paint.clone();
            drop(config);

            let indi_is_connected = self.indi.state() == indi::ConnState::Connected;

            let cam_frame = if show_ccd && indi_is_connected {
                || -> anyhow::Result<CameraFrame> {
                    let options = self.options.read().unwrap();
                    let Some(device) = &options.cam.device else { anyhow::bail!("Camera is not selected"); };
                    let cam_name = &device.name;
                    let cam_ccd_prop = &device.prop;
                    let cam_ccd = indi::CamCcd::from_ccd_prop_name(cam_ccd_prop);
                    let focal_len = options.telescope.real_focal_length();
                    if focal_len <= 0.1 {
                        anyhow::bail!("Wrong telescope focal lenght");
                    }
                    let (sensor_width, sensor_height) = self.indi.camera_get_max_frame_size(&cam_name, cam_ccd)?;
                    let (pixel_width_um, pixel_height_um) = self.indi.camera_get_pixel_size_um(&cam_name, cam_ccd)?;
                    let (width_mm, height_mm) = options.cam.calc_active_zone_mm(
                        sensor_width, sensor_height,
                        pixel_width_um, pixel_height_um
                    );
                    let mut full_cam_name = cam_name.to_string();
                    if !cam_ccd_prop.is_empty() {
                        full_cam_name += ", ";
                        full_cam_name += cam_ccd_prop;
                    }
                    let cam_rotation = self.cam_rotation.borrow();
                    let rot_angle = *cam_rotation.get(&device.name).unwrap_or(&0.0);
                    drop(cam_rotation);

                    Ok(CameraFrame{
                        name: full_cam_name,
                        horiz_angle: f64::atan2(width_mm, focal_len),
                        vert_angle: f64::atan2(height_mm, focal_len),
                        rot_angle,
                    })
                } ().ok()
            } else {
                None
            };

            let telescope_pos = if indi_is_connected {
                let tele_pos_fun = || -> anyhow::Result<EqCoord> {
                    let options = self.options.read().unwrap();
                    let device_name = &options.mount.device;
                    let (ra, dec) = self.indi.mount_get_eq_ra_and_dec(device_name)?;
                    Ok(EqCoord {
                        ra: hour_to_radian(ra),
                        dec: degree_to_radian(dec),
                    })
                };
                tele_pos_fun().ok()
            } else {
                None
            };

            let ps_img = self.ps_img.borrow();
            let ps_result = self.ps_result.borrow();
            let solved_image =
                if let (Some(solved_img), Some(ps_result), true)
                = (&*ps_img, &*ps_result, show_ps_image) {
                    Some(PlateSolvedImage {
                        image:       solved_img.clone(),
                        coord:       ps_result.crd_now.clone(),
                        horiz_angle: ps_result.width,
                        vert_angle:  ps_result.height,
                        rot_angle:   ps_result.rotation,
                        time:        ps_result.time,
                    })
                } else {
                    None
                };
            drop(ps_result);
            drop(ps_img);

            self.map_widget.set_paint_config(
                &user_time,
                &paint_config,
                &telescope_pos,
                &cam_frame,
                &solved_image
            );
        }
    }

    fn set_time_to_widgets_impl(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let user_time = self.user_time.borrow();
        let cur_dt = user_time.time(true);

        ui.set_prop_f64("spb_year.value", cur_dt.year() as f64);
        ui.set_prop_f64("spb_mon.value",  cur_dt.month() as f64);
        ui.set_prop_f64("spb_day.value",  cur_dt.day() as f64);
        ui.set_prop_f64("spb_hour.value", cur_dt.hour() as f64);
        ui.set_prop_f64("spb_min.value",  cur_dt.minute() as f64);
        ui.set_prop_f64("spb_sec.value",  cur_dt.second() as f64);

        gtk_utils::enable_action(&self.window, "map_now", !user_time.now());

        let prev = PrevWidgetsDT {
            year: cur_dt.year(),
            mon: cur_dt.month() as i32,
            day: cur_dt.day() as i32,
            hour: cur_dt.hour() as i32,
            min: cur_dt.minute() as i32,
            sec: cur_dt.second() as i32,
        };
        *self.prev_wdt.borrow_mut() = prev;
    }

    fn check_data_loaded(self: &Rc<Self>) {
        gtk_utils::exec_and_show_error(&self.window, || {
            let result = self.check_data_loaded_impl();
            if let Err(_) = result {
                *self.skymap_data.borrow_mut() = Some(Rc::new(SkyMap::new()));
            }
            result
        });
    }

    fn check_data_loaded_impl(self: &Rc<Self>) -> anyhow::Result<()> {
        let mut skymap = self.skymap_data.borrow_mut();
        if skymap.is_some() {
            return Ok(());
        }
        let mut map = SkyMap::new();

        let cur_exe = std::env::current_exe()?;

        let cur_path = cur_exe.parent()
            .ok_or_else(|| anyhow::anyhow!("Error getting cur_exe.parent()"))?;
        let skymap_data_path = cur_path.join("data");

        let skymap_local_data_path = dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("dirs::data_local_dir"))?
            .join(env!("CARGO_PKG_NAME"))
            .join("data");

        const DSO_FILE: &str = "dso.csv";
        map.load_dso(skymap_local_data_path.join(DSO_FILE)).
            or_else(|_| {
                map.load_dso(skymap_data_path.join(DSO_FILE))
            })?;

        const NAMED_STARS_FILE: &str = "named_stars.csv";
        map.load_named_stars(skymap_local_data_path.join(NAMED_STARS_FILE))
            .or_else(|_| {
                map.load_named_stars(skymap_data_path.join(NAMED_STARS_FILE))
            })?;

        let map = Rc::new(map);
        *skymap = Some(Rc::clone(&map));
        drop(skymap);

        self.map_widget.set_skymap(&map);

        // Load stars in another thread

        let (stars_sender, stars_receiver) = async_channel::unbounded();

        std::thread::spawn(move || {
            let mut stars_map = SkyMap::new();
            const STARS_FILE: &str = "stars.bin";
            let res = stars_map.load_stars(skymap_local_data_path.join(STARS_FILE))
                .or_else(|_| {
                    stars_map.load_stars(skymap_data_path.join(STARS_FILE))
                });
            if let Err(err) = res {
                stars_sender.send_blocking(Err(err)).unwrap();
                return;
            }
            stars_sender.send_blocking(Ok(stars_map)).unwrap();
        });

        glib::spawn_future_local(clone!(@weak self as self_ => async move {
            while let Ok(skymaps_with_stars_res) = stars_receiver.recv().await {
                match skymaps_with_stars_res {
                    Ok(mut skymaps_with_stars) => {
                        let mut skymap_data_opt = self_.skymap_data.borrow_mut();
                        let Some(skymap_data) = &*skymap_data_opt else { return; };
                        skymaps_with_stars.merge_other_skymaps(skymap_data);
                        let skymaps_with_stars_rc = Rc::new(skymaps_with_stars);
                        *skymap_data_opt = Some(Rc::clone(&skymaps_with_stars_rc));
                        self_.map_widget.set_skymap(&skymaps_with_stars_rc);
                    }
                    Err(err) => {
                        gtk_utils::show_error_message(
                            &self_.window,
                            "Error loading stars data",
                            &err.to_string()
                        );
                    }
                }
            }
        }));

        Ok(())
    }

    fn handler_time_changed(&self) {
        self.excl.exec(|| {
            let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
            let prev_time = self.prev_wdt.borrow();
            let year_diff = ui.prop_f64("spb_year.value") as i32 - prev_time.year;
            let mon_diff = ui.prop_f64("spb_mon.value") as i32 - prev_time.mon;
            let day_diff = ui.prop_f64("spb_day.value") as i32 - prev_time.day;
            let hour_diff = ui.prop_f64("spb_hour.value") as i32 - prev_time.hour;
            let min_diff = ui.prop_f64("spb_min.value") as i32 - prev_time.min;
            let sec_diff = ui.prop_f64("spb_sec.value") as i32 - prev_time.sec;
            drop(prev_time);

            let mut user_time = self.user_time.borrow_mut();
            let mut time = user_time.time(false);

            time = time.with_year(time.year() + year_diff).unwrap_or(time);

            time = if mon_diff > 0 {
                time.checked_add_months(Months::new(mon_diff as u32)).unwrap_or(time)
            } else {
                time.checked_sub_months(Months::new(-mon_diff as u32)).unwrap_or(time)
            };

            time = if day_diff > 0 {
                time.checked_add_days(Days::new(day_diff as u64)).unwrap_or(time)
            } else {
                time.checked_sub_days(Days::new(-day_diff as u64)).unwrap_or(time)
            };

            let duration =
                Duration::hours(hour_diff as i64) +
                Duration::minutes(min_diff as i64) +
                Duration::seconds(sec_diff as i64);

            time += duration;

            user_time.set_time(time);
            drop(user_time);

            self.set_time_to_widgets_impl();
            self.update_skymap_widget(true);
            self.show_selected_objects_info();
        });
    }

    fn handler_btn_play_pressed(&self) {
        self.excl.exec(|| {
            let btn_play = self.builder.object::<gtk::ToggleButton>("btn_play").unwrap();
            let mut user_time = self.user_time.borrow_mut();
            user_time.pause(!btn_play.is_active());
            drop(user_time);
            self.set_time_to_widgets_impl();
        });
    }

    fn handler_btn_now_pressed(&self) {
        self.excl.exec(|| {
            let mut user_time = self.user_time.borrow_mut();
            user_time.set_now();
            drop(user_time);
            self.set_time_to_widgets_impl();
            self.update_skymap_widget(true);
            self.show_selected_objects_info();
        });
    }

    fn handler_max_magnitude_changed(&self, value: f64) {
        self.excl.exec(|| {
            let value = value as f32;
            let mut options = self.ui_options.borrow_mut();
            if options.paint.max_dso_mag == value {
                return;
            }
            options.paint.max_dso_mag = value;
            drop(options);

            self.update_skymap_widget(true);
        });
    }

    fn handler_obj_visibility_changed(&self) {
        let mut opts = self.ui_options.borrow_mut();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        Self::read_visibility_options_from_widgets(&mut opts, &ui);
        drop(opts);

        self.update_widgets_enable_state();
        self.update_skymap_widget(true);
    }

    fn update_widgets_enable_state(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let dso_enabled = ui.prop_bool("chb_show_dso.active");
        ui.enable_widgets(false, &[
            ("chb_show_galaxies", dso_enabled),
            ("chb_show_nebulas", dso_enabled),
            ("chb_show_sclusters", dso_enabled),
        ]);
    }

    fn handler_object_selected(&self, obj: Option<SkymapObject>) {
        *self.selected_item.borrow_mut() = obj;
        self.show_selected_objects_info();
        self.update_selected_item_graph();
    }

    fn show_selected_objects_info(&self) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        let obj = self.selected_item.borrow();

        let mut names = String::new();
        let mut nicknames = String::new();
        let mut obj_type_str = "";
        let mut mag_cap_str = "";
        let mut mag_str = String::new();
        let mut bv_str = String::new();
        let mut ra_str = String::new();
        let mut dec_str = String::new();
        let mut ra_now_str = String::new();
        let mut dec_now_str = String::new();

        let mut zenith_str = String::new();
        let mut azimuth_str = String::new();

        if let Some(obj) = &*obj {
            names = obj.names().join(", ");
            nicknames = obj.nicknames().join(", ");

            obj_type_str = obj.obj_type().to_str();

            if let Some(mag) = obj.mag_v() {
                mag_str = format!("{:.2}", mag);
            } else if let Some(mag) = obj.mag_b() {
                mag_str = format!("{:.2}", mag);
            }

            if obj.mag_v().is_some() {
                mag_cap_str = "Mag. (V)";
            } else if obj.mag_b().is_some() {
                mag_cap_str = "Mag. (B)";
            } else {
                mag_cap_str = "Mag.";
            }

            bv_str = obj.bv().map(|bv| format!("{:.2}", bv)).unwrap_or_default();

            let j2000 = j2000_time();
            let time = self.map_widget.time();
            let epoch_cvt = EpochCvt::new(&j2000, &time);

            let crd = obj.crd();

            ra_str = hour_to_str(radian_to_hour(crd.ra));
            dec_str = degree_to_str(radian_to_degree(crd.dec));

            let now_crd = epoch_cvt.convert_eq(&crd);
            ra_now_str = hour_to_str(radian_to_hour(now_crd.ra));
            dec_now_str = degree_to_str(radian_to_degree(now_crd.dec));

            let observer = self.create_observer();
            let cvt = EqToSphereCvt::new(observer.longitude, observer.latitude, &time);

            let h_crd = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&obj.crd()));

            zenith_str = degree_to_str(radian_to_degree(h_crd.alt));
            azimuth_str = degree_to_str(radian_to_degree(h_crd.az));
        }

        ui.set_prop_str("e_sm_sel_names.text", Some(&names));
        ui.set_prop_str("e_sm_sel_nicknames.text", Some(&nicknames));
        ui.set_prop_str("l_sm_sel_type.label", Some(&obj_type_str));
        ui.set_prop_str("l_sm_sel_mag_cap.label", Some(&mag_cap_str));
        ui.set_prop_str("l_sm_sel_mag.label", Some(&mag_str));
        ui.set_prop_str("l_sm_sel_bv.label", Some(&bv_str));
        ui.set_prop_str("l_sm_sel_ra.label", Some(&ra_str));
        ui.set_prop_str("l_sm_sel_dec.label", Some(&dec_str));
        ui.set_prop_str("l_sm_sel_ra_now.label", Some(&ra_now_str));
        ui.set_prop_str("l_sm_sel_dec_now.label", Some(&dec_now_str));
        ui.set_prop_str("l_sm_sel_zenith.label", Some(&zenith_str));
        ui.set_prop_str("l_sm_sel_az.label", Some(&azimuth_str));
    }

    fn init_search_result_treeview(&self) {
        let tv = self.builder.object::<gtk::TreeView>("tv_sm_search_result").unwrap();
        let columns = [
            /* 0 */ ("Name", String::static_type()),
            /* 1 */ ("Type", String::static_type()),
        ];
        let types = columns.iter().map(|(_, t)| *t).collect::<Vec<_>>();
        let model = gtk::ListStore::new(&types);
        for (idx, (col_name, _)) in columns.into_iter().enumerate() {
            let cell_text = gtk::CellRendererText::new();
            let col = gtk::TreeViewColumn::builder()
                .title(col_name)
                .resizable(true)
                .clickable(true)
                .visible(true)
                .build();
            TreeViewColumnExt::pack_start(&col, &cell_text, true);
            TreeViewColumnExt::add_attribute(&col, &cell_text, "markup", idx as i32);
            tv.append_column(&col);
        }
        tv.set_model(Some(&model));
    }

    fn handler_search_text_changed(&self, _se: &gtk::SearchEntry) {
        self.search();
    }

    pub fn search(&self) {
        let Some(skymap) = &*self.skymap_data.borrow() else { return; };
        let se_sm_search = self.builder.object::<gtk::SearchEntry>("se_sm_search").unwrap();
        let text = se_sm_search.text().trim().to_string();
        let found_items = skymap.search(&text);
        let observer = self.create_observer();
        let time = self.map_widget.time();
        let cvt = EqToSphereCvt::new(observer.longitude, observer.latitude, &time);
        let mut result = Vec::new();
        for obj in found_items {
            let hcrd = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&obj.crd()));
            result.push(FoundItem{ obj, above_horiz: hcrd.alt > 0.0 });
        }
        result.sort_by(|obj1, obj2| {
            match (obj1.above_horiz, obj2.above_horiz) {
                (true, true)|
                (false, false) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
            }
        });
        *self.search_result.borrow_mut() = result;
        self.show_search_result();
    }

    fn show_search_result(&self) {
        let tv = self.builder.object::<gtk::TreeView>("tv_sm_search_result").unwrap();
        let Some(model) = tv.model() else { return; };
        let Ok(model) = model.downcast::<gtk::ListStore>() else { return; };
        let result = self.search_result.borrow();
        model.clear();
        for item in &*result {
            let mut names_str = item.obj.names().join(", ");
            let mut type_str = item.obj.obj_type().to_str().to_string();
            let make_gray = |text: &mut String| {
                *text = format!(r##"<span color="gray">{}</span>"##, text);
            };
            if !item.above_horiz {
                make_gray(&mut names_str);
                make_gray(&mut type_str);
            }
            model.insert_with_values(None, &[(0, &names_str),(1, &type_str),]);
        }

        if !result.is_empty() {
            let selection = tv.selection();
            let mut path = gtk::TreePath::new();
            path.append_index(0);
            selection.select_path(&path);
        }
    }

    fn handler_key_press_event(&self, event: &gdk::EventKey) -> glib::Propagation {
        let se_sm_search = self.builder.object::<gtk::SearchEntry>("se_sm_search").unwrap();

        if event.state().contains(gdk::ModifierType::CONTROL_MASK)
        && matches!(event.keyval(), gdk::keys::constants::F|gdk::keys::constants::f) {
            se_sm_search.grab_focus();
            return glib::Propagation::Stop;
        }

        if matches!(event.keyval(), gdk::keys::constants::Up|gdk::keys::constants::Down)
        && se_sm_search.has_focus() {
            let tv = self.builder.object::<gtk::TreeView>("tv_sm_search_result").unwrap();
            let Some(model) = tv.model() else {
                return glib::Propagation::Proceed;
            };

            let result_count = gtk_utils::get_model_row_count(&model);
            if result_count == 0 {
                return glib::Propagation::Proceed;
            }
            let selection = tv.selection();
            let path = selection.selected_rows().0;
            if path.len() != 1 {
                return glib::Propagation::Proceed;
            }
            let indices = path[0].clone().indices();
            if indices.len() != 1 {
                return glib::Propagation::Proceed;
            }
            let mut cur_index = indices[0] as i32;
            match event.keyval() {
                gdk::keys::constants::Up => cur_index -= 1,
                gdk::keys::constants::Down => cur_index += 1,
                _ => unreachable!(),
            }
            if cur_index < 0 || cur_index >= result_count as i32 {
                return glib::Propagation::Stop;
            }
            let mut path = gtk::TreePath::new();
            path.append_index(cur_index);
            selection.select_path(&path);
            if let [path] = selection.selected_rows().0.as_slice() {
                tv.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
            }

            return glib::Propagation::Stop;
        }

        glib::Propagation::Proceed
    }

    pub fn handler_search_result_selection_changed(&self, selection: &gtk::TreeSelection) {
        let items = selection
            .selected_rows().0
            .iter()
            .flat_map(|path| path.indices())
            .collect::<Vec<_>>();

        let &[index] = items.as_slice() else { return; };
        let index = index as usize;
        let found_items = self.search_result.borrow();
        let Some(selected_obj) = found_items.get(index) else { return; };
        *self.selected_item.borrow_mut() = Some(selected_obj.obj.clone());
        self.show_selected_objects_info();
        self.update_selected_item_graph();
        self.map_widget.set_selected_object(Some(&selected_obj.obj));
    }

    fn update_selected_item_graph(&self) {
        let da_sm_item_graph = self.builder.object::<gtk::DrawingArea>("da_sm_item_graph").unwrap();
        da_sm_item_graph.queue_draw();
    }

    fn handler_draw_item_graph(
        &self,
        area: &gtk::DrawingArea,
        cr:   &cairo::Context
    ) -> anyhow::Result<()> {
        let user_time = self.user_time.borrow();
        let cur_dt = user_time.time(false);
        let cur_dt_local = user_time.time(true);
        drop(user_time);

        let selected_item = self.selected_item.borrow();
        let crd = selected_item.as_ref().map(|item| item.crd());
        drop(selected_item);
        let observer = self.create_observer();

        paint_altitude_by_time(area, cr, cur_dt, cur_dt_local, &observer, &crd)?;
        Ok(())
    }

    fn handler_widget_mouse_button_press(&self, evt: &gdk::EventButton) -> glib::Propagation {
        if evt.button() == gdk::ffi::GDK_BUTTON_SECONDARY as u32 {
            let Some((x, y)) = evt.coords() else {
                return glib::Propagation::Proceed;
            };
            let eq_coord = self.map_widget.widget_crd_to_eq(x, y);
            *self.clicked_crd.borrow_mut() = eq_coord;
            let indi_is_active = self.indi.state() == indi::ConnState::Connected;
            let selected_item = self.selected_item.borrow();
            let enable_goto = indi_is_active && selected_item.is_some() && !self.goto_started.get();
            gtk_utils::enable_action(&self.window, "sm_goto_selected", enable_goto);
            gtk_utils::enable_action(&self.window, "sm_goto_sel_solve", enable_goto);
            gtk_utils::enable_action(
                &self.window,
                "sm_goto_point",
                indi_is_active && eq_coord.is_some() && !self.goto_started.get(),
            );
            let m_sm_goto_sel = self.builder.object::<gtk::Menu>("m_sm_widget").unwrap();
            m_sm_goto_sel.set_attach_widget(Some(self.map_widget.get_widget()));
            m_sm_goto_sel.popup_at_pointer(None);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    fn coord_of_selected_object_at_spec_time(&self) -> Option<EqCoord> {
        let selected_item = self.selected_item.borrow();
        let Some(selected_item) = &*selected_item else { return None; };
        let j2000 = j2000_time();
        let time = self.map_widget.time();
        let epoch_cvt = EpochCvt::new(&j2000, &time);
        let crd = selected_item.crd();
        Some(epoch_cvt.convert_eq(&crd))
    }

    fn handler_goto_selected(self: &Rc<Self>) {
        let Some(crd) = self.coord_of_selected_object_at_spec_time() else {
            return;
        };
        self.goto_coordinate(&crd, true);
    }

    fn handler_goto_sel_and_solve(self: &Rc<Self>) {
        let Some(crd) = self.coord_of_selected_object_at_spec_time() else {
            return;
        };
        self.goto_coordinate(&crd, false);
    }

    fn handler_goto_point(self: &Rc<Self>) {
        let clicked_crd = self.clicked_crd.borrow();
        let Some(clicked_crd) = &*clicked_crd else { return; };
        self.goto_coordinate(clicked_crd, true);
    }

    fn goto_coordinate(self: &Rc<Self>, coord: &EqCoord, only_goto: bool) {
        let mut options = self.options.write().unwrap();
        options.read_all(&self.builder);
        drop(options);
        let config = if only_goto {
            GotoConfig::OnlyGoto
        } else {
            GotoConfig::GotoPlateSolveAndCorrect
        };
        gtk_utils::exec_and_show_error(&self.window, || {
            self.core.start_goto_coord(coord, config)?;
            Ok(())
        });
    }

    fn set_full_screen_mode(&self, full_screen: bool) {
        let bx_skymap_panel = self.builder.object::<gtk::Widget>("bx_skymap_panel").unwrap();
        if full_screen {
            self.read_ui_options_from_widgets();
            bx_skymap_panel.set_visible(false);
        } else {
            bx_skymap_panel.set_visible(true);
        }
        self.full_screen.set(full_screen);
    }

    fn create_plate_solve_preview(&self, data: &Preview8BitImgData) {
        if data.rgb_data.width == 0
        || data.rgb_data.height == 0 {
            *self.ps_img.borrow_mut() = None;
            return;
        }

        let bytes = glib::Bytes::from_owned(data.rgb_data.bytes.clone());
        let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(
            &bytes,
            gtk::gdk_pixbuf::Colorspace::Rgb,
            false,
            8,
            data.rgb_data.width as i32,
            data.rgb_data.height as i32,
            (data.rgb_data.width * 3) as i32,
        );

        let pixbuf = limit_pixbuf_by_longest_size(pixbuf, 2000);
        *self.ps_img.borrow_mut() = Some(pixbuf);
    }
}
