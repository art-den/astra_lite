use std::{
    sync::{Arc, RwLock},
    rc::Rc,
    cell::{RefCell, Cell},
    time::Duration,
    path::PathBuf,
    process::Command
};
use gtk::{prelude::*, glib, glib::clone, cairo};
use serde::{Serialize, Deserialize};
use crate::{
    indi,
    utils::io_utils::*,
    core::core::*,
    options::*,
};
use super::{gtk_utils, ui_common::*};

pub fn init_ui(
    app:      &gtk::Application,
    indi:     &Arc<indi::Connection>,
    options:  &Arc<RwLock<Options>>,
    core:     &Arc<Core>,
    logs_dir: &PathBuf
) {
    let css_provider = gtk::CssProvider::new();
    css_provider.load_from_data(CSS).unwrap();
    gtk::StyleContext::add_provider_for_screen(
        &gtk::gdk::Screen::default().expect("Could not connect to a display."),
        &css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let builder = gtk::Builder::from_string(include_str!(r"resources/main.ui"));
    gtk_utils::disable_scroll_for_most_of_widgets(&builder);

    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let icon = gtk::gdk_pixbuf::Pixbuf::from_read(include_bytes!(
        r"resources/astra_lite48x48.png"
    ).as_slice()).unwrap();
    window.set_icon(Some(&icon));

    gtk_utils::exec_and_show_error(&window, || {
        let mut opts = options.write().unwrap();
        load_json_from_config_file::<Options>(&mut opts, MainUi::OPTIONS_FN)?;
        opts.raw_frames.check()?;
        opts.live.check()?;
        Ok(())
    });

    let mut main_options = MainUiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut main_options, MainUi::CONF_FN)
    });

    let data = Rc::new(MainUi {
        logs_dir:       logs_dir.clone(),
        core:           Arc::clone(core),
        options:        Arc::clone(options),
        main_options:   RefCell::new(main_options),
        handlers:       RefCell::new(Vec::new()),
        progress:       RefCell::new(None),
        window:         window.clone(),
        builder:        builder.clone(),
        close_win_flag: Cell::new(false),
        conn_string:    RefCell::new(String::new()),
        dev_string:     RefCell::new(String::new()),
        perf_string:    RefCell::new(String::new()),
        self_:          RefCell::new(None), // used to drop MainData in window's delete_event
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    window.set_application(Some(app));
    window.show();
    data.apply_options();
    data.apply_theme();
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
            data.exec_main_ui_handlers(MainUiEvent::Timer);
            glib::ControlFlow::Continue
        }
    ));

    let excl = Rc::new(ExclusiveCaller::new());
    let mut handlers = data.handlers.borrow_mut();
    super::ui_hardware::init_ui(app, &builder, &data, options, core, indi, &mut handlers);
    super::ui_camera::init_ui(app, &builder, &data, options, core, indi, &excl, &mut handlers);
    super::ui_focuser::init_ui(app, &builder, &data, options, core, indi, &excl, &mut handlers);
    super::ui_skymap::init_ui(app, &builder, &data, &options, indi, &excl, &mut handlers);

    // show common options
    excl.exec(|| {
        let opts = options.read().unwrap();
        opts.show_all(&builder);
        drop(opts);
    });

    let mi_dark_theme = builder.object::<gtk::RadioMenuItem>("mi_dark_theme").unwrap();
    mi_dark_theme.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            data.main_options.borrow_mut().theme = Theme::Dark;
            data.apply_theme();
        }
    }));

    let mi_light_theme = builder.object::<gtk::RadioMenuItem>("mi_light_theme").unwrap();
    mi_light_theme.connect_activate(clone!(@weak data => move |mi| {
        if mi.is_active() {
            data.main_options.borrow_mut().theme = Theme::Light;
            data.apply_theme();
        }
    }));

    let da_progress = builder.object::<gtk::DrawingArea>("da_progress").unwrap();
    da_progress.connect_draw(clone!(@weak data => @default-panic, move |area, cr| {
        data.handler_draw_progress(area, cr);
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
        data.exec_main_ui_handlers(MainUiEvent::FullScreen(btn.is_active()));
    }));

    let nb_main = builder.object::<gtk::Notebook>("nb_main").unwrap();
    nb_main.connect_switch_page(clone!(@weak data => move |_, _, page| {
        let enable_fullscreen = match page {
            TAB_MAP|TAB_CAMERA => true,
            _                  => false
        };
        btn_fullscreen.set_sensitive(enable_fullscreen);
        let tab = TabPage::from_tab_index(page);
        data.exec_main_ui_handlers(MainUiEvent::TabPageChanged(tab.clone()));
    }));

    window.connect_delete_event(
        clone!(@weak data => @default-return glib::Propagation::Proceed,
        move |_, _| {
            let res = data.handler_close_window();
            if res == glib::Propagation::Proceed {
                gtk::main_iteration_do(true);

                let mut opts = data.options.write().unwrap();
                opts.read_all(&builder);
                drop(opts);

                *data.self_.borrow_mut() = None;
            }
            res
        })
    );

    gtk_utils::connect_action(&window, &data, "stop",             MainUi::handler_action_stop);
    gtk_utils::connect_action(&window, &data, "continue",         MainUi::handler_action_continue);
    gtk_utils::connect_action(&window, &data, "open_logs_folder", MainUi::handler_action_open_logs_folder);

    data.correct_widgets_props();
    data.connect_state_events();
    data.update_window_title();
}

pub const TIMER_PERIOD_MS: u64 = 250;

#[derive(Clone, PartialEq)]
pub enum TabPage {
    Hardware,
    SkyMap,
    Camera,
}

impl TabPage {
    fn from_tab_index(index: u32) -> Self {
        match index {
            TAB_HARDWARE => TabPage::Hardware,
            TAB_MAP      => TabPage::SkyMap,
            TAB_CAMERA   => TabPage::Camera,
            _ => unreachable!(),
        }
    }
}

#[derive(Clone)]
pub enum MainUiEvent {
    Timer,
    FullScreen(bool),
    BeforeModeContinued,
    TabPageChanged(TabPage),
    ProgramClosing,
    BeforeDisconnect,
}

pub type MainUiHandlers = Vec<Box<dyn Fn(MainUiEvent) + 'static>>;

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
struct MainUiOptions {
    win_width:     i32,
    win_height:    i32,
    win_maximized: bool,
    theme:         Theme,
}

impl Default for MainUiOptions {
    fn default() -> Self {
        Self {
            win_width:     -1,
            win_height:    -1,
            win_maximized: false,
            theme:         Theme::default(),
        }
    }
}

pub struct MainUi {
    logs_dir:       PathBuf,
    options:        Arc<RwLock<Options>>,
    main_options:   RefCell<MainUiOptions>,
    handlers:       RefCell<MainUiHandlers>,
    progress:       RefCell<Option<Progress>>,
    core:           Arc<Core>,
    builder:        gtk::Builder,
    window:         gtk::ApplicationWindow,
    close_win_flag: Cell<bool>,
    conn_string:    RefCell<String>,
    dev_string:     RefCell<String>,
    perf_string:    RefCell<String>,
    self_:          RefCell<Option<Rc<MainUi>>>
}

impl Drop for MainUi {
    fn drop(&mut self) {
        log::info!("MainUi dropped");
    }
}

impl MainUi {
    const CONF_FN: &'static str = "ui_common";
    const OPTIONS_FN: &'static str = "options";

    fn connect_state_events(self: &Rc<Self>) {
        let (sender, receiver) = async_channel::unbounded();
        self.core.subscribe_events(move |event| {
            sender.send_blocking(event).unwrap();
        });

        glib::spawn_future_local(clone! (@weak self as self_ => async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    CoreEvent::ModeChanged => {
                        self_.correct_widgets_props();
                        self_.show_mode_caption();
                    },
                    CoreEvent::Propress(progress) => {
                        *self_.progress.borrow_mut() = progress;
                        let da_progress = self_.builder.object::<gtk::DrawingArea>("da_progress").unwrap();
                        da_progress.queue_draw();
                    },
                    _ => {},
                }
            }
        }));
    }

    fn handler_close_window(self: &Rc<Self>) -> glib::Propagation {
        if self.core.mode_data().mode.get_type() != ModeType::Waiting {
            let dialog = gtk::MessageDialog::builder()
                .transient_for(&self.window)
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

            dialog.connect_response(clone!(@weak self as self_ =>
                move |dlg, response| {
                if response == gtk::ResponseType::Yes {
                    self_.core.abort_active_mode();
                    self_.close_win_flag.set(true);
                }
                dlg.close();
            }));
            return glib::Propagation::Stop
        }

        self.read_options_from_widgets();

        let options = self.main_options.borrow();
        _ = save_json_to_config::<MainUiOptions>(&options, MainUi::CONF_FN);
        drop(options);

        self.exec_main_ui_handlers(MainUiEvent::ProgramClosing);

        self.handlers.borrow_mut().clear();

        glib::Propagation::Proceed
    }

    fn apply_options(self: &Rc<Self>) {
        let options = self.main_options.borrow();

        if options.win_width != -1 && options.win_height != -1 {
            self.window.resize(options.win_width, options.win_height);
        }

        if options.win_maximized {
            self.window.maximize();
        }

        let mi_dark_theme = self.builder.object::<gtk::RadioMenuItem>("mi_dark_theme").unwrap();
        let mi_light_theme = self.builder.object::<gtk::RadioMenuItem>("mi_light_theme").unwrap();
        match options.theme {
            Theme::Dark => mi_dark_theme.set_active(true),
            Theme::Light => mi_light_theme.set_active(true),
        }
    }

    fn apply_theme(self: &Rc<Self>) {
        let gtk_settings = gtk::Settings::default().unwrap();
        let options = self.main_options.borrow();
        gtk_settings.set_property(
            "gtk-application-prefer-dark-theme",
            options.theme == Theme::Dark
        );
    }

    fn read_options_from_widgets(self: &Rc<Self>) {
        let mut options = self.main_options.borrow_mut();
        let (width, height) = self.window.size();
        options.win_width = width;
        options.win_height = height;
        options.win_maximized = self.window.is_maximized();
    }

    fn handler_draw_progress(
        self: &Rc<Self>,
        area: &gtk::DrawingArea,
        cr:   &cairo::Context
    ) {
        let progress_data = self.progress.borrow();
        if let Some(progress_data) = progress_data.as_ref() {
            if progress_data.total == 0 {
                return;
            }
            let progress_ratio = progress_data.cur as f64 / progress_data.total as f64;
            let progress_text = format!("{} / {}", progress_data.cur, progress_data.total);
            gtk_utils::exec_and_show_error(&self.window, || {
                draw_progress_bar(
                    area,
                    cr,
                    progress_ratio,
                    &progress_text
                )
            });
        }
    }

    fn correct_widgets_props(self: &Rc<Self>) {
        let mode_data = self.core.mode_data();
        let can_be_continued = mode_data.aborted_mode
            .as_ref()
            .map(|m| m.can_be_continued_after_stop())
            .unwrap_or(false);
        gtk_utils::enable_actions(&self.window, &[
            ("stop",     mode_data.mode.can_be_stopped()),
            ("continue", can_be_continued),
        ]);
    }

    fn show_mode_caption(self: &Rc<Self>) {
        let mode_data = self.core.mode_data();
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
        let lbl_cur_action = self.builder.object::<gtk::Label>("lbl_cur_action").unwrap();
        lbl_cur_action.set_text(&caption);
    }

    fn handler_action_stop(self: &Rc<Self>) {
        self.core.abort_active_mode();
    }

    fn handler_action_continue(self: &Rc<Self>) {
        gtk_utils::exec_and_show_error(&self.window, || {
            self.exec_main_ui_handlers(MainUiEvent::BeforeModeContinued);
            self.core.continue_prev_mode()?;
            Ok(())
        });
    }

    fn exec_main_ui_handlers(self: &Rc<Self>, event: MainUiEvent) {
        for fs_handler in self.handlers.borrow().iter() {
            fs_handler(event.clone());
        }
    }

    fn handler_action_open_logs_folder(self: &Rc<Self>) {
        gtk_utils::exec_and_show_error(&self.window, || {
            if cfg!(target_os = "windows") {
                Command::new("explorer")
                    .args([self.logs_dir.to_str().unwrap_or_default()])
                    .spawn()?;
            } else {
                let uri = glib::filename_to_uri(&self.logs_dir, None)?;
                gtk::show_uri_on_window(gtk::Window::NONE, &uri, 0)?;
            }
            Ok(())
        });
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
        let mut title = "AstraLite (${arch} ver. ${ver})  --  Deepsky astrophotography and livestacking".to_string();
        title = title.replace("${arch}", std::env::consts::ARCH);
        title = title.replace("${ver}",  env!("CARGO_PKG_VERSION"));

        let mut append_if_not_empty = |string_to_append: &str| {
            if string_to_append.is_empty() {
                return;
            }
            title.push_str("  --  [");
            title.push_str(string_to_append);
            title.push_str("]");
        };

        append_if_not_empty(&self.dev_string.borrow());
        append_if_not_empty(&self.conn_string.borrow());
        append_if_not_empty(&self.perf_string.borrow());

        self.window.set_title(&title)
    }

    pub fn exec_before_disconnect_handlers(self: &Rc<Self>) {
        self.exec_main_ui_handlers(MainUiEvent::BeforeDisconnect);
    }

    pub fn current_tab_page(&self) -> TabPage {
        let nb_main = self.builder.object::<gtk::Notebook>("nb_main").unwrap();
        let page_index = nb_main.current_page().unwrap_or_default();
        TabPage::from_tab_index(page_index)
    }
}

