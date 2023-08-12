use std::{cell::RefCell, sync::{RwLock, Arc}, rc::Rc};
use serde::{Serialize, Deserialize};
use gtk::{prelude::*, glib, glib::clone};
use crate::{
    options::*,
    io_utils::*,
    gtk_utils,
    state::*,
    indi_api,
    image_processing::*
};

const CONF_FN: &str = "gui_guiding";

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct GuiOptions {
    pub paned_pos1: i32,
    pub paned_pos2: i32,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            paned_pos1: -1,
            paned_pos2: -1,
        }
    }
}

struct GuidingData {
    gui_options:   RefCell<GuiOptions>,
    options:       Arc<RwLock<Options>>,
    state:         Arc<State>,
    indi:          Arc<indi_api::Connection>,
    builder:       gtk::Builder,
    window:        gtk::ApplicationWindow,
    indi_evt_conn: RefCell<Option<indi_api::Subscription>>,
    self_:         RefCell<Option<Rc<GuidingData>>>
}

impl Drop for GuidingData {
    fn drop(&mut self) {
        log::info!("GuidingData dropped");
    }
}

pub enum MainThreadEvent {
    ShowFrameProcessingResult(FrameProcessResult),
    StateEvent(StateEvent),
    IndiEvent(indi_api::Event),
}

pub fn build_ui(
    _app:    &gtk::Application,
    builder: &gtk::Builder,
    options: &Arc<RwLock<Options>>,
    state:   &Arc<State>,
    indi:    &Arc<indi_api::Connection>,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut gui_options = GuiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut gui_options, CONF_FN)?;
        Ok(())
    });
    let data = Rc::new(GuidingData {
        gui_options:   RefCell::new(gui_options),
        options:       Arc::clone(options),
        state:         Arc::clone(state),
        indi:          Arc::clone(indi),
        builder:       builder.clone(),
        window:        window.clone(),
        indi_evt_conn: RefCell::new(None),
        self_:         RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    show_options(&data);
    connect_indi_and_state_events(&data);

    window.connect_delete_event(
        clone!(@weak data => @default-return gtk::Inhibit(false),
        move |_, _| {
            let res = handler_close_window(&data);
            *data.self_.borrow_mut() = None;
            res
        })
    );
}

fn handler_close_window(data: &Rc<GuidingData>) -> gtk::Inhibit {
    data.state.disconnect_guid_cam_proc_result_event();

    read_options_from_widgets(data);

    let gui_options = data.gui_options.borrow();
    _ = save_json_to_config::<GuiOptions>(&gui_options, CONF_FN);
    drop(gui_options);

    gtk::Inhibit(false)
}

fn show_options(data: &Rc<GuidingData>) {
    let pan_guid1 = data.builder.object::<gtk::Paned>("pan_guid1").unwrap();
    let pan_guid2 = data.builder.object::<gtk::Paned>("pan_guid2").unwrap();
    let opts = data.gui_options.borrow();
    if opts.paned_pos1 != -1 {
        pan_guid1.set_position(opts.paned_pos1);
    }
    if opts.paned_pos2 != -1 {
        pan_guid2.set_position(pan_guid2.allocated_height() - opts.paned_pos2);
    }
    let _hlp = gtk_utils::GtkHelper::new_from_builder(&data.builder);
    drop(opts);
}

fn read_options_from_widgets(data: &Rc<GuidingData>) {
    let pan_guid1 = data.builder.object::<gtk::Paned>("pan_guid1").unwrap();
    let pan_guid2 = data.builder.object::<gtk::Paned>("pan_guid2").unwrap();
    let mut opts = data.gui_options.borrow_mut();
    let _hlp = gtk_utils::GtkHelper::new_from_builder(&data.builder);
    if pan_guid1.is_position_set() {
        opts.paned_pos1 = pan_guid1.position();
    }
    if pan_guid2.is_position_set() {
        opts.paned_pos2 = pan_guid2.allocated_height() - pan_guid2.position();
    }
    drop(opts);
}

fn connect_indi_and_state_events(data: &Rc<GuidingData>) {
    let (main_thread_sender, main_thread_receiver) =
        glib::MainContext::channel(glib::PRIORITY_DEFAULT);

    let sender = main_thread_sender.clone();
    *data.indi_evt_conn.borrow_mut() = Some(data.indi.subscribe_events(move |event| {
        sender.send(MainThreadEvent::IndiEvent(event)).unwrap();
    }));

    let sender = main_thread_sender.clone();
    data.state.subscribe_events(move |event| {
        sender.send(MainThreadEvent::StateEvent(event)).unwrap();
    });

    let sender = main_thread_sender.clone();
    data.state.connect_guid_cam_proc_result_event(move |res| {
        _ = sender.send(MainThreadEvent::ShowFrameProcessingResult(res));
    });

    main_thread_receiver.attach(None,
        clone!(@weak data => @default-return Continue(false),
        move |event| {
            process_event_in_main_thread(&data, event);
            Continue(true)
        }
    ));
}

fn process_event_in_main_thread(_data: &Rc<GuidingData>, event: MainThreadEvent) {
    match event {
        MainThreadEvent::IndiEvent(indi_api::Event::ConnChange(_)) => {}
        MainThreadEvent::IndiEvent(indi_api::Event::PropChange(event_data)) => {
            match &event_data.change {
                indi_api::PropChange::New(_) => {}
                indi_api::PropChange::Change{ .. } => {}
                indi_api::PropChange::Delete => {}
            };
        },
        MainThreadEvent::IndiEvent(indi_api::Event::DeviceDelete(_)) => {}
        MainThreadEvent::ShowFrameProcessingResult(_) => {}
        MainThreadEvent::StateEvent(StateEvent::ModeChanged) => {}
        MainThreadEvent::StateEvent(StateEvent::ModeContinued) => {}
        _ => {},
    }
}