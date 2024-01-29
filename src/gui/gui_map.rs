use std::{cell::RefCell, rc::Rc, sync::RwLock, sync::Arc};
use serde::{Serialize, Deserialize};
use gtk::{prelude::*, glib, glib::clone};
use crate::{options::*, utils::io_utils::*};
use super::{gtk_utils, gui_main::*};

pub fn init_ui(
    _app:     &gtk::Application,
    builder:  &gtk::Builder,
    options:  &Arc<RwLock<Options>>,
    handlers: &mut MainGuiHandlers,
) {
    let window = builder.object::<gtk::ApplicationWindow>("window").unwrap();

    let mut gui_options = GuiOptions::default();
    gtk_utils::exec_and_show_error(&window, || {
        load_json_from_config_file(&mut gui_options, MapGui::CONF_FN)?;
        Ok(())
    });
    let data = Rc::new(MapGui {
        gui_options: RefCell::new(gui_options),
        options:     Arc::clone(options),
        builder:     builder.clone(),
        window:      window.clone(),
        self_:       RefCell::new(None),
    });

    *data.self_.borrow_mut() = Some(Rc::clone(&data));

    data.show_options();

    handlers.push(Box::new(clone!(@weak data => move |event| {
        data.handler_main_gui_event(event);
    })));
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

struct MapGui {
    gui_options: RefCell<GuiOptions>,
    options:     Arc<RwLock<Options>>,
    builder:     gtk::Builder,
    window:      gtk::ApplicationWindow,
    self_:       RefCell<Option<Rc<MapGui>>>
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
            _ => {},
        }
    }

    fn handler_closing(self: &Rc<Self>) {
        self.read_options_from_widgets();

        let gui_options = self.gui_options.borrow();
        _ = save_json_to_config::<GuiOptions>(&gui_options, Self::CONF_FN);
        drop(gui_options);

        *self.self_.borrow_mut() = None;
    }

    fn show_options(self: &Rc<Self>) {
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
        ui.set_prop_bool("chb_show_solar.active", opts.to_show.solar);

        drop(opts);
    }

    fn read_options_from_widgets(self: &Rc<Self>) {
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
        opts.to_show.solar     = ui.prop_bool("chb_show_solar.active");

        drop(opts);
    }

}