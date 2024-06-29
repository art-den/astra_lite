use std::{cell::{Cell, RefCell}, rc::Rc, sync::{Arc, RwLock}};
use chrono::{prelude::*, Days, Duration, Months};
use serde::{Serialize, Deserialize};
use gtk::{prelude::*, glib, glib::clone, cairo, gdk};
use crate::{indi::{self, value_to_sexagesimal}, options::*, utils::io_utils::*};
use super::{gtk_utils::{self, DEFAULT_DPMM}, gui_common::*, gui_main::*, sky_map::{data::*, painter::*, utils::*}};
use super::sky_map::{data::Observer, widget::SkymapWidget};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    gui:      &Rc<Gui>,
    options:  &Arc<RwLock<Options>>,
    indi:     &Arc<indi::Connection>,
    handlers: &mut MainGuiHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut gui_options = GuiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut gui_options, MapGui::CONF_FN)?;
        Ok(())
    });

    let pan_map1 = builder.object::<gtk::Paned>("pan_map1").unwrap();
    let map_widget = SkymapWidget::new();
    pan_map1.add2(map_widget.get_widget());

    let data = Rc::new(MapGui {
        gui_options:    RefCell::new(gui_options),
        indi:           Arc::clone(indi),
        options:        Arc::clone(options),
        builder:        builder.clone(),
        window:         window.clone(),
        gui:            Rc::clone(gui),
        excl:           ExclusiveCaller::new(),
        map_widget,
        skymap_data:    RefCell::new(None),
        user_time:      RefCell::new(UserTime::default()),
        prev_second:    Cell::new(0),
        paint_ts:       RefCell::new(std::time::Instant::now()),
        prev_wdt:       RefCell::new(PrevWidgetsDT::default()),
        selected_item:  RefCell::new(None),
        search_result:  RefCell::new(Vec::new()),
        clicked_crd:    RefCell::new(None),
        self_:          RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_widgets();
    data.init_search_result_treeview();
    data.show_options();
    data.updatw_widgets_enable_state();

    data.connect_main_gui_events(handlers);
    data.connect_events();

    data.set_observer_data_for_widget();
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
struct GuiOptions {
    paned_pos1:         i32,
    paint:              PaintConfig,
    show_ccd:           bool,
    search_above_horiz: bool,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            paned_pos1:         -1,
            paint:              PaintConfig::default(),
            show_ccd:           true,
            search_above_horiz: true,
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

struct MapGui {
    gui_options:   RefCell<GuiOptions>,
    indi:          Arc<indi::Connection>,
    options:       Arc<RwLock<Options>>,
    builder:       gtk::Builder,
    window:        gtk::ApplicationWindow,
    gui:           Rc<Gui>,
    excl:          ExclusiveCaller,
    map_widget:    Rc<SkymapWidget>,
    skymap_data:   RefCell<Option<Rc<SkyMap>>>,
    user_time:     RefCell<UserTime>,
    prev_second:   Cell<u32>,
    paint_ts:      RefCell<std::time::Instant>, // last paint moment timestamp
    prev_wdt:      RefCell<PrevWidgetsDT>,
    selected_item: RefCell<Option<SkymapObject>>,
    search_result: RefCell<Vec<SkymapObject>>,
    clicked_crd:   RefCell<Option<EqCoord>>,
    self_:         RefCell<Option<Rc<MapGui>>>
}

impl Drop for MapGui {
    fn drop(&mut self) {
        log::info!("MapData dropped");
    }
}

impl MapGui {
    const CONF_FN: &'static str = "gui_map";

    fn handler_main_gui_event(self: &Rc<Self>, event: MainGuiEvent) {
        match event {
            MainGuiEvent::ProgramClosing =>
                self.handler_closing(),
            MainGuiEvent::Timer =>
                self.handler_main_timer(),
            MainGuiEvent::TabPageChanged(page) if page == TabPage::SkyMap => {
                self.update_date_time_widgets(true);
                self.update_skymap_widget(true);
                self.show_selected_objects_info();
            }
            _ => {},
        }
    }

    fn handler_closing(&self) {
        self.read_options_from_widgets();

        let gui_options = self.gui_options.borrow();
        _ = save_json_to_config::<GuiOptions>(&gui_options, Self::CONF_FN);
        drop(gui_options);

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
        da_sm_item_graph.set_height_request((20.0 * dpimm_y) as i32);
    }

    fn connect_main_gui_events(self: &Rc<Self>, handlers: &mut MainGuiHandlers) {
        handlers.push(Box::new(clone!(@weak self as self_ => move |event| {
            self_.handler_main_gui_event(event);
        })));
    }

    fn connect_events(self: &Rc<Self>) {
        gtk_utils::connect_action(&self.window, self, "map_play",         Self::handler_btn_play_pressed);
        gtk_utils::connect_action(&self.window, self, "map_now",          Self::handler_btn_now_pressed);
        gtk_utils::connect_action(&self.window, self, "skymap_options",   Self::handler_action_options);
        gtk_utils::connect_action(&self.window, self, "sm_goto_selected", Self::handler_goto_selected);
        gtk_utils::connect_action(&self.window, self, "sm_goto_point",    Self::handler_goto_point);

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
        connect_obj_visibility_changed("chb_sm_show_ccd");

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

        let chb_sm_above_horizon = self.builder.object::<gtk::CheckButton>("chb_sm_above_horizon").unwrap();
        chb_sm_above_horizon.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                self_.handler_above_horizon_changed(chb);
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
    }

    fn show_options(&self) {
        let pan_map1 = self.builder.object::<gtk::Paned>("pan_map1").unwrap();
        let opts = self.gui_options.borrow();
        pan_map1.set_position(opts.paned_pos1);
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        ui.set_prop_bool("chb_show_stars.active", opts.paint.filter.contains(ItemFilterFlags::STARS));
        ui.set_prop_bool("chb_show_dso.active", opts.paint.filter.contains(ItemFilterFlags::DSO));
        ui.set_prop_bool("chb_show_galaxies.active", opts.paint.filter.contains(ItemFilterFlags::GALAXIES));
        ui.set_prop_bool("chb_show_nebulas.active", opts.paint.filter.contains(ItemFilterFlags::NEBULAS));
        ui.set_prop_bool("chb_show_sclusters.active", opts.paint.filter.contains(ItemFilterFlags::CLUSTERS));
        ui.set_prop_bool("chb_sm_show_ccd.active", opts.show_ccd);

        ui.set_range_value("scl_max_dso_mag", opts.paint.max_dso_mag as f64);
        ui.set_prop_bool("chb_sm_above_horizon.active", opts.search_above_horiz);

        drop(opts);
    }

    fn read_options_from_widgets(&self) {
        let pan_map1 = self.builder.object::<gtk::Paned>("pan_map1").unwrap();
        let mut opts = self.gui_options.borrow_mut();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        opts.paned_pos1 = pan_map1.position();
        opts.paint.max_dso_mag = ui.range_value("scl_max_dso_mag") as f32;
        opts.search_above_horiz = ui.prop_bool("chb_sm_above_horizon.active");

        Self::read_visibility_options_from_widgets(&mut opts, &ui);

        drop(opts);
    }

    fn read_visibility_options_from_widgets(opts: &mut GuiOptions, ui: &gtk_utils::UiHelper) {

        opts.paint.filter.set(ItemFilterFlags::STARS, ui.prop_bool("chb_show_stars.active"));
        opts.paint.filter.set(ItemFilterFlags::DSO, ui.prop_bool("chb_show_dso.active"));
        opts.paint.filter.set(ItemFilterFlags::GALAXIES, ui.prop_bool("chb_show_galaxies.active"));
        opts.paint.filter.set(ItemFilterFlags::NEBULAS, ui.prop_bool("chb_show_nebulas.active"));
        opts.paint.filter.set(ItemFilterFlags::CLUSTERS, ui.prop_bool("chb_show_sclusters.active"));
        opts.show_ccd = ui.prop_bool("chb_sm_show_ccd.active");
    }

    fn handler_action_options(self: &Rc<Self>) {
        let builder = gtk::Builder::from_string(include_str!("resources/skymap_options.ui"));
        let dialog = builder.object::<gtk::Dialog>("dialog").unwrap();
        gtk_utils::add_ok_and_cancel_buttons(
            &dialog,
            "Ok",     gtk::ResponseType::Ok,
            "Cancel", gtk::ResponseType::Cancel,
        );
        gtk_utils::set_dialog_default_button(&dialog);

        let ui = gtk_utils::UiHelper::new_from_builder(&builder);

        let options = self.options.read().unwrap();
        ui.set_prop_str("e_lat.text", Some(&indi::value_to_sexagesimal(options.sky_map.latitude, true, 9)));
        ui.set_prop_str("e_long.text", Some(&indi::value_to_sexagesimal(options.sky_map.longitude, true, 9)));
        drop(options);
        drop(ui);

        let btn_get_from_gps = builder.object::<gtk::Button>("btn_get_from_gps").unwrap();
        btn_get_from_gps.connect_clicked(clone!(@strong self as self_, @strong builder, @strong dialog => move |_| {
            gtk_utils::exec_and_show_error(&dialog, || {
                let indi = &self_.indi;
                if indi.state() != indi::ConnState::Connected {
                    anyhow::bail!("INDI is not connected!");
                }
                let gps_devices: Vec<_> = indi.get_devices_list()
                    .into_iter()
                    .filter(|dev| dev.interface.contains(indi::DriverInterface::GPS))
                    .collect();
                if gps_devices.is_empty() {
                    anyhow::bail!("GPS device not found!");
                }
                let dev = &gps_devices[0];
                let (latitude, longitude, _) = indi.gps_get_lat_long_elev(&dev.name)?;
                let ui = gtk_utils::UiHelper::new_from_builder(&builder);
                ui.set_prop_str("e_lat.text", Some(&indi::value_to_sexagesimal(latitude, true, 9)));
                ui.set_prop_str("e_long.text", Some(&indi::value_to_sexagesimal(longitude, true, 9)));
                Ok(())
            });
        }));

        dialog.show();

        dialog.connect_response(clone!(@strong self as self_, @strong builder => move |dlg, resp| {
            if resp == gtk::ResponseType::Ok {
                let mut options = self_.options.write().unwrap();
                let ui = gtk_utils::UiHelper::new_from_builder(&builder);
                let mut err_str = String::new();

                let latitude_str = ui.prop_string("e_lat.text").unwrap_or_default();
                if let Some(latitude) = indi::sexagesimal_to_value(&latitude_str) {
                    options.sky_map.latitude = latitude;
                } else {
                    err_str += &format!("Wrong latitude: {}\n", latitude_str);
                }
                let longitude_str = ui.prop_string("e_long.text").unwrap_or_default();
                if let Some(longitude) = indi::sexagesimal_to_value(&longitude_str) {
                    options.sky_map.longitude = longitude;
                } else {
                    err_str += &format!("Wrong longitude: {}\n", longitude_str);
                }
                if !err_str.is_empty() {
                    gtk_utils::show_error_message(&self_.window, "Error", &err_str);
                    return;
                }
                drop(options);
                self_.set_observer_data_for_widget();
            }
            dlg.close();
        }));
    }

    fn create_observer(&self) -> Observer {
        let sky_map_options = self.options.read().unwrap().sky_map.clone();
        Observer {
            latitude: degree_to_radian(sky_map_options.latitude),
            longitude: degree_to_radian(sky_map_options.longitude),
        }
    }

    fn set_observer_data_for_widget(&self) {
        let observer = self.create_observer();
        self.map_widget.set_observer(&observer);
    }

    fn handler_main_timer(self: &Rc<Self>) {
        if self.gui.current_tab_page() != TabPage::SkyMap {
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

    fn update_skymap_widget(self: &Rc<Self>, force: bool) {
        self.check_data_loaded();

        let mut paint_ts = self.paint_ts.borrow_mut();
        if force || paint_ts.elapsed().as_secs_f64() > 0.5 {
            let user_time = self.user_time.borrow().time(false);
            *paint_ts = std::time::Instant::now();

            let config = self.gui_options.borrow();
            let show_ccd = config.show_ccd;
            let paint_config = config.paint.clone();
            drop(config);

            let indi_is_connected = self.indi.state() == indi::ConnState::Connected;

            let cam_frame = if show_ccd && indi_is_connected {
                let calc_cam_frame = || -> anyhow::Result<CameraFrame> {
                    let options = self.options.read().unwrap();
                    let cam_name = &options.cam.device.name;
                    let cam_ccd_prop = &options.cam.device.prop;
                    let cam_ccd = indi::CamCcd::from_ccd_prop_name(cam_ccd_prop);
                    if options.telescope.focal_len <= 0.1 {
                        anyhow::bail!("Wrong telescope focal lenght");
                    }
                    let (max_width, max_height) = self.indi.camera_get_max_frame_size(&cam_name, cam_ccd)?;
                    let bin = options.cam.frame.binning.get_ratio();
                    let (pixel_width_um, pixel_height_um) = self.indi.camera_get_pixel_size_um(&options.cam.device.name, cam_ccd)?;
                    let cropped_width = options.cam.frame.crop.translate(max_width / bin) as f64;
                    let cropped_height = options.cam.frame.crop.translate(max_height / bin) as f64;
                    let pixel_width_mm = pixel_width_um / 1000.0;
                    let pixel_height_mm = pixel_height_um / 1000.0;
                    let width_mm = cropped_width * pixel_width_mm;
                    let height_mm = cropped_height * pixel_height_mm;
                    let mut full_cam_name = cam_name.to_string();
                    if !cam_ccd_prop.is_empty() {
                        full_cam_name += ", ";
                        full_cam_name += cam_ccd_prop;
                    }
                    Ok(CameraFrame{
                        name: full_cam_name,
                        horiz_angle: f64::atan2(width_mm, options.telescope.focal_len),
                        vert_angle: f64::atan2(height_mm, options.telescope.focal_len),
                        rot_angle: 0.0,
                    })
                };
                calc_cam_frame().ok()
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

            self.map_widget.set_paint_config(
                &user_time,
                &paint_config,
                &telescope_pos,
                &cam_frame
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

    fn handler_time_changed(self: &Rc<Self>) {
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

    fn handler_btn_play_pressed(self: &Rc<Self>) {
        self.excl.exec(|| {
            let btn_play = self.builder.object::<gtk::ToggleButton>("btn_play").unwrap();
            let mut user_time = self.user_time.borrow_mut();
            user_time.pause(!btn_play.is_active());
            drop(user_time);
            self.set_time_to_widgets_impl();
        });
    }

    fn handler_btn_now_pressed(self: &Rc<Self>) {
        self.excl.exec(|| {
            let mut user_time = self.user_time.borrow_mut();
            user_time.set_now();
            drop(user_time);
            self.set_time_to_widgets_impl();
            self.update_skymap_widget(true);
            self.show_selected_objects_info();
        });
    }

    fn handler_max_magnitude_changed(self: &Rc<Self>, value: f64) {
        self.excl.exec(|| {
            let value = value as f32;
            let mut options = self.gui_options.borrow_mut();
            if options.paint.max_dso_mag == value {
                return;
            }
            options.paint.max_dso_mag = value;
            drop(options);

            self.update_skymap_widget(true);
        });
    }

    fn handler_obj_visibility_changed(self: &Rc<Self>) {
        let mut opts = self.gui_options.borrow_mut();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        Self::read_visibility_options_from_widgets(&mut opts, &ui);
        drop(opts);

        self.updatw_widgets_enable_state();
        self.update_skymap_widget(true);
    }

    fn updatw_widgets_enable_state(&self) {
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

        let names = obj.as_ref().map(|obj| obj.names().join(", ")).unwrap_or_default();
        let obj_type = obj.as_ref().map(|obj| obj.obj_type()).unwrap_or(SkyItemType::None);
        let obj_type_str = obj_type.to_str();
        let mag_str = obj.as_ref().map(|obj| {
            if let Some(mag) = obj.mag_v() {
                format!("{:.2}", mag)
            } else if let Some(mag) = obj.mag_b() {
                format!("{:.2}", mag)
            } else {
                String::new()
            }
        }).unwrap_or_default();

        let mag_cap_str = obj.as_ref().map(|obj| {
            if obj.mag_v().is_some() {
                "Magnitude (V)"
            } else if obj.mag_b().is_some() {
                "Magnitude (B)"
            } else {
                "Magnitude"
            }
        }).unwrap_or("Magnitude");

        let bv = obj.as_ref().map(|obj|obj.bv()).flatten();
        let bv_str = bv.map(|bv| format!("{:.2}", bv)).unwrap_or_default();

        let ra_str = match obj.as_ref().map(|obj| obj.crd()) {
            Some(crd) => value_to_sexagesimal(radian_to_hour(crd.ra), true, 9),
            None => String::new(),
        };

        let dec_str = match obj.as_ref().map(|obj| obj.crd()) {
            Some(crd) => value_to_sexagesimal(radian_to_degree(crd.dec), true, 8),
            None => String::new(),
        };

        let horiz_crd = obj.as_ref().map(|obj| {
            let observer = self.create_observer();
            let time = self.map_widget.time();
            let cvt = EqToHorizCvt::new(&observer, &time);
            cvt.eq_to_horiz(&obj.crd())
        });

        let zenith_str = horiz_crd.as_ref().map(|crd|
            value_to_sexagesimal(radian_to_degree(crd.alt), true, 8)
        ).unwrap_or_default();

        let azimuth_str = horiz_crd.as_ref().map(|crd|
            value_to_sexagesimal(radian_to_degree(crd.az), true, 8)
        ).unwrap_or_default();

        ui.set_prop_str("e_sm_sel_names.text", Some(&names));
        ui.set_prop_str("l_sm_sel_type.label", Some(&obj_type_str));
        ui.set_prop_str("l_sm_sel_mag_cap.label", Some(&mag_cap_str));
        ui.set_prop_str("l_sm_sel_mag.label", Some(&mag_str));
        ui.set_prop_str("l_sm_sel_bv.label", Some(&bv_str));
        ui.set_prop_str("l_sm_sel_ra.label", Some(&ra_str));
        ui.set_prop_str("l_sm_sel_dec.label", Some(&dec_str));
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
            TreeViewColumnExt::add_attribute(&col, &cell_text, "text", idx as i32);
            tv.append_column(&col);
        }
        tv.set_model(Some(&model));
    }

    fn handler_search_text_changed(&self, _se: &gtk::SearchEntry) {
        self.search();
    }

    fn handler_above_horizon_changed(&self, chb: &gtk::CheckButton) {
        self.gui_options.borrow_mut().search_above_horiz = chb.is_active();
        self.search();
    }

    pub fn search(&self) {
        let Some(skymap) = &*self.skymap_data.borrow() else { return; };
        let se_sm_search = self.builder.object::<gtk::SearchEntry>("se_sm_search").unwrap();
        let text = se_sm_search.text().trim().to_string();
        let mut found_items = skymap.search(&text);
        let options = self.gui_options.borrow();
        if options.search_above_horiz {
            let observer = self.create_observer();
            let time = self.map_widget.time();
            let cvt = EqToHorizCvt::new(&observer, &time);
            found_items.retain(|obj| cvt.eq_to_horiz(&obj.crd()).alt > 0.0);
        }
        *self.search_result.borrow_mut() = found_items;
        self.show_search_result();
    }

    fn show_search_result(&self) {
        let tv = self.builder.object::<gtk::TreeView>("tv_sm_search_result").unwrap();
        let Some(model) = tv.model() else { return; };
        let Ok(model) = model.downcast::<gtk::ListStore>() else { return; };
        let result = self.search_result.borrow();
        model.clear();
        for item in &*result {
            model.insert_with_values(
                None, &[
                (0, &item.names().join(", ")),
                (1, &item.obj_type().to_str()),
            ]);
        }
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
        *self.selected_item.borrow_mut() = Some(selected_obj.clone());
        self.show_selected_objects_info();
        self.update_selected_item_graph();
        self.map_widget.set_selected_object(Some(&selected_obj));
    }

    fn update_selected_item_graph(&self) {
        let da_sm_item_graph = self.builder.object::<gtk::DrawingArea>("da_sm_item_graph").unwrap();
        da_sm_item_graph.queue_draw();
    }

    fn handler_draw_item_graph(
        self:  &Rc<Self>,
        _area: &gtk::DrawingArea,
        _cr:   &cairo::Context
    ) -> anyhow::Result<()> {
        // TODO: draw graph with zenith by time
        //let Some(item) = &*self.selected_obj.borrow()
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
            gtk_utils::enable_action(&self.window, "sm_goto_selected", indi_is_active && selected_item.is_some());
            gtk_utils::enable_action(&self.window, "sm_goto_point", indi_is_active && eq_coord.is_some());
            let m_sm_goto_sel = self.builder.object::<gtk::Menu>("m_sm_widget").unwrap();
            m_sm_goto_sel.set_attach_widget(Some(self.map_widget.get_widget()));
            m_sm_goto_sel.popup_at_pointer(None);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    fn goto_eq_coord(&self, crd: &EqCoord) {
        let options = self.options.read().unwrap();
        let mount_device = &options.mount.device;
        gtk_utils::exec_and_show_error(&self.window, || {
            self.indi.mount_set_tracking(
                &mount_device,
                true,
                false,
                Some(1000)
            )?;

            self.indi.mount_set_eq_coord(
                &mount_device,
                radian_to_hour(crd.ra),
                radian_to_degree(crd.dec),
                true,
                None
            )?;
            Ok(())
        });
    }

    fn handler_goto_selected(self: &Rc<Self>) {
        let selected_item = self.selected_item.borrow();
        let Some(selected_item) = &*selected_item else { return; };
        self.goto_eq_coord(&selected_item.crd());
    }

    fn handler_goto_point(self: &Rc<Self>) {
        let clicked_crd = self.clicked_crd.borrow();
        let Some(clicked_crd) = &*clicked_crd else { return; };
        self.goto_eq_coord(clicked_crd);
    }
}