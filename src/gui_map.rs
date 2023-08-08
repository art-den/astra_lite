use std::{cell::RefCell, rc::Rc, sync::RwLock, sync::Arc};
use serde::{Serialize, Deserialize};
use gtk::prelude::*;
use crate::{options::*, io_utils::*, gtk_utils};

pub const CONF_FN: &str = "gui_map";

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct GuiOptions {
    pub paned_pos1:     i32,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            paned_pos1: -1,
        }
    }
}

struct MapData {
    gui_options: RefCell<GuiOptions>,
    options:     Arc<RwLock<Options>>,
    builder:     gtk::Builder,
    window:      gtk::ApplicationWindow,
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
    gtk_utils::exec_and_show_error(&window, || {
        let _data = Rc::new(MapData {
            gui_options: RefCell::new(gui_options),
            options:     Arc::clone(options),
            builder:     builder.clone(),
            window:      window.clone(),
        });

        Ok(())
    });
}
