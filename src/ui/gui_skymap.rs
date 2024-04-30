use std::{cell::RefCell, cell::Cell, f64::consts::PI, rc::Rc, sync::{Arc, RwLock}};
use chrono::{prelude::*, Days, Duration, Months};
use serde::{Serialize, Deserialize};
use gtk::{prelude::*, glib, glib::clone};
use crate::{indi, options::*, utils::io_utils::*};
use super::{gtk_utils, gui_common::ExclusiveCaller, gui_main::*};
use super::sky_map::{data::Observer, skymap_widget::SkymapWidget};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    gui:      &Rc<Gui>,
    options:  &Arc<RwLock<Options>>,
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
        gui_options: RefCell::new(gui_options),
        options:     Arc::clone(options),
        builder:     builder.clone(),
        window:      window.clone(),
        gui:         Rc::clone(gui),
        excl:        ExclusiveCaller::new(),
        map_widget,
        user_time:        RefCell::new(UserTime::default()),
        prev_second: Cell::new(0),
        paint_ts:    RefCell::new(std::time::Instant::now()),
        prev_wdt:    RefCell::new(PrevWidgetsDT::default()),
        self_:       RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.init_widgets();
    data.show_options();

    data.connect_main_gui_events(handlers);
    data.connect_events();

    data.set_observer_data_for_widget();

    gtk_utils::connect_action(&window, &data, "skymap_options", MapGui::handler_action_options);
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct FilterOptions {
    visible:    bool,
    stars:      bool,
    galaxies:   bool,
    clusters:   bool, // star clusters
    nebulae:    bool,
    pl_nebulae: bool, // planet nebulae
    other:      bool,
}

impl Default for FilterOptions {
    fn default() -> Self {
        Self {
            visible:    false,
            stars:      false,
            galaxies:   true,
            clusters:   true,
            nebulae:    true,
            pl_nebulae: true,
            other:      true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct ObjectsToShow {
    stars: bool,
    dso:   bool,
}

impl Default for ObjectsToShow {
    fn default() -> Self {
        Self {
            stars: true,
            dso:   true,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct GuiOptions {
    pub paned_pos1: i32,
    pub filter: FilterOptions,
    pub to_show: ObjectsToShow,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            paned_pos1: -1,
            filter:     Default::default(),
            to_show:    Default::default(),
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
    gui_options: RefCell<GuiOptions>,
    options:     Arc<RwLock<Options>>,
    builder:     gtk::Builder,
    window:      gtk::ApplicationWindow,
    gui:         Rc<Gui>,
    excl:        ExclusiveCaller,
    map_widget:  Rc<SkymapWidget>,
    user_time:   RefCell<UserTime>,
    prev_second: Cell<u32>,
    paint_ts:    RefCell<std::time::Instant>, // last paint moment timestamp
    prev_wdt:    RefCell<PrevWidgetsDT>,
    self_:       RefCell<Option<Rc<MapGui>>>
}

impl Drop for MapGui {
    fn drop(&mut self) {
        log::info!("MapData dropped");
    }
}

impl MapGui {
    const CONF_FN: &'static str = "gui_map";

    fn handler_main_gui_event(&self, event: MainGuiEvent) {
        match event {
            MainGuiEvent::ProgramClosing =>
                self.handler_closing(),
            MainGuiEvent::Timer =>
                self.handler_main_timer(),
            MainGuiEvent::TabPageChanged(page) if page == TabPage::SkyMap => {
                self.update_date_time_widgets(true);
                self.update_skymap_widget(true);
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
    }

    fn connect_main_gui_events(self: &Rc<Self>, handlers: &mut MainGuiHandlers) {
        handlers.push(Box::new(clone!(@weak self as self_ => move |event| {
            self_.handler_main_gui_event(event);
        })));
    }

    fn connect_events(self: &Rc<Self>) {
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

        gtk_utils::connect_action(&self.window, self, "map_play", Self::handler_btn_play_pressed);
        gtk_utils::connect_action(&self.window, self, "map_now", Self::handler_btn_now_pressed);
    }

    fn show_options(&self) {
        let pan_map1 = self.builder.object::<gtk::Paned>("pan_map1").unwrap();
        let opts = self.gui_options.borrow();
        pan_map1.set_position(opts.paned_pos1);
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_bool("chb_flt_visible.active", opts.filter.visible);
        ui.set_prop_bool("chb_flt_stars.active", opts.filter.visible);
        ui.set_prop_bool("chb_flt_galaxies.active", opts.filter.galaxies);
        ui.set_prop_bool("chb_flt_clusters.active", opts.filter.clusters);
        ui.set_prop_bool("chb_flt_nebulae.active", opts.filter.nebulae);
        ui.set_prop_bool("chb_flt_pl_nebulae.active", opts.filter.pl_nebulae);
        ui.set_prop_bool("chb_flt_other.active", opts.filter.other);
        ui.set_prop_bool("chb_show_stars.active", opts.to_show.stars);
        ui.set_prop_bool("chb_show_dso.active", opts.to_show.dso);

        drop(opts);
    }

    fn read_options_from_widgets(&self) {
        let pan_map1 = self.builder.object::<gtk::Paned>("pan_map1").unwrap();
        let mut opts = self.gui_options.borrow_mut();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        opts.paned_pos1 = pan_map1.position();
        opts.filter.visible    = ui.prop_bool("chb_flt_visible.active");
        opts.filter.visible    = ui.prop_bool("chb_flt_stars.active");
        opts.filter.galaxies   = ui.prop_bool("chb_flt_galaxies.active");
        opts.filter.clusters   = ui.prop_bool("chb_flt_clusters.active");
        opts.filter.nebulae    = ui.prop_bool("chb_flt_nebulae.active");
        opts.filter.pl_nebulae = ui.prop_bool("chb_flt_pl_nebulae.active");
        opts.filter.other      = ui.prop_bool("chb_flt_other.active");
        opts.to_show.stars     = ui.prop_bool("chb_show_stars.active");
        opts.to_show.dso       = ui.prop_bool("chb_show_dso.active");

        drop(opts);
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
                self_.set_observer_data_for_widget();
            }
            dlg.close();
        }));
    }

    fn set_observer_data_for_widget(&self) {
        let sky_map_options = self.options.read().unwrap().sky_map.clone();
        let observer = Observer {
            latitude: PI * sky_map_options.latitude / 180.0,
            longitude: PI * sky_map_options.longitude / 180.0,
        };
        self.map_widget.set_observer(&observer);
    }

    fn handler_main_timer(&self) {
        if self.gui.current_tab_page() != TabPage::SkyMap {
            return;
        }

        // Change time in widget if second is changed
        self.update_date_time_widgets(false);

        // Update map 2 times per second
        self.update_skymap_widget(false);
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
            self.map_widget.set_time(user_time);
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
        });
    }


}