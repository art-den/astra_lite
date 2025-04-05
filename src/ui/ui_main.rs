use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    process::Command,
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration
};

use gtk::{prelude::*, glib, glib::clone, cairo};
use macros::FromBuilder;
use serde::{Serialize, Deserialize};
use crate::{
    core::{core::*, events::*}, indi, options::*, utils::io_utils::*,
};
use super::{gtk_utils::*, module::*, utils::*};


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
    let widgets = Widgets::from_builder(&builder);

    widgets.window.set_application(Some(app));

    let icon = gtk::gdk_pixbuf::Pixbuf::from_read(include_bytes!(
        r"resources/astra_lite48x48.png"
    ).as_slice()).unwrap();
    widgets.window.set_icon(Some(&icon));

    exec_and_show_error(&widgets.window, || {
        let mut opts = options.write().unwrap();
        load_json_from_config_file::<Options>(&mut opts, MainUi::OPTIONS_FN)?;
        opts.check()?;
        Ok(())
    });

    let mut ui_options = UiOptions::default();
    exec_and_show_error(&widgets.window, || {
        load_json_from_config_file(&mut ui_options, MainUi::CONF_FN)
    });

    let main_ui = Rc::new(MainUi {
        widgets,
        logs_dir:       logs_dir.clone(),
        core:           Arc::clone(core),
        indi:           Arc::clone(indi),
        options:        Arc::clone(options),
        modules:        RefCell::new(UiModules::new()),
        ui_options:     RefCell::new(ui_options),
        progress:       RefCell::new(None),
        close_win_flag: Cell::new(false),
        prev_tab_page:  Cell::new(TabPage::Hardware),
        conn_string:    RefCell::new(String::new()),
        dev_string:     RefCell::new(String::new()),
        perf_string:    RefCell::new(String::new()),
        expanders:      RefCell::new(Vec::new()),
        self_:          RefCell::new(None), // used to drop MainData in window's delete_event
    });

    *main_ui.self_.borrow_mut() = Some(Rc::clone(&main_ui));

    main_ui.apply_options();
    main_ui.apply_theme();

    let hardware      = super::ui_hardware     ::init_ui(&main_ui.widgets.window, &main_ui, options, core, indi);
    let camera        = super::ui_camera       ::init_ui(&main_ui.widgets.window, &main_ui, options, core, indi);
    let darks_library = super::ui_darks_library::init_ui(&main_ui.widgets.window, options, core, indi);
    let preview       = super::ui_preview      ::init_ui(&main_ui.widgets.window, &main_ui, options, core);
    let focuser       = super::ui_focuser      ::init_ui(&main_ui.widgets.window, &main_ui, options, core, indi);
    let guiding       = super::ui_guiding      ::init_ui(&main_ui.widgets.window, &main_ui, options, core, indi);
    let mount         = super::ui_mount        ::init_ui(&main_ui.widgets.window, options, core, indi);
    let plate_solve   = super::ui_plate_solve  ::init_ui(&main_ui.widgets.window, &main_ui, options, core, indi);
    let polar_align   = super::ui_polar_align  ::init_ui(&main_ui.widgets.window, &main_ui, options, core, indi);
    let map           = super::ui_skymap       ::init_ui(&main_ui.widgets.window, &main_ui, core, options, indi);

    let mut modules = main_ui.modules.borrow_mut();
    modules.add(hardware);
    modules.add(camera);
    modules.add(preview);
    modules.add(darks_library);
    modules.add(focuser);
    modules.add(guiding);
    modules.add(mount);
    modules.add(plate_solve);
    modules.add(polar_align);
    modules.add(map);
    drop(modules);

    main_ui.build_modules_panels();

    main_ui.show_all_options();
    let modules = main_ui.modules.borrow();
    modules.process_event(&UiModuleEvent::AfterFirstShowOptions);
    drop(modules);

    main_ui.apply_panel_options();

    main_ui.connect_delete_event();
    main_ui.connect_close_after_finish_work();
    main_ui.connect_widgets_events();
    main_ui.correct_widgets_props();
    main_ui.connect_state_events();
    main_ui.update_window_title();

    disable_scroll_for_common_widgets(main_ui.widgets.window.upcast_ref());

    main_ui.widgets.window.show();
}

pub const TIMER_PERIOD_MS: u64 = 250;

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
.header_label {
    color: mix(@theme_fg_color, rgb(0, 64, 255), 0.4);
    background: rgba(0, 64, 255, .1);
}
.expander > title {
    color: mix(@theme_fg_color, rgb(0, 64, 255), 0.4);
    background: rgba(0, 64, 255, .1);
}
.bold {
    font-weight: bold;
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
struct UiOptions {
    win_width:     i32,
    win_height:    i32,
    win_maximized: bool,
    theme:         Theme,
    expanders:     HashMap<String, bool>,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            win_width:     -1,
            win_height:    -1,
            win_maximized: false,
            theme:         Theme::default(),
            expanders:     HashMap::new(),
        }
    }
}

#[derive(FromBuilder)]
struct Widgets {
    window: gtk::ApplicationWindow,
    nb_main: gtk::Notebook,
    lbl_cur_action: gtk::Label,
    bx_hw_left: gtk::Box,
    bx_hw_comm: gtk::Box,
    bx_map_top: gtk::Box,
    bx_map_left: gtk::Box,
    scr_map_left: gtk::ScrolledWindow,
    bx_map_center: gtk::Box,
    bx_comm_left: gtk::Box,
    bx_comm_left2: gtk::Box,
    bx_comm_bot_left: gtk::Box,
    bx_comm_center: gtk::Box,
    bx_comm_right: gtk::Box,
    scr_comm_right: gtk::ScrolledWindow,
    btn_fullscreen: gtk::ToggleButton,
    mi_dark_theme: gtk::RadioMenuItem,
    mi_light_theme: gtk::RadioMenuItem,
    da_progress: gtk::DrawingArea,
    mi_normal_log_mode: gtk::RadioMenuItem,
    mi_verbose_log_mode: gtk::RadioMenuItem,
    mi_max_log_mode: gtk::RadioMenuItem,
}

pub struct MainUi {
    widgets:        Widgets,
    logs_dir:       PathBuf,
    options:        Arc<RwLock<Options>>,
    modules:        RefCell<UiModules>,
    ui_options:     RefCell<UiOptions>,
    progress:       RefCell<Option<Progress>>,
    core:           Arc<Core>,
    indi:           Arc<indi::Connection>,
    close_win_flag: Cell<bool>,
    prev_tab_page:  Cell<TabPage>,
    conn_string:    RefCell<String>,
    dev_string:     RefCell<String>,
    perf_string:    RefCell<String>,
    expanders:      RefCell<Vec<(String, gtk::Expander, bool)>>,
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

    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.mi_dark_theme.connect_activate(
            clone!(@weak self as self_ => move |mi| {
                if mi.is_active() {
                    self_.ui_options.borrow_mut().theme = Theme::Dark;
                    self_.apply_theme();
                }
            })
        );

        self.widgets.mi_light_theme.connect_activate(
            clone!(@weak self as self_ => move |mi| {
                if mi.is_active() {
                    self_.ui_options.borrow_mut().theme = Theme::Light;
                    self_.apply_theme();
                }
            })
        );

        self.widgets.da_progress.connect_draw(
            clone!(@weak self as self_ => @default-panic, move |area, cr| {
                self_.handler_draw_progress(area, cr);
                glib::Propagation::Proceed
            })
        );

        self.widgets.mi_normal_log_mode.connect_activate(move |mi| {
            if mi.is_active() {
                log::info!("Setting verbose log::LevelFilter::Info level");
                log::set_max_level(log::LevelFilter::Info);
            }
        });

        self.widgets.mi_verbose_log_mode.connect_activate(move |mi| {
            if mi.is_active() {
                log::info!("Setting verbose log::LevelFilter::Debug level");
                log::set_max_level(log::LevelFilter::Debug);
            }
        });

        self.widgets.mi_max_log_mode.connect_activate(move |mi| {
            if mi.is_active() {
                log::info!("Setting verbose log::LevelFilter::Trace level");
                log::set_max_level(log::LevelFilter::Trace);
            }
        });

        self.widgets.btn_fullscreen.set_sensitive(false);
        self.widgets.btn_fullscreen.connect_active_notify(
            clone!(@weak self as self_ => move |btn| {
                self_.handler_btn_fullscreen(btn);
            })
        );

        self.widgets.nb_main.connect_switch_page(
            clone!(@weak self as self_  => move |_, _, page| {
                let enable_fullscreen = match page {
                    TAB_MAP|TAB_MAIN => true,
                    _                  => false
                };
                self_.widgets.btn_fullscreen.set_sensitive(enable_fullscreen);
                let tab = TabPage::from_tab_index(page);
                let modules = self_.modules.borrow();
                modules.process_event(&UiModuleEvent::TabChanged {
                    from: self_.prev_tab_page.get(),
                    to:   tab
                });
                self_.prev_tab_page.set(tab);
            })
        );

        connect_action(&self.widgets.window, self, "stop",             MainUi::handler_action_stop);
        connect_action(&self.widgets.window, self, "continue",         MainUi::handler_action_continue);
        connect_action(&self.widgets.window, self, "open_logs_folder", MainUi::handler_action_open_logs_folder);
    }

    fn connect_state_events(self: &Rc<Self>) {
        let (sender, receiver) = async_channel::unbounded();
        self.core.event_subscriptions().subscribe(move |event| {
            sender.send_blocking(event).unwrap();
        });

        glib::spawn_future_local(clone! (@weak self as self_ => async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    Event::Error(err) => {
                        show_error_message(
                            &self_.widgets.window,
                            "Core error",
                            &err
                        );
                    }
                    Event::ModeChanged => {
                        self_.correct_widgets_props();
                        self_.show_mode_caption();
                    },
                    Event::Progress(progress, _) => {
                        *self_.progress.borrow_mut() = progress;
                        self_.widgets.da_progress.queue_draw();
                        self_.show_mode_caption();
                    },
                    _ => {},
                }
            }
        }));
    }

    fn connect_delete_event(self: &Rc<Self>) {
        self.widgets.window.connect_delete_event(
            clone!(@weak self as self_ => @default-return glib::Propagation::Proceed,
            move |_, _| {
                let res = self_.handler_close_window();
                if res == glib::Propagation::Proceed {
                    gtk::main_iteration_do(true);
                    self_.get_all_options();
                    *self_.self_.borrow_mut() = None;
                }
                res
            })
        );
    }

    fn connect_close_after_finish_work(self: &Rc<Self>) {
        glib::timeout_add_local(
            Duration::from_millis(TIMER_PERIOD_MS),
            clone!(@weak self as self_ => @default-return glib::ControlFlow::Break,
            move || {
                if self_.close_win_flag.get() {
                    self_.widgets.window.close();
                    return glib::ControlFlow::Break;
                }
                let modules = self_.modules.borrow();
                modules.process_event(&UiModuleEvent::Timer);
                glib::ControlFlow::Continue
            }
        ));
    }

    fn handler_close_window(self: &Rc<Self>) -> glib::Propagation {
        if self.core.mode_data().mode.get_type() != ModeType::Waiting {
            let dialog = gtk::MessageDialog::builder()
                .transient_for(&self.widgets.window)
                .title("Operation is in progress")
                .text("Terminate current operation?")
                .modal(true)
                .message_type(gtk::MessageType::Question)
                .build();
            add_ok_and_cancel_buttons(
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

        self.read_ui_options_from_widgets();

        let options = self.ui_options.borrow();
        _ = save_json_to_config::<UiOptions>(&options, MainUi::CONF_FN);
        drop(options);

        if let Ok(mut options) = self.options.try_write() {
            let modules = self.modules.borrow();
            modules.get_options(&mut options);
        }

        let modules = self.modules.borrow();
        modules.process_event(&UiModuleEvent::ProgramClosing);
        drop(modules);

        self.core.event_subscriptions().clear();
        self.indi.unsubscribe_all();

        self.modules.borrow_mut().clear();

        *self.self_.borrow_mut() = None;

        glib::Propagation::Proceed
    }

    fn build_modules_panels(&self) {
        let modules = self.modules.borrow();
        let mut expanders = self.expanders.borrow_mut();

        expanders.clear();
        clear_container(&self.widgets.bx_comm_left);
        clear_container(&self.widgets.bx_comm_bot_left);
        clear_container(&self.widgets.bx_comm_center);
        clear_container(&self.widgets.bx_comm_right);
        clear_container(&self.widgets.bx_hw_left);
        clear_container(&self.widgets.bx_hw_comm);
        clear_container(&self.widgets.bx_map_top);
        clear_container(&self.widgets.bx_map_left);
        clear_container(&self.widgets.bx_map_center);

        for module in modules.items() {
            let panels = module.panels();
            for panel in panels {
                let container = match (&panel.tab, &panel.pos) {
                    (TabPage::Main, PanelPosition::Left) =>
                        self.widgets.bx_comm_left.upcast_ref::<gtk::Container>(),
                    (TabPage::Main, PanelPosition::BottomLeft) =>
                        self.widgets.bx_comm_bot_left.upcast_ref::<gtk::Container>(),
                    (TabPage::Main, PanelPosition::Center) =>
                        self.widgets.bx_comm_center.upcast_ref::<gtk::Container>(),
                    (TabPage::Main, PanelPosition::Right) =>
                        self.widgets.bx_comm_right.upcast_ref::<gtk::Container>(),
                    (TabPage::Hardware, PanelPosition::Left) =>
                        self.widgets.bx_hw_left.upcast_ref::<gtk::Container>(),
                    (TabPage::Hardware, PanelPosition::Center) =>
                        self.widgets.bx_hw_comm.upcast_ref::<gtk::Container>(),
                    (TabPage::SkyMap, PanelPosition::Top) =>
                        self.widgets.bx_map_top.upcast_ref::<gtk::Container>(),
                    (TabPage::SkyMap, PanelPosition::Left) =>
                        self.widgets.bx_map_left.upcast_ref::<gtk::Container>(),
                    (TabPage::SkyMap, PanelPosition::Center) =>
                        self.widgets.bx_map_center.upcast_ref::<gtk::Container>(),
                    _ => unreachable!(),
                };

                let is_visible =
                    cfg!(debug_assertions) ||
                    !panel.flags.contains(PanelFlags::DEVELOP);

                panel.widget.set_margin_top(5);
                panel.widget.set_margin_start(5);
                let panel_widget = panel.create_widget();
                if let Some(expander) = panel_widget.downcast_ref::<gtk::Expander>() {
                    let expanded_by_default = panel.flags.contains(PanelFlags::EXPANDED);
                    expanders.push((
                        panel.str_id.to_string(),
                        expander.clone(),
                        expanded_by_default
                    ));
                }
                if let Some(label) = panel.create_caption_label() {
                    label.set_visible(is_visible);
                    container.add(&label);
                }
                panel_widget.set_visible(is_visible);
                container.add(&panel_widget);
                if matches!(panel.pos, PanelPosition::Left|PanelPosition::Right)
                {
                    let separator = gtk::Separator::builder()
                        .visible(is_visible)
                        .orientation(gtk::Orientation::Horizontal)
                        .build();
                    container.add(&separator);
                }
            }
        }
    }

    fn apply_options(&self) {
        let options = self.ui_options.borrow();

        if options.win_width != -1 && options.win_height != -1 {
            self.widgets.window.resize(options.win_width, options.win_height);
        }

        if options.win_maximized {
            self.widgets.window.maximize();
        }

        match options.theme {
            Theme::Dark => self.widgets.mi_dark_theme.set_active(true),
            Theme::Light => self.widgets.mi_light_theme.set_active(true),
        }
    }

    fn apply_panel_options(&self) {
        let options = self.ui_options.borrow();
        let expanders = self.expanders.borrow();
        for (id, expander, expanded_by_default) in &*expanders {
            let is_expanded = options.expanders
                .get(id)
                .copied()
                .unwrap_or(*expanded_by_default);
            expander.set_expanded(is_expanded);
        }
    }

    fn apply_theme(&self) {
        let gtk_settings = gtk::Settings::default().unwrap();
        let options = self.ui_options.borrow();
        gtk_settings.set_property(
            "gtk-application-prefer-dark-theme",
            options.theme == Theme::Dark
        );
    }

    fn read_ui_options_from_widgets(&self) {
        let mut options = self.ui_options.borrow_mut();
        let (width, height) = self.widgets.window.size();
        options.win_width = width;
        options.win_height = height;
        options.win_maximized = self.widgets.window.is_maximized();

        let expanders = self.expanders.borrow();
        options.expanders.clear();
        for (id, expander, _) in &*expanders {
            options.expanders.insert(id.clone(), expander.is_expanded());
        }
    }

    fn handler_draw_progress(
        &self,
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
            exec_and_show_error(&self.widgets.window, || {
                draw_progress_bar(
                    area,
                    cr,
                    progress_ratio,
                    &progress_text
                )
            });
        }
    }

    fn correct_widgets_props(&self) {
        let mode_data = self.core.mode_data();
        let can_be_continued = mode_data.aborted_mode
            .as_ref()
            .map(|m| m.can_be_continued_after_stop())
            .unwrap_or(false);
        enable_actions(&self.widgets.window, &[
            ("stop",     mode_data.mode.can_be_stopped()),
            ("continue", can_be_continued),
        ]);
    }

    fn show_mode_caption(&self) {
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
        self.widgets.lbl_cur_action.set_text(&caption);
    }

    fn handler_action_stop(&self) {
        self.core.abort_active_mode();
    }

    fn handler_action_continue(&self) {
        exec_and_show_error(&self.widgets.window, || {
            self.core.continue_prev_mode()?;
            Ok(())
        });
    }

    fn handler_btn_fullscreen(&self, btn: &gtk::ToggleButton) {
        let full_screen = btn.is_active();
        self.widgets.bx_comm_left2.set_visible(!full_screen);
        self.widgets.scr_comm_right.set_visible(!full_screen);
        self.widgets.scr_map_left.set_visible(!full_screen);
        let modules = self.modules.borrow();
        modules.process_event(&UiModuleEvent::FullScreen(full_screen));
        drop(modules);
    }

    fn handler_action_open_logs_folder(&self) {
        exec_and_show_error(&self.widgets.window, || {
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

        self.widgets.window.set_title(&title)
    }

    pub fn current_tab_page(&self) -> TabPage {
        let page_index = self.widgets.nb_main.current_page().unwrap_or_default();
        TabPage::from_tab_index(page_index)
    }

    pub fn show_all_options_impl(&self, options: &Options) {
        let modules = self.modules.borrow();
        modules.show_options(&options);
    }

    pub fn show_all_options(&self) {
        let options = self.options.read().unwrap();
        self.show_all_options_impl(&options);
    }

    pub fn get_all_options_impl(&self, options: &mut Options) {
        let modules = self.modules.borrow();
        modules.get_options(options);
    }

    pub fn get_all_options(&self) {
        let mut options = self.options.write().unwrap();
        self.get_all_options_impl(&mut options);
    }
}
