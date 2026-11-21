use std::{cell::Cell, rc::Rc, sync::Arc};
use gtk::{prelude::*, pango, glib::{self, clone}};

use crate::{
    core::{api::{Command, PolarAlignPatch}, engine::Engine},
    options::*,
    utils::log_utils::log_if_error,
};

use super::module::*;


pub fn init_ui(engine: &Arc<Engine>) -> Rc<dyn UiModule> {
    let obj = Rc::new(DebugUi {
        widgets: Widgets::new(),
        engine: Arc::clone(engine),
        opts_syncing: Cell::new(false),
    });
    obj.connect_widgets_events();
    obj
}

struct Widgets {
    grd:             gtk::Grid,
    spb_sim_alt_err: gtk::SpinButton,
    spb_sim_az_err:  gtk::SpinButton,
}

impl Widgets {
    fn new() -> Self {
        let bold = pango::AttrList::new();
        bold.insert(pango::AttrInt::new_weight(pango::Weight::Bold));

        let grd = gtk::Grid::builder()
            .visible(true)
            .row_spacing(5)
            .column_spacing(5)
            .build();

        let mut row = 0;

        let cam_header = gtk::Label::builder()
            .visible(true)
            .label("Camera")
            .halign(gtk::Align::Start)
            .attributes(&bold)
            .build();

        let chb_blob_frozen = gtk::CheckButton::builder()
            .visible(true)
            .label("Blob frozen")
            .build();

        grd.attach(&cam_header, 0, row, 2, 1);
        row += 1;

        grd.attach(&chb_blob_frozen, 0, row, 2, 1);
        row += 1;

        let pa_sep = gtk::Separator::builder()
            .visible(true)
            .orientation(gtk::Orientation::Horizontal)
            .build();

        grd.attach(&pa_sep, 0, row, 2, 1);
        row += 1;

        let pa_header = gtk::Label::builder()
            .visible(true)
            .label("Polar alignment")
            .halign(gtk::Align::Start)
            .attributes(&bold)
            .build();

        grd.attach(&pa_header, 0, row, 2, 1);

        row += 1;

        let l_alt_err = gtk::Label::builder()
            .visible(true)
            .label("Sim. alt. err (°)")
            .halign(gtk::Align::Start)
            .build();

        grd.attach(&l_alt_err, 0, row, 1, 1);

        let spb_sim_alt_err = gtk::SpinButton::builder()
            .visible(true)
            .digits(2)
            .hexpand(true)
            .hexpand_set(true)
            .build();

        spb_sim_alt_err.set_range(-45.0, 45.0);
        spb_sim_alt_err.set_increments(0.01, 0.1);

        grd.attach(&spb_sim_alt_err, 1, row, 1, 1);
        row += 1;

        let l_az_err = gtk::Label::builder()
            .visible(true)
            .label("Sim. az. err (°)")
            .halign(gtk::Align::Start)
            .build();

        grd.attach(&l_az_err, 0, row, 1, 1);

        let spb_sim_az_err = gtk::SpinButton::builder()
            .visible(true)
            .digits(2)
            .hexpand(true)
            .hexpand_set(true)
            .build();

        spb_sim_az_err.set_range(-45.0, 45.0);
        spb_sim_az_err.set_increments(0.01, 0.1);

        grd.attach(&spb_sim_az_err, 1, row, 1, 1);

        Widgets {
            grd,
            spb_sim_alt_err,
            spb_sim_az_err
        }
    }
}

struct DebugUi {
    widgets:      Widgets,
    engine:       Arc<Engine>,
    // Suppresses command handlers while widgets mirror the options
    opts_syncing: Cell<bool>,
}

impl Drop for DebugUi {
    fn drop(&mut self) {
        log::info!("DebugUi dropped");
    }
}

impl UiModule for DebugUi {
    fn show_options(&self, options: &Options) {
        let prev = self.opts_syncing.replace(true);
        self.widgets.spb_sim_alt_err.set_value(options.polar_align.sim_alt_err);
        self.widgets.spb_sim_az_err .set_value(options.polar_align.sim_az_err);
        self.opts_syncing.set(prev);
    }

    fn panels(&self) -> Vec<Panel> {
        vec![Panel {
            str_id: "debug",
            name:   "Debug".to_string(),
            widget: self.widgets.grd.clone().upcast(),
            pos:    PanelPosition::Left,
            tab:    TabPage::Main,
            flags:  PanelFlags::DEVELOP,
        }]
    }
}

impl DebugUi {
    fn connect_widgets_events(self: &Rc<Self>) {
        self.widgets.spb_sim_alt_err.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(Command::SetPolarAlignOptions(
                        PolarAlignPatch { sim_alt_err: Some(spb.value()), ..Default::default() }
                    )),
                    "Set sim alt err"
                );
            })
        );

        self.widgets.spb_sim_az_err.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                if self_.opts_syncing.get() { return; }
                log_if_error(
                    &self_.engine.exec_command(Command::SetPolarAlignOptions(
                        PolarAlignPatch { sim_az_err: Some(spb.value()), ..Default::default() }
                    )),
                    "Set sim az err"
                );
            })
        );
    }
}
