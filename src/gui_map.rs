use std::{cell::RefCell, rc::Rc, sync::RwLock, sync::Arc};
use serde::{Serialize, Deserialize};
use gtk::{prelude::*, glib, glib::clone};
use crate::{options::*, io_utils::*, gtk_utils};

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
    _app:    &gtk::Application,
    builder: &gtk::Builder,
    options: &Arc<RwLock<Options>>,
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

    window.connect_delete_event(clone!(@weak data => @default-return gtk::Inhibit(false), move |_, _| {
        let res = handler_close_window(&data);
        *data.self_.borrow_mut() = None;
        res
    }));
}

fn handler_close_window(data: &Rc<MapData>) -> gtk::Inhibit {
    read_options_from_widgets(data);

    let gui_options = data.gui_options.borrow();
    _ = save_json_to_config::<GuiOptions>(&gui_options, CONF_FN);
    drop(gui_options);

    gtk::Inhibit(false)
}

fn show_options(data: &Rc<MapData>) {
    let pan_map1 = data.builder.object::<gtk::Paned>("pan_map1").unwrap();
    let opts = data.gui_options.borrow();
    pan_map1.set_position(opts.paned_pos1);
    let hlp = gtk_utils::GtkHelper::new_from_builder(&data.builder);
    hlp.set_active_bool_prop("chb_flt_visible",    opts.filter.visible);
    hlp.set_active_bool_prop("chb_flt_stars",      opts.filter.visible);
    hlp.set_active_bool_prop("chb_flt_galaxies",   opts.filter.galaxies);
    hlp.set_active_bool_prop("chb_flt_clusters",   opts.filter.clusters);
    hlp.set_active_bool_prop("chb_flt_nebulae",    opts.filter.nebulae);
    hlp.set_active_bool_prop("chb_flt_pl_nebulae", opts.filter.pl_nebulae);
    hlp.set_active_bool_prop("chb_flt_other",      opts.filter.other);
    hlp.set_active_bool_prop("chb_show_stars",     opts.to_show.stars);
    hlp.set_active_bool_prop("chb_show_dso",       opts.to_show.dso);
    hlp.set_active_bool_prop("chb_show_solar",     opts.to_show.solar);

    drop(opts);
}

fn read_options_from_widgets(data: &Rc<MapData>) {
    let pan_map1 = data.builder.object::<gtk::Paned>("pan_map1").unwrap();
    let mut opts = data.gui_options.borrow_mut();
    let hlp = gtk_utils::GtkHelper::new_from_builder(&data.builder);
    opts.paned_pos1 = pan_map1.position();
    opts.filter.visible    = hlp.active_bool_prop("chb_flt_visible");
    opts.filter.visible    = hlp.active_bool_prop("chb_flt_stars");
    opts.filter.galaxies   = hlp.active_bool_prop("chb_flt_galaxies");
    opts.filter.clusters   = hlp.active_bool_prop("chb_flt_clusters");
    opts.filter.nebulae    = hlp.active_bool_prop("chb_flt_nebulae");
    opts.filter.pl_nebulae = hlp.active_bool_prop("chb_flt_pl_nebulae");
    opts.filter.other      = hlp.active_bool_prop("chb_flt_other");
    opts.to_show.stars     = hlp.active_bool_prop("chb_show_stars");
    opts.to_show.dso       = hlp.active_bool_prop("chb_show_dso");
    opts.to_show.solar     = hlp.active_bool_prop("chb_show_solar");

    drop(opts);
}
