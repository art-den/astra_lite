use std::{sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, rc::Rc, cell::RefCell, time::Duration};
use gtk::{prelude::*, glib, glib::clone, cairo};
use serde::{Serialize, Deserialize};

use crate::{indi_api, gtk_utils, io_utils::*, image_processing::*};

pub const TIMER_PERIOD_MS: u64 = 250;
pub type TimerHandlers = Vec<Box<dyn Fn() + 'static>>;

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

#[derive(Default)]
struct ProgressData {
    progress: f64,
    text: String,
}

pub struct MainData {
    options:          RefCell<MainOptions>,
    timer_handlers:   RefCell<TimerHandlers>,
    progress:         RefCell<ProgressData>,
    pub indi:         Arc<indi_api::Connection>,
    pub builder:      gtk::Builder,
    pub window:       gtk::ApplicationWindow,
    pub indi_status:  RefCell<indi_api::ConnState>,
    pub cur_frame:    Arc<ResultImage>,
    pub thread_timer: Arc<ThreadTimer>,
    pub conn_string:  RefCell<String>,
    pub dev_string:   RefCell<String>,
}

impl Drop for MainData {
    fn drop(&mut self) {
        log::info!("MainData dropped");
    }
}

pub fn build_ui(application: &gtk::Application) {
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

    let mut options = MainOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut options, "conf_main")
    });
    let data = Rc::new(MainData {
        options:        RefCell::new(options),
        timer_handlers: RefCell::new(Vec::new()),
        progress:       RefCell::new(ProgressData::default()),
        indi:           Arc::new(indi_api::Connection::new()),
        window:         window.clone(),
        builder:        builder.clone(),
        indi_status:    RefCell::new(indi_api::ConnState::Disconnected),
        cur_frame:      Arc::new(ResultImage::new()),
        thread_timer:   Arc::new(ThreadTimer::new()),
        conn_string:    RefCell::new(String::new()),
        dev_string:     RefCell::new(String::new()),
    });
    window.set_application(Some(application));
    window.show();
    apply_options(&data);
    apply_theme(&data);
    gtk::main_iteration_do(true);
    gtk::main_iteration_do(true);
    let data_weak = Rc::downgrade(&data);
    glib::timeout_add_local(
        Duration::from_millis(TIMER_PERIOD_MS),
        move || {
            let Some(data) = data_weak.upgrade() else {
                return Continue(false);
            };
            for handler in data.timer_handlers.borrow().iter() {
                handler();
            }
            Continue(true)
        }
    );
    crate::gui_hardware::build_ui(
        application,
        &data
    );
    crate::gui_camera::build_ui(
        application,
        &data,
        &mut data.timer_handlers.borrow_mut()
    );

    gtk_utils::enable_widgets(&builder, &[
        ("mi_color_theme", cfg!(target_os = "windows"))
    ]);

    let mi_dark_theme = builder.object::<gtk::RadioMenuItem>("mi_dark_theme").unwrap();
    mi_dark_theme.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            data.options.borrow_mut().theme = Theme::Dark;
            apply_theme(&data);
        }
    }));

    let mi_light_theme = builder.object::<gtk::RadioMenuItem>("mi_light_theme").unwrap();
    mi_light_theme.connect_activate(clone!(@strong data => move |mi| {
        if mi.is_active() {
            data.options.borrow_mut().theme = Theme::Light;
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

    update_window_title(&data);
}


fn handler_close_window(data: &Rc<MainData>) -> gtk::Inhibit {
    read_options_from_widgets(data);
    let options = data.options.borrow();
    _ = save_json_to_config::<MainOptions>(&options, "conf_main");
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
    let options = data.options.borrow();

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
        let options = data.options.borrow();
        gtk_settings.set_property(
            "gtk-application-prefer-dark-theme",
            options.theme == Theme::Dark
        );
    }
}

fn read_options_from_widgets(data: &Rc<MainData>) {
    let mut options = data.options.borrow_mut();
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
    gtk_utils::exec_and_show_error(&data.window, || {
        gtk_utils::draw_progress_bar(
            area,
            cr,
            progress_data.progress,
            &progress_data.text
        )
    });
}

impl MainData {
    pub fn show_progress(&self, progress: f64, text: String) {
        let mut progress_data = self.progress.borrow_mut();
        progress_data.progress = progress;
        progress_data.text = text;
        drop(progress_data);
        let da_progress = self.builder.object::<gtk::DrawingArea>("da_progress").unwrap();
        da_progress.queue_draw();
    }

    pub fn set_cur_action_text(&self, text: &str) {
        let lbl_cur_action = self.builder.object::<gtk::Label>("lbl_cur_action").unwrap();
        lbl_cur_action.set_text(text);
    }

    pub fn set_dev_list_and_conn_status(&self, dev_list: String, conn_status: String) {
        *self.dev_string.borrow_mut() = dev_list;
        *self.conn_string.borrow_mut() = conn_status;
        update_window_title(self);
    }
}

///////////////////////////////////////////////////////////////////////////////

pub struct ThreadTimer {
    thread: Option<std::thread::JoinHandle<()>>,
    commands: Arc<Mutex<Vec<TimerCommand>>>,
    exit_flag: Arc<AtomicBool>,
}

struct TimerCommand {
    fun: Option<Box<dyn FnOnce() + Sync + Send + 'static>>,
    time: std::time::Instant,
    to_ms: u32,
}

impl Drop for ThreadTimer {
    fn drop(&mut self) {
        log::info!("Stopping ThreadTimer thread...");
        self.exit_flag.store(true, Ordering::Relaxed);
        let thread = self.thread.take().unwrap();
        _ = thread.join();
        log::info!("Done!");
    }
}

impl ThreadTimer {
    fn new() -> Self {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let exit_flag = Arc::new(AtomicBool::new(false));

        let thread = {
            let commands = Arc::clone(&commands);
            let exit_flag = Arc::clone(&exit_flag);
            std::thread::spawn(move || {
                Self::thread_fun(&commands, &exit_flag);
            })
        };
        Self {
            thread: Some(thread),
            commands,
            exit_flag,
        }
    }

    pub fn exec(&self, to_ms: u32, fun: impl FnOnce() + Sync + Send + 'static) {
        let mut commands = self.commands.lock().unwrap();
        let command = TimerCommand {
            fun: Some(Box::new(fun)),
            time: std::time::Instant::now(),
            to_ms,
        };
        commands.push(command);
    }

    fn thread_fun(
        commands:  &Mutex<Vec<TimerCommand>>,
        exit_flag: &AtomicBool
    ) {
        while !exit_flag.load(Ordering::Relaxed) {
            let mut commands = commands.lock().unwrap();
            for cmd in &mut *commands {
                if cmd.time.elapsed().as_millis() as u32 >= cmd.to_ms {
                    let fun = cmd.fun.take().unwrap();
                    fun();
                }
            }
            commands.retain(|cmd| cmd.fun.is_some());
            drop(commands);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

    }
}