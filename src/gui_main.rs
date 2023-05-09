use std::{sync::{Arc, RwLock}, rc::Rc, cell::RefCell, time::Duration, path::PathBuf};
use gtk::{prelude::*, glib, glib::clone, cairo};
use serde::{Serialize, Deserialize};

use crate::{indi_api, gtk_utils, io_utils::*, state::*, options::*};

pub const TIMER_PERIOD_MS: u64 = 250;
pub const CONF_FN: &str = "gui_main";
pub const OPTIONS_FN: &str = "options";

pub enum MainGuiEvent {
    Timer,
    FullScreen(bool),
    BeforeModeContinued
}

pub type MainGuiHandlers = Vec<Box<dyn Fn(MainGuiEvent) + 'static>>;

const CSS: &[u8] = b"
.greenbutton {
    background: rgba(0, 255, 0, .3);
}
.greenbutton:disabled {
    background: rgba(0, 255, 0, .05);
}
.redbutton {
    background: rgba(255, 0, 0, .3);
}
.redbutton:disabled {
    background: rgba(255, 0, 0, .05);
}
.yellowbutton {
    background: rgba(255, 255, 0, .3);
}
.yellowbutton:disabled {
    background: rgba(255, 255, 0, .05);
}
.expander > title {
    color: mix(@theme_fg_color, rgb(0, 64, 255), 0.4);
    background: rgba(0, 64, 255, .1);
}
";

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct MainOptions {
    win_width:     i32,
    win_height:    i32,
    win_maximized: bool,
    theme:         Theme,
}

impl Default for MainOptions {
    fn default() -> Self {
        Self {
            win_width:     -1,
            win_height:    -1,
            win_maximized: false,
            theme:         Theme::default(),
        }
    }
}

pub struct MainData {
    logs_dir:     PathBuf,
    pub options:  Arc<RwLock<Options>>,
    main_options: RefCell<MainOptions>,
    handlers:     RefCell<MainGuiHandlers>,
    progress:     RefCell<Option<Progress>>,
    conn_string:  RefCell<String>,
    dev_string:   RefCell<String>,
    pub state:    Arc<RwLock<State>>,
    pub indi:     Arc<indi_api::Connection>,
    pub builder:  gtk::Builder,
    pub window:   gtk::ApplicationWindow,
}

impl Drop for MainData {
    fn drop(&mut self) {
        log::info!("MainData dropped");
    }
}

pub fn build_ui(
    app:      &gtk::Application,
    logs_dir: &PathBuf
) {
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_data(CSS).unwrap();
    gtk::StyleContext::add_provider_for_screen(
        &gtk::gdk::Screen::default().expect("Could not connect to a display."),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let builder = gtk::Builder::from_string(include_str!(r"../ui/main.ui"));
    gtk_utils::disable_scroll_for_most_of_widgets(&builder);

    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let icon = gtk::gdk_pixbuf::Pixbuf::from_read(include_bytes!(
        r"../ui/astra_lite48x48.png"
    ).as_slice()).unwrap();
    window.set_icon(Some(&icon));

    let mut main_options = MainOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut main_options, CONF_FN)
    });
    let indi = Arc::new(indi_api::Connection::new());

    let mut options = Options::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut options, OPTIONS_FN)?;
        options.cam.raw_frames.check_and_correct()?;
        options.cam.live.check_and_correct()?;
        Ok(())
    });

    let options = Arc::new(RwLock::new(options));

    let data = Rc::new(MainData {
        logs_dir:     logs_dir.clone(),
        state:        Arc::new(RwLock::new(State::new(&indi, &options))),
        options,
        main_options: RefCell::new(main_options),
        handlers:     RefCell::new(Vec::new()),
        progress:     RefCell::new(None),
        indi,
        window:       window.clone(),
        builder:      builder.clone(),
        conn_string:  RefCell::new(String::new()),
        dev_string:   RefCell::new(String::new()),
    });

    State::connect_indi_events(&data.state);

    window.set_application(Some(app));
    window.show();
    apply_options(&data);
    apply_theme(&data);
    gtk::main_iteration_do(true);
    gtk::main_iteration_do(true);
    gtk::main_iteration_do(true);
    let data_weak = Rc::downgrade(&data);
    glib::timeout_add_local(
        Duration::from_millis(TIMER_PERIOD_MS),
        move || {
            let Some(data) = data_weak.upgrade() else {
                return Continue(false);
            };
            for handler in data.handlers.borrow().iter() {
                handler(MainGuiEvent::Timer);
            }
            Continue(true)
        }
    );

    crate::gui_hardware::build_ui(
        app,
        Rc::clone(&data),
        Arc::clone(&data.state),
        Arc::clone(&data.indi),
        data.builder.clone(),
        data.window.clone(),
    );
    crate::gui_camera::build_ui(
        app,
        &data,
        &mut data.handlers.borrow_mut()
    );

    gtk_utils::enable_widgets(&builder, false, &[
        ("mi_color_theme", cfg!(target_os = "windows"))
    ]);

    let mi_dark_theme = builder.object::<gtk::RadioMenuItem>("mi_dark_theme").unwrap();
    mi_dark_theme.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            data.main_options.borrow_mut().theme = Theme::Dark;
            apply_theme(&data);
        }
    }));

    let mi_light_theme = builder.object::<gtk::RadioMenuItem>("mi_light_theme").unwrap();
    mi_light_theme.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            data.main_options.borrow_mut().theme = Theme::Light;
            apply_theme(&data);
        }
    }));

    let data_weak = Rc::downgrade(&data);
    window.connect_delete_event(move |_, _| {
        let Some(data) = data_weak.upgrade() else {
            return gtk::Inhibit(false);
        };
        let res = handler_close_window(&data);
        gtk::main_iteration_do(true);
        res
    });

    let da_progress = builder.object::<gtk::DrawingArea>("da_progress").unwrap();
    da_progress.connect_draw(clone!(@weak data => @default-panic, move |area, cr| {
        handler_draw_progress(&data, area, cr);
        Inhibit(false)
    }));

    let mi_normal_log_mode = builder.object::<gtk::RadioMenuItem>("mi_normal_log_mode").unwrap();
    mi_normal_log_mode.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            log::info!("Setting verbose log::LevelFilter::Info level");
            log::set_max_level(log::LevelFilter::Info);
        }
    }));

    let mi_verbose_log_mode = builder.object::<gtk::RadioMenuItem>("mi_verbose_log_mode").unwrap();
    mi_verbose_log_mode.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            log::info!("Setting verbose log::LevelFilter::Debug level");
            log::set_max_level(log::LevelFilter::Debug);
        }
    }));

    let mi_max_log_mode = builder.object::<gtk::RadioMenuItem>("mi_max_log_mode").unwrap();
    mi_max_log_mode.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            log::info!("Setting verbose log::LevelFilter::Trace level");
            log::set_max_level(log::LevelFilter::Trace);
        }
    }));

    let btn_fullscreen = builder.object::<gtk::ToggleButton>("btn_fullscreen").unwrap();
    btn_fullscreen.set_sensitive(false);
    btn_fullscreen.connect_active_notify(clone!(@strong data => move |btn| {
        for fs_handler in data.handlers.borrow().iter() {
            fs_handler(MainGuiEvent::FullScreen(btn.is_active()));
        }
    }));

    let nb_main = builder.object::<gtk::Notebook>("nb_main").unwrap();
    nb_main.connect_switch_page(clone!(@strong data => move |_, _, page| {
        let enable_fullscreen = match page { 1|2 => true, _ => false };
        btn_fullscreen.set_sensitive(enable_fullscreen);
    }));

    gtk_utils::connect_action(&window, &data, "stop",             handler_action_stop);
    gtk_utils::connect_action(&window, &data, "continue",         handler_action_continue);
    gtk_utils::connect_action(&window, &data, "open_logs_folder", handler_action_open_logs_folder);
    correct_widgets_props(&data);
    connect_state_events(&data);
    update_window_title(&data);
}

fn connect_state_events(data: &Rc<MainData>) {
    let (sender, receiver) =
        glib::MainContext::channel(glib::PRIORITY_DEFAULT);
    let mut state = data.state.write().unwrap();
    state.subscribe_events(move |event| {
        sender.send(event).unwrap();
    });
    receiver.attach(None, clone! (@strong data => move |event| {
        match event {
            Event::ModeChanged => {
                correct_widgets_props(&data);
                show_mode_caption(&data);
            },
            Event::Propress(progress) => {
                *data.progress.borrow_mut() = progress;
                let da_progress = data.builder.object::<gtk::DrawingArea>("da_progress").unwrap();
                da_progress.queue_draw();
            },
            _ => {},
        }
        Continue(true)
    }));
}

fn handler_close_window(data: &Rc<MainData>) -> gtk::Inhibit {
    read_options_from_widgets(data);
    let options = data.main_options.borrow();
    _ = save_json_to_config::<MainOptions>(&options, CONF_FN);
    drop(options);

    let options = data.options.read().unwrap();
    _ = save_json_to_config::<Options>(&options, OPTIONS_FN);
    drop(options);

    gtk::Inhibit(false)
}

fn update_window_title(data: &MainData) {
    let title = "AstraLite (${arch} ver. ${ver})   --   Deepsky astrophotography and livestacking   --   [${devices_list}]   --   [${conn_status}]";
    let title = title.replace("${arch}",         std::env::consts::ARCH);
    let title = title.replace("${ver}",          env!("CARGO_PKG_VERSION"));
    let title = title.replace("${devices_list}", &data.dev_string.borrow());
    let title = title.replace("${conn_status}",  &data.conn_string.borrow());

    data.window.set_title(&title)
}

fn apply_options(data: &Rc<MainData>) {
    let options = data.main_options.borrow();

    if options.win_width != -1 && options.win_height != -1 {
        data.window.resize(options.win_width, options.win_height);
    }

    if options.win_maximized {
        data.window.maximize();
    }

    let mi_dark_theme = data.builder.object::<gtk::RadioMenuItem>("mi_dark_theme").unwrap();
    let mi_light_theme = data.builder.object::<gtk::RadioMenuItem>("mi_light_theme").unwrap();
    match options.theme {
        Theme::Dark => mi_dark_theme.set_active(true),
        Theme::Light => mi_light_theme.set_active(true),
    }
}

fn apply_theme(data: &Rc<MainData>) {
    if cfg!(target_os = "windows") {
        let gtk_settings = gtk::Settings::default().unwrap();
        let options = data.main_options.borrow();
        gtk_settings.set_property(
            "gtk-application-prefer-dark-theme",
            options.theme == Theme::Dark
        );
    }
}

fn read_options_from_widgets(data: &Rc<MainData>) {
    let mut options = data.main_options.borrow_mut();
    let (width, height) = data.window.size();
    options.win_width = width;
    options.win_height = height;
    options.win_maximized = data.window.is_maximized();
}

fn handler_draw_progress(
    data: &Rc<MainData>,
    area: &gtk::DrawingArea,
    cr:   &cairo::Context
) {
    let progress_data = data.progress.borrow();
    if let Some(progress_data) = progress_data.as_ref() {
        if progress_data.total == 0 {
            return;
        }
        let progress_ratio = progress_data.cur as f64 / progress_data.total as f64;
        let progress_text = format!("{} / {}", progress_data.cur, progress_data.total);
        gtk_utils::exec_and_show_error(&data.window, || {
            gtk_utils::draw_progress_bar(
                area,
                cr,
                progress_ratio,
                &progress_text
            )
        });
    }
}

fn correct_widgets_props(data: &Rc<MainData>) {
    let state = data.state.read().unwrap();
    let can_be_continued = state.aborted_mode().as_ref().map(|m| m.can_be_continued_after_stop()).unwrap_or(false);
    gtk_utils::enable_actions(&data.window, &[
        ("stop",     state.mode().can_be_stopped()),
        ("continue", can_be_continued),
    ]);
}

fn show_mode_caption(data: &Rc<MainData>) {
    let state = data.state.read().unwrap();
    let caption = if let Some(finished) = state.finished_mode() {
        finished.progress_string() + " (finished)"
    } else {
        let mut tmp = state.mode().progress_string();
        if let Some(aborted) = state.aborted_mode() {
            tmp += " + ";
            tmp += &aborted.progress_string();
            tmp += " (aborted)";
        }
        tmp
    };
    let lbl_cur_action = data.builder.object::<gtk::Label>("lbl_cur_action").unwrap();
    lbl_cur_action.set_text(&caption);
}

fn handler_action_stop(data: &Rc<MainData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let mut state = data.state.write().unwrap();
        state.abort_active_mode()?;
        Ok(())
    });
}

fn handler_action_continue(data: &Rc<MainData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        let mut state = data.state.write().unwrap();
        for fs_handler in data.handlers.borrow().iter() {
            fs_handler(MainGuiEvent::BeforeModeContinued);
        }
        state.continue_prev_mode()?;
        Ok(())
    });
}

fn handler_action_open_logs_folder(data: &Rc<MainData>) {
    let mut uri = r"file://".to_string();
    uri += data.logs_dir.as_os_str().to_str().unwrap_or_default();
    _ = gtk::show_uri_on_window(gtk::Window::NONE, &uri, 0);
}

impl MainData {
    pub fn set_dev_list_and_conn_status(&self, dev_list: String, conn_status: String) {
        *self.dev_string.borrow_mut() = dev_list;
        *self.conn_string.borrow_mut() = conn_status;
        update_window_title(self);
    }
}
