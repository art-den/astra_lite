use std::{cell::RefCell, rc::Rc, sync::RwLock, sync::Arc};
use serde::{Serialize, Deserialize};
use gtk::{prelude::*, glib, glib::clone};
use crate::{options::*, utils::io_utils::*};
use super::{gtk_utils, gui_main::*};

const CONF_FN: &str = "gui_map";

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
    solar: bool,
}

impl Default for ObjectsToShow {
    fn default() -> Self {
        Self {
            stars: true,
            dso:   true,
            solar: true,
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

struct MapData {
    gui_options: RefCell<GuiOptions>,
    options:     Arc<RwLock<Options>>,
    builder:     gtk::Builder,
    window:      gtk::ApplicationWindow,
    self_:       RefCell<Option<Rc<MapData>>>
}

impl Drop for MapData {
    fn drop(&mut self) {
        log::info!("MapData dropped");
    }
}

pub fn build_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    options:  &Arc<RwLock<Options>>,
    handlers: &mut MainGuiHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut gui_options = GuiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut gui_options, CONF_FN)?;
        Ok(())
    });
    let data = Rc::new(MapData {
        gui_options: RefCell::new(gui_options),
        options:     Arc::clone(options),
        builder:     builder.clone(),
        window:      window.clone(),
        self_:       RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    show_options(&data);

    handlers.push(Box::new(clone!(@weak data => move |event| {
        handler_main_gui_event(&data, event);
    })));
}

fn handler_main_gui_event(data: &Rc<MapData>, event: MainGuiEvent) {
    match event {
        MainGuiEvent::ProgramClosing =>
            handler_closing(data),
        _ => {},
    }
}


fn handler_closing(data: &Rc<MapData>) {
    read_options_from_widgets(data);

    let gui_options = data.gui_options.borrow();
    _ = save_json_to_config::<GuiOptions>(&gui_options, CONF_FN);
    drop(gui_options);
}

fn show_options(data: &Rc<MapData>) {
    let pan_map1 = data.builder.object::<gtk::Paned>("pan_map1").unwrap();
    let opts = data.gui_options.borrow();
    pan_map1.set_position(opts.paned_pos1);
    let ui = gtk_utils::UiHelper::new_from_builder(&data.builder);
    ui.set_prop_bool("chb_flt_visible.active", opts.filter.visible);
    ui.set_prop_bool("chb_flt_stars.active", opts.filter.visible);
    ui.set_prop_bool("chb_flt_galaxies.active", opts.filter.galaxies);
    ui.set_prop_bool("chb_flt_clusters.active", opts.filter.clusters);
    ui.set_prop_bool("chb_flt_nebulae.active", opts.filter.nebulae);
    ui.set_prop_bool("chb_flt_pl_nebulae.active", opts.filter.pl_nebulae);
    ui.set_prop_bool("chb_flt_other.active", opts.filter.other);
    ui.set_prop_bool("chb_show_stars.active", opts.to_show.stars);
    ui.set_prop_bool("chb_show_dso.active", opts.to_show.dso);
    ui.set_prop_bool("chb_show_solar.active", opts.to_show.solar);

    drop(opts);
}

fn read_options_from_widgets(data: &Rc<MapData>) {
    let pan_map1 = data.builder.object::<gtk::Paned>("pan_map1").unwrap();
    let mut opts = data.gui_options.borrow_mut();
    let ui = gtk_utils::UiHelper::new_from_builder(&data.builder);
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
    opts.to_show.solar     = ui.prop_bool("chb_show_solar.active");

    drop(opts);
}
