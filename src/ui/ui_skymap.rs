use std::{cell::{Cell, RefCell}, collections::HashMap, rc::{Rc, Weak}, sync::{Arc, RwLock}};
use chrono::{prelude::*, Days, Duration, Months};
use macros::FromBuilder;
use serde::{Serialize, Deserialize};
use gtk::{cairo, gdk, glib::{self, clone}, prelude::*};
use crate::{
    core::{core::*, events::*, frame_processing::*, mode_goto::GotoConfig},
    indi::{self, degree_to_str, hour_to_str},
    options::*,
    plate_solve::PlateSolveOkResult,
    utils::{gtk_utils::{self, *}, io_utils::*},
};
use super::{
    sky_map::{alt_widget::paint_altitude_by_time, data::*, math::*, painter::*},
    ui_main::*,
    ui_skymap_options::SkymapOptionsDialog,
    utils::*,
    module::*
};
use super::sky_map::{data::Observer, widget::SkymapWidget};

pub fn init_ui(
    window:  &gtk::ApplicationWindow,
    main_ui: &Rc<MainUi>,
    core:    &Arc<Core>,
    options: &Arc<RwLock<Options>>,
    indi:    &Arc<indi::Connection>,
) -> Rc<dyn UiModule> {
    let mut ui_options = UiOptions::default();
    gtk_utils::exec_and_show_error(window, || {
        load_json_from_config_file(&mut ui_options, MapUi::CONF_FN)?;
        Ok(())
    });

    let map_widget = SkymapWidget::new();

    let widgets = Widgets {
        top:      TopWidgets     ::from_builder_str(include_str!(r"resources/map_top.ui")),
        datetime: DatetimeWidgets::from_builder_str(include_str!(r"resources/map_datetime.ui")),
        obj:      ObjectWidgets  ::from_builder_str(include_str!(r"resources/map_obj.ui")),
        search:   SearchWidgets  ::from_builder_str(include_str!(r"resources/map_search.ui")),
    };

    let obj = Rc::new(MapUi {
        widgets,
        ui_options:    RefCell::new(ui_options),
        core:          Arc::clone(core),
        indi:          Arc::clone(indi),
        options:       Arc::clone(options),
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
        goto_started:  Cell::new(false),
        closed:        Cell::new(false),
        cam_rotation:  RefCell::new(HashMap::new()),
        ps_img:        RefCell::new(None),
        ps_result:     RefCell::new(None),
        weak_self_:    RefCell::new(Weak::default()),
        map_widget,
    });

    *obj.weak_self_.borrow_mut() = Rc::downgrade(&obj);

    obj.init_widgets();
    obj.init_search_result_treeview();
    obj.show_options();
    obj.update_widgets_enable_state();

    obj.connect_widgets_events();
    obj.connect_core_events();

    obj.set_observer_data_for_widget();

    obj
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
    pub paint:  PaintConfig,
    show_ccd:   bool,
    show_ps:    bool,
    exp_dt:     bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            paint:    PaintConfig::default(),
            show_ccd: true,
            exp_dt:   true,
            show_ps:  true,
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

#[derive(FromBuilder)]
struct TopWidgets {
    bx:                 gtk::Box,
    scl_max_dso_mag:    gtk::Scale,
    chb_show_stars:     gtk::CheckButton,
    chb_show_dso:       gtk::CheckButton,
    chb_show_galaxies:  gtk::CheckButton,
    chb_show_nebulas:   gtk::CheckButton,
    chb_show_sclusters: gtk::CheckButton,
    chb_show_eq_grid:   gtk::CheckButton,
    chb_show_ccd:       gtk::CheckButton,
    chb_show_ps:        gtk::CheckButton,
}

#[derive(FromBuilder)]
struct DatetimeWidgets {
    bx:       gtk::Box,
    spb_year: gtk::SpinButton,
    spb_mon:  gtk::SpinButton,
    spb_day:  gtk::SpinButton,
    spb_hour: gtk::SpinButton,
    spb_min:  gtk::SpinButton,
    spb_sec:  gtk::SpinButton,
    btn_play: gtk::ToggleButton,
}

#[derive(FromBuilder)]
struct ObjectWidgets {
    bx:          gtk::Box,
    e_names:     gtk::Entry,
    e_nicknames: gtk::Entry,
    l_type:      gtk::Label,
    l_mag_cap:   gtk::Label,
    l_mag:       gtk::Label,
    l_bv:        gtk::Label,
    l_ra:        gtk::Label,
    l_dec:       gtk::Label,
    l_ra_now:    gtk::Label,
    l_dec_now:   gtk::Label,
    l_zenith:    gtk::Label,
    l_az:        gtk::Label,
    da_graph:    gtk::DrawingArea,
    m_widget:    gtk::Menu,
}

#[derive(FromBuilder)]
struct SearchWidgets {
    bx: gtk::Box,
    se_search: gtk::SearchEntry,
    tv_result: gtk::TreeView,
}


struct Widgets {
    top:      TopWidgets,
    datetime: DatetimeWidgets,
    obj:      ObjectWidgets,
    search:   SearchWidgets,
}

struct MapUi {
    widgets:       Widgets,
    ui_options:    RefCell<UiOptions>,
    core:          Arc<Core>,
    indi:          Arc<indi::Connection>,
    options:       Arc<RwLock<Options>>,
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
    goto_started:  Cell<bool>,
    closed:        Cell<bool>,
    cam_rotation:  RefCell<HashMap<String, f64>>,
    ps_img:        RefCell<Option<gdk::gdk_pixbuf::Pixbuf>>,
    ps_result:     RefCell<Option<PlateSolveOkResult>>,
    weak_self_:    RefCell<Weak<MapUi>>
}

impl Drop for MapUi {
    fn drop(&mut self) {
        log::info!("MapUi dropped");
    }
}

impl UiModule for MapUi {
    fn show_options(&self, _options: &Options) {
    }

    fn get_options(&self, _options: &mut Options) {
    }

    fn panels(&self) -> Vec<Panel> {
        vec![
            Panel {
                str_id: "map_top",
                name:   String::new(),
                widget: self.widgets.top.bx.clone().upcast(),
                pos:    PanelPosition::Top,
                tab:    PanelTab::Map,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "map_datetime",
                name:   "Date & time".to_string(),
                widget: self.widgets.datetime.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Map,
                flags:  PanelFlags::empty(),
            },
            Panel {
                str_id: "map_obj",
                name:   "Selected object".to_string(),
                widget: self.widgets.obj.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Map,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "map_search",
                name:   "Search".to_string(),
                widget: self.widgets.search.bx.clone().upcast(),
                pos:    PanelPosition::Left,
                tab:    PanelTab::Map,
                flags:  PanelFlags::NO_EXPANDER,
            },
            Panel {
                str_id: "map_common",
                name:   String::new(),
                widget: self.map_widget.get_widget().clone().upcast(),
                pos:    PanelPosition::Center,
                tab:    PanelTab::Map,
                flags:  PanelFlags::NO_EXPANDER,
            },
        ]
    }

    fn process_event(&self, event: &UiModuleEvent) {
        match event {
            UiModuleEvent::AfterFirstShowOptions => {
                let widget = self.map_widget.get_widget();
                widget.set_expand(true);
            }
            UiModuleEvent::ProgramClosing => {
                self.handler_closing();
            }
            UiModuleEvent::TabChanged { to: TabPage::SkyMap, .. } => {
                self.check_data_loaded();
                self.set_observer_data_for_widget();
                self.update_date_time_widgets(true);
                self.update_skymap_widget(true);
                self.show_selected_objects_info();
            }
            UiModuleEvent::Timer => {
                self.handler_main_timer();
            }
            _ => {}
        }
    }
}

impl MapUi {
    const CONF_FN: &'static str = "ui_skymap";

    fn handler_closing(&self) {
        self.closed.set(true);

        self.read_ui_options_from_widgets();

        let ui_options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&ui_options, Self::CONF_FN);
        drop(ui_options);
    }

    fn init_widgets(&self) {
        let set_range = |spb: &gtk::SpinButton, min, max| {
            spb.set_range(min - 1.0, max + 1.0);
            spb.set_increments(1.0, 1.0);
        };

        set_range(&self.widgets.datetime.spb_year, 0.0, 3000.0);
        set_range(&self.widgets.datetime.spb_mon, 1.0, 12.0);
        set_range(&self.widgets.datetime.spb_day, 1.0, 31.0);
        set_range(&self.widgets.datetime.spb_hour, 0.0, 24.0);
        set_range(&self.widgets.datetime.spb_min, 0.0, 60.0);
        set_range(&self.widgets.datetime.spb_sec, 0.0, 60.0);

        self.widgets.top.scl_max_dso_mag.set_range(0.0, 20.0);
        self.widgets.top.scl_max_dso_mag.set_increments(0.5, 2.0);

        let (dpimm_x, dpimm_y) = gtk_utils::get_widget_dpmm(&self.window)
            .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
        self.widgets.top.scl_max_dso_mag.set_width_request((40.0 * dpimm_x) as i32);

        self.widgets.obj.da_graph.set_height_request((30.0 * dpimm_y) as i32);
    }

    fn connect_widgets_events(self: &Rc<Self>) {
        gtk_utils::connect_action   (&self.window, self, "map_play",          Self::handler_btn_play_pressed);
        gtk_utils::connect_action   (&self.window, self, "map_now",           Self::handler_btn_now_pressed);
        gtk_utils::connect_action_rc(&self.window, self, "skymap_options",    Self::handler_action_options);
        gtk_utils::connect_action_rc(&self.window, self, "sm_goto_selected",  Self::handler_goto_selected);
        gtk_utils::connect_action_rc(&self.window, self, "sm_goto_sel_solve", Self::handler_goto_sel_and_solve);
        gtk_utils::connect_action_rc(&self.window, self, "sm_goto_point",     Self::handler_goto_point);

        let connect_spin_btn_evt = |spin_btn: &gtk::SpinButton| {
            spin_btn.connect_value_changed(clone!(@weak self as self_ => move |_| {
                self_.handler_time_changed();
            }));
        };

        connect_spin_btn_evt(&self.widgets.datetime.spb_year);
        connect_spin_btn_evt(&self.widgets.datetime.spb_mon);
        connect_spin_btn_evt(&self.widgets.datetime.spb_day);
        connect_spin_btn_evt(&self.widgets.datetime.spb_hour);
        connect_spin_btn_evt(&self.widgets.datetime.spb_min);
        connect_spin_btn_evt(&self.widgets.datetime.spb_sec);

        self.widgets.top.scl_max_dso_mag.connect_value_changed(
            clone!(@weak self as self_ => move |scale| {
                self_.handler_max_magnitude_changed(scale.value());
            })
        );

        let connect_obj_visibility_changed = |ch: &gtk::CheckButton| {
            ch.connect_active_notify(clone!(@weak self as self_ => move |_| {
                self_.handler_obj_visibility_changed();
            }));
        };

        connect_obj_visibility_changed(&self.widgets.top.chb_show_stars);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_dso);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_galaxies);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_nebulas);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_sclusters);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_eq_grid);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_ccd);
        connect_obj_visibility_changed(&self.widgets.top.chb_show_ps);

        self.map_widget.add_obj_sel_handler(
            clone!(@weak self as self_ => move |object| {
                self_.handler_object_selected(object);
            })
        );

        self.widgets.search.se_search.connect_search_changed(
            clone!(@weak self as self_ => move |se| {
                self_.handler_search_text_changed(se);
            })
        );

        self.widgets.search.tv_result.selection().connect_changed(
            clone!( @weak self as self_ => move |selection| {
                self_.handler_search_result_selection_changed(selection);
            })
        );

        self.widgets.obj.da_graph.connect_draw(
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
                if self_.main_ui.current_tab_page() == TabPage::SkyMap {
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
        let opts = self.ui_options.borrow();

        self.widgets.top.chb_show_stars.set_active(opts.paint.filter.contains(ItemsToShow::STARS));
        self.widgets.top.chb_show_dso.set_active(opts.paint.filter.contains(ItemsToShow::DSO));
        self.widgets.top.chb_show_galaxies.set_active(opts.paint.filter.contains(ItemsToShow::GALAXIES));
        self.widgets.top.chb_show_nebulas.set_active(opts.paint.filter.contains(ItemsToShow::NEBULAS));
        self.widgets.top.chb_show_sclusters.set_active(opts.paint.filter.contains(ItemsToShow::CLUSTERS));
        self.widgets.top.chb_show_eq_grid.set_active(opts.paint.eq_grid.visible);
        self.widgets.top.chb_show_ccd.set_active(opts.show_ccd);
        self.widgets.top.chb_show_ps.set_active(opts.show_ps);
        self.widgets.top.scl_max_dso_mag.set_value(opts.paint.max_dso_mag as f64);

        drop(opts);
    }

    fn read_ui_options_from_widgets(&self) {
        let mut opts = self.ui_options.borrow_mut();
        opts.paint.max_dso_mag = self.widgets.top.scl_max_dso_mag.value() as f32;
        self.read_visibility_options_from_widgets(&mut opts);
        drop(opts);
    }

    fn read_visibility_options_from_widgets(&self, opts: &mut UiOptions) {
        opts.paint.filter.set(ItemsToShow::STARS, self.widgets.top.chb_show_stars.is_active());
        opts.paint.filter.set(ItemsToShow::DSO, self.widgets.top.chb_show_dso.is_active());
        opts.paint.filter.set(ItemsToShow::GALAXIES, self.widgets.top.chb_show_galaxies.is_active());
        opts.paint.filter.set(ItemsToShow::NEBULAS, self.widgets.top.chb_show_nebulas.is_active());
        opts.paint.filter.set(ItemsToShow::CLUSTERS, self.widgets.top.chb_show_sclusters.is_active());

        opts.paint.eq_grid.visible = self.widgets.top.chb_show_eq_grid.is_active();
        opts.show_ccd = self.widgets.top.chb_show_ccd.is_active();
        opts.show_ps = self.widgets.top.chb_show_ps.is_active();
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
        let user_time = self.user_time.borrow();
        let cur_dt = user_time.time(true);

        self.widgets.datetime.spb_year.set_value(cur_dt.year() as f64);
        self.widgets.datetime.spb_mon.set_value(cur_dt.month() as f64);
        self.widgets.datetime.spb_day.set_value(cur_dt.day() as f64);
        self.widgets.datetime.spb_hour.set_value(cur_dt.hour() as f64);
        self.widgets.datetime.spb_min.set_value(cur_dt.minute() as f64);
        self.widgets.datetime.spb_sec.set_value(cur_dt.second() as f64);

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

    fn check_data_loaded(&self) {
        gtk_utils::exec_and_show_error(&self.window, || {
            let result = self.check_data_loaded_impl();
            if let Err(_) = result {
                *self.skymap_data.borrow_mut() = Some(Rc::new(SkyMap::new()));
            }
            result
        });
    }

    fn check_data_loaded_impl(&self) -> anyhow::Result<()> {
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

        let weak_self = self.weak_self_.borrow();
        let Some(self_) = weak_self.upgrade() else {
            return Err(anyhow::anyhow!("self.weak_self_ is empty"));
        };
        glib::spawn_future_local(clone!(@weak self_ => async move {
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
            let prev_time = self.prev_wdt.borrow();
            //self.widgets.datetime.
            let year_diff = self.widgets.datetime.spb_year.value() as i32 - prev_time.year;
            let mon_diff = self.widgets.datetime.spb_mon.value() as i32 - prev_time.mon;
            let day_diff = self.widgets.datetime.spb_day.value() as i32 - prev_time.day;
            let hour_diff = self.widgets.datetime.spb_hour.value() as i32 - prev_time.hour;
            let min_diff = self.widgets.datetime.spb_min.value() as i32 - prev_time.min;
            let sec_diff = self.widgets.datetime.spb_sec.value() as i32 - prev_time.sec;
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
            let mut user_time = self.user_time.borrow_mut();
            user_time.pause(!self.widgets.datetime.btn_play.is_active());
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
        self.read_visibility_options_from_widgets(&mut opts);
        drop(opts);

        self.update_widgets_enable_state();
        self.update_skymap_widget(true);
    }

    fn update_widgets_enable_state(&self) {
        let dso_enabled = self.widgets.top.chb_show_dso.is_active();
        self.widgets.top.chb_show_galaxies.set_sensitive(dso_enabled);
        self.widgets.top.chb_show_nebulas.set_sensitive(dso_enabled);
        self.widgets.top.chb_show_sclusters.set_sensitive(dso_enabled);
    }

    fn handler_object_selected(&self, obj: Option<SkymapObject>) {
        *self.selected_item.borrow_mut() = obj;
        self.show_selected_objects_info();
        self.update_selected_item_graph();
    }

    fn show_selected_objects_info(&self) {
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

        self.widgets.obj.e_names.set_text(&names);
        self.widgets.obj.e_nicknames.set_text(&nicknames);
        self.widgets.obj.l_type.set_label(&obj_type_str);
        self.widgets.obj.l_mag_cap.set_label(&mag_cap_str);
        self.widgets.obj.l_mag.set_label(&mag_str);
        self.widgets.obj.l_bv.set_label(&bv_str);
        self.widgets.obj.l_ra.set_label(&ra_str);
        self.widgets.obj.l_dec.set_label(&dec_str);
        self.widgets.obj.l_ra_now.set_label(&ra_now_str);
        self.widgets.obj.l_dec_now.set_label(&dec_now_str);
        self.widgets.obj.l_zenith.set_label(&zenith_str);
        self.widgets.obj.l_az.set_label(&azimuth_str);
    }

    fn init_search_result_treeview(&self) {
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
            self.widgets.search.tv_result.append_column(&col);
        }
        self.widgets.search.tv_result.set_model(Some(&model));
    }

    fn handler_search_text_changed(&self, _se: &gtk::SearchEntry) {
        self.search();
    }

    pub fn search(&self) {
        let Some(skymap) = &*self.skymap_data.borrow() else { return; };
        let text = self.widgets.search.se_search.text().trim().to_string();
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
        let Some(model) = self.widgets.search.tv_result.model() else { return; };
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
            let selection = self.widgets.search.tv_result.selection();
            let mut path = gtk::TreePath::new();
            path.append_index(0);
            selection.select_path(&path);
        }
    }

    fn handler_key_press_event(&self, event: &gdk::EventKey) -> glib::Propagation {
        if event.state().contains(gdk::ModifierType::CONTROL_MASK)
        && matches!(event.keyval(), gdk::keys::constants::F|gdk::keys::constants::f) {
            self.widgets.search.se_search.grab_focus();
            return glib::Propagation::Stop;
        }

        if matches!(event.keyval(), gdk::keys::constants::Up|gdk::keys::constants::Down)
        && self.widgets.search.se_search.has_focus() {
            let Some(model) = self.widgets.search.tv_result.model() else {
                return glib::Propagation::Proceed;
            };

            let result_count = gtk_utils::get_model_row_count(&model);
            if result_count == 0 {
                return glib::Propagation::Proceed;
            }
            let selection = self.widgets.search.tv_result.selection();
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
                self.widgets.search.tv_result.set_cursor(path, Option::<&gtk::TreeViewColumn>::None, false);
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
        self.widgets.obj.da_graph.queue_draw();
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
            self.widgets.obj.m_widget.set_attach_widget(Some(self.map_widget.get_widget()));
            self.widgets.obj.m_widget.popup_at_pointer(None);
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
        self.main_ui.get_all_options();

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
