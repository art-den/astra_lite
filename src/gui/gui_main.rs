use std::{sync::{Arc, RwLock}, rc::Rc, cell::{RefCell, Cell}, time::Duration, path::PathBuf, process::Command};
use gtk::{prelude::*, glib, glib::clone, cairo::{self}};
use serde::{Serialize, Deserialize};

use crate::{
    indi::indi_api,
    utils::io_utils::*,
    core::state::*,
    options::*,
};
use super::{gtk_utils, gui_common::*};

pub const TIMER_PERIOD_MS: u64 = 250;
const CONF_FN: &str = "gui_main";
const OPTIONS_FN: &str = "options";

#[derive(Clone)]
pub enum TabPage {
    Hardware,
    SkyMap,
    Camera,
}

pub enum MainGuiEvent {
    Timer,
    FullScreen(bool),
    BeforeModeContinued,
    TabPageChanged(TabPage),
    ProgramClosing,
}

pub type MainGuiHandlers = Vec<Box<dyn Fn(MainGuiEvent) + 'static>>;

const TAB_HARDWARE: u32 = 0;
const TAB_MAP:      u32 = 1;
const TAB_CAMERA:   u32 = 2;

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
struct MainOptions {
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

struct MainData {
    logs_dir:       PathBuf,
    options:        Arc<RwLock<Options>>,
    main_options:   RefCell<MainOptions>,
    handlers:       RefCell<MainGuiHandlers>,
    progress:       RefCell<Option<Progress>>,
    state:          Arc<State>,
    builder:        gtk::Builder,
    window:         gtk::ApplicationWindow,
    close_win_flag: Cell<bool>,
    self_:          RefCell<Option<Rc<MainData>>>
}

impl Drop for MainData {
    fn drop(&mut self) {
        log::info!("MainData dropped");
    }
}

pub fn build_ui(
    app:      &gtk::Application,
    indi:     &Arc<indi_api::Connection>,
    options:  &Arc<RwLock<Options>>,
    state:    &Arc<State>,
    logs_dir: &PathBuf
) {
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_data(CSS).unwrap();
    gtk::StyleContext::add_provider_for_screen(
        &gtk::gdk::Screen::default().expect("Could not connect to a display."),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let builder = gtk::Builder::from_string(include_str!(r"../../ui/main.ui"));
    gtk_utils::disable_scroll_for_most_of_widgets(&builder);

    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let icon = gtk::gdk_pixbuf::Pixbuf::from_read(include_bytes!(
        r"../../ui/astra_lite48x48.png"
    ).as_slice()).unwrap();
    window.set_icon(Some(&icon));

    gtk_utils::exec_and_show_error(&window, || {
        let mut options = options.write().unwrap();
        load_json_from_config_file::<Options>(&mut options, OPTIONS_FN)?;
        options.raw_frames.check()?;
        options.live.check()?;
        Ok(())
    });

    let mut main_options = MainOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut main_options, CONF_FN)
    });

    let gui = Rc::new(Gui::new(&builder));

    let data = Rc::new(MainData {
        logs_dir:       logs_dir.clone(),
        state:          Arc::clone(state),
        options:        Arc::clone(options),
        main_options:   RefCell::new(main_options),
        handlers:       RefCell::new(Vec::new()),
        progress:       RefCell::new(None),
        window:         window.clone(),
        builder:        builder.clone(),
        close_win_flag: Cell::new(false),
        self_:          RefCell::new(None), // used to drop MainData in window's delete_event
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));


    if !cfg!(debug_assertions) {
        // Hide map and guide pages for release build
        let nb_main = builder.object::<gtk::Notebook>("nb_main").unwrap();
        let map_tab = nb_main.nth_page(Some(TAB_MAP)).unwrap();
        map_tab.hide();
    }

    window.set_application(Some(app));
    window.show();
    apply_options(&data);
    apply_theme(&data);
    gtk::main_iteration_do(true);
    gtk::main_iteration_do(true);
    gtk::main_iteration_do(true);
    glib::timeout_add_local(
        Duration::from_millis(TIMER_PERIOD_MS),
        clone!(@weak data => @default-return glib::ControlFlow::Break,
        move || {
            if data.close_win_flag.get() {
                data.window.close();
                return glib::ControlFlow::Break;
            }
            for handler in data.handlers.borrow().iter() {
                handler(MainGuiEvent::Timer);
            }
            glib::ControlFlow::Continue
        }
    ));

    let mut handlers = data.handlers.borrow_mut();
    super::gui_hardware::build_ui(app, &builder, &gui, options, state, indi, &mut handlers);
    super::gui_camera::build_ui(app, &builder, &gui, options, state, indi, &mut handlers);
    super::gui_map::build_ui(app, &builder, &options, &mut handlers);

    let ui = gtk_utils::UiHelper::new_from_builder(&builder);

    ui.enable_widgets(false, &[("mi_color_theme", cfg!(target_os = "windows"))]);

    let mi_dark_theme = builder.object::<gtk::RadioMenuItem>("mi_dark_theme").unwrap();
    mi_dark_theme.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            data.main_options.borrow_mut().theme = Theme::Dark;
            apply_theme(&data);
        }
    }));

    let mi_light_theme = builder.object::<gtk::RadioMenuItem>("mi_light_theme").unwrap();
    mi_light_theme.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            data.main_options.borrow_mut().theme = Theme::Light;
            apply_theme(&data);
        }
    }));

    let da_progress = builder.object::<gtk::DrawingArea>("da_progress").unwrap();
    da_progress.connect_draw(clone!(@weak data => @default-panic, move |area, cr| {
        handler_draw_progress(&data, area, cr);
        glib::Propagation::Proceed
    }));

    let mi_normal_log_mode = builder.object::<gtk::RadioMenuItem>("mi_normal_log_mode").unwrap();
    mi_normal_log_mode.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            log::info!("Setting verbose log::LevelFilter::Info level");
            log::set_max_level(log::LevelFilter::Info);
        }
    }));

    let mi_verbose_log_mode = builder.object::<gtk::RadioMenuItem>("mi_verbose_log_mode").unwrap();
    mi_verbose_log_mode.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            log::info!("Setting verbose log::LevelFilter::Debug level");
            log::set_max_level(log::LevelFilter::Debug);
        }
    }));

    let mi_max_log_mode = builder.object::<gtk::RadioMenuItem>("mi_max_log_mode").unwrap();
    mi_max_log_mode.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            log::info!("Setting verbose log::LevelFilter::Trace level");
            log::set_max_level(log::LevelFilter::Trace);
        }
    }));

    let btn_fullscreen = builder.object::<gtk::ToggleButton>("btn_fullscreen").unwrap();
    btn_fullscreen.set_sensitive(false);
    btn_fullscreen.connect_active_notify(clone!(@weak data => move |btn| {
        for fs_handler in data.handlers.borrow().iter() {
            fs_handler(MainGuiEvent::FullScreen(btn.is_active()));
        }
    }));

    let nb_main = builder.object::<gtk::Notebook>("nb_main").unwrap();
    nb_main.connect_switch_page(clone!(@weak data => move |_, _, page| {
        let enable_fullscreen = match page {
            TAB_MAP|TAB_CAMERA => true,
            _                  => false
        };
        btn_fullscreen.set_sensitive(enable_fullscreen);
        let tab = match page {
            TAB_HARDWARE => TabPage::Hardware,
            TAB_MAP      => TabPage::SkyMap,
            TAB_CAMERA   => TabPage::Camera,
            _ => unreachable!(),
        };
        for handler in data.handlers.borrow().iter() {
            handler(MainGuiEvent::TabPageChanged(tab.clone()));
        }
    }));

    window.connect_delete_event(
        clone!(@weak data => @default-return glib::Propagation::Proceed,
        move |_, _| {
            let res = handler_close_window(&data);
            if res == glib::Propagation::Proceed {
                gtk::main_iteration_do(true);
                *data.self_.borrow_mut() = None;
            }
            res
        })
    );

    gtk_utils::connect_action(&window, &data, "stop",             handler_action_stop);
    gtk_utils::connect_action(&window, &data, "continue",         handler_action_continue);
    gtk_utils::connect_action(&window, &data, "open_logs_folder", handler_action_open_logs_folder);

    correct_widgets_props(&data);
    connect_state_events(&data);
    gui.update_window_title();
}

fn connect_state_events(data: &Rc<MainData>) {
    let (sender, receiver) =
        glib::MainContext::channel(glib::Priority::DEFAULT);
    data.state.subscribe_events(move |event| {
        sender.send(event).unwrap();
    });
    receiver.attach(None,
        clone! (@weak data => @default-return glib::ControlFlow::Break,
        move |event| {
            match event {
                StateEvent::ModeChanged => {
                    correct_widgets_props(&data);
                    show_mode_caption(&data);
                },
                StateEvent::Propress(progress) => {
                    *data.progress.borrow_mut() = progress;
                    let da_progress = data.builder.object::<gtk::DrawingArea>("da_progress").unwrap();
                    da_progress.queue_draw();
                },
                _ => {},
            }
            glib::ControlFlow::Continue
        })
    );
}

fn handler_close_window(data: &Rc<MainData>) -> glib::Propagation {
    if data.state.mode_data().mode.get_type() != ModeType::Waiting {
        println!("Showing dialog...");
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&data.window)
            .title("Operation is in progress")
            .text("Terminate current operation?")
            .modal(true)
            .message_type(gtk::MessageType::Question)
            .build();
        gtk_utils::add_ok_and_cancel_buttons(
            dialog.upcast_ref::<gtk::Dialog>(),
            "Yes", gtk::ResponseType::Yes,
            "No", gtk::ResponseType::No,
        );
        dialog.show();

        dialog.connect_response(clone!(@weak data =>
            move |dlg, response| {
            if response == gtk::ResponseType::Yes {
                data.state.abort_active_mode();
                data.close_win_flag.set(true);
            }
            dlg.close();
        }));
        return glib::Propagation::Stop
    }

    read_options_from_widgets(data);

    let options = data.main_options.borrow();
    _ = save_json_to_config::<MainOptions>(&options, CONF_FN);
    drop(options);

    for handler in data.handlers.borrow().iter() {
        handler(MainGuiEvent::ProgramClosing);
    }

    glib::Propagation::Proceed
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
            draw_progress_bar(
                area,
                cr,
                progress_ratio,
                &progress_text
            )
        });
    }
}

fn correct_widgets_props(data: &Rc<MainData>) {
    let mode_data = data.state.mode_data();
    let can_be_continued = mode_data.aborted_mode
        .as_ref()
        .map(|m| m.can_be_continued_after_stop())
        .unwrap_or(false);
    gtk_utils::enable_actions(&data.window, &[
        ("stop",     mode_data.mode.can_be_stopped()),
        ("continue", can_be_continued),
    ]);
}

fn show_mode_caption(data: &Rc<MainData>) {
    let mode_data = data.state.mode_data();
    let is_cur_mode_active = mode_data.mode.get_type() != ModeType::Waiting;
    let mut caption = String::new();
    if let (false, Some(finished)) = (is_cur_mode_active, &mode_data.finished_mode) {
        caption += &(finished.progress_string() + " (finished)");
    } else {
        caption += &mode_data.mode.progress_string();
        if let Some(aborted) = &mode_data.aborted_mode {
            caption += " + ";
            caption += &aborted.progress_string();
            caption += " (aborted)";
        }
    }
    let lbl_cur_action = data.builder.object::<gtk::Label>("lbl_cur_action").unwrap();
    lbl_cur_action.set_text(&caption);
}

fn handler_action_stop(data: &Rc<MainData>) {
    data.state.abort_active_mode();
}

fn handler_action_continue(data: &Rc<MainData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        for fs_handler in data.handlers.borrow().iter() {
            fs_handler(MainGuiEvent::BeforeModeContinued);
        }
        data.state.continue_prev_mode()?;
        Ok(())
    });
}

fn handler_action_open_logs_folder(data: &Rc<MainData>) {
    gtk_utils::exec_and_show_error(&data.window, || {
        if cfg!(target_os = "windows") {
            Command::new("explorer")
                .args([data.logs_dir.to_str().unwrap_or_default()])
                .spawn()?;
        } else {
            let uri = glib::filename_to_uri(&data.logs_dir, None)?;
            gtk::show_uri_on_window(gtk::Window::NONE, &uri, 0)?;
        }
        Ok(())
    });
}

pub struct Gui {
    builder:     gtk::Builder,
    conn_string: RefCell<String>,
    dev_string:  RefCell<String>,
    perf_string: RefCell<String>,
}

impl Gui {
    fn new(builder: &gtk::Builder) -> Self {
        Self {
            builder:     builder.clone(),
            conn_string: RefCell::new(String::new()),
            dev_string:  RefCell::new(String::new()),
            perf_string: RefCell::new(String::new()),
        }
    }

    pub fn set_dev_list_and_conn_status(&self, dev_list: String, conn_status: String) {
        *self.dev_string.borrow_mut() = dev_list;
        *self.conn_string.borrow_mut() = conn_status;
        self.update_window_title();
    }

    pub fn set_perf_string(&self, perf_string: String) {
        *self.perf_string.borrow_mut() = perf_string;
        self.update_window_title();
    }

    fn update_window_title(&self) {
        let title = "AstraLite (${arch} ver. ${ver})   --   Deepsky astrophotography and livestacking   --   [${devices_list}]   --   [${conn_status}]   --   [${perf}]";
        let title = title.replace("${arch}",         std::env::consts::ARCH);
        let title = title.replace("${ver}",          env!("CARGO_PKG_VERSION"));
        let title = title.replace("${devices_list}", &self.dev_string.borrow());
        let title = title.replace("${conn_status}",  &self.conn_string.borrow());
        let title = title.replace("${perf}",         &self.perf_string.borrow());
        let window = self.builder.object::<gtk::ApplicationWindow>("window").unwrap();
        window.set_title(&title)
    }
}
