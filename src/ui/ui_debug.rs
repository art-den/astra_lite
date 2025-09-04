use std::{rc::Rc, sync::{Arc, RwLock}};
use gtk::{prelude::*, pango, glib::{self, clone}};

use crate::{options::*};

use super::module::*;


pub fn init_ui(options: &Arc<RwLock<Options>>) -> Rc<dyn UiModule> {
    let obj = Rc::new(DebugUi {
        widgets: Widgets::new(),
        options: Arc::clone(options),
    });
    obj.connect_widgets_events();
    obj
}

struct Widgets {
    grd:             gtk::Grid,
    chb_blob_frozen: gtk::CheckButton,
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
            chb_blob_frozen,
            spb_sim_alt_err,
            spb_sim_az_err
        }
    }
}

struct DebugUi {
    widgets: Widgets,
    options: Arc<RwLock<Options>>,
}

impl Drop for DebugUi {
    fn drop(&mut self) {
        log::info!("DebugUi dropped");
    }
}

impl UiModule for DebugUi {
    fn show_options(&self, options: &Options) {
        self.widgets.chb_blob_frozen.set_active(options.cam.debug.blob_frozen);
        self.widgets.spb_sim_alt_err.set_value(options.polar_align.sim_alt_err);
        self.widgets.spb_sim_az_err.set_value(options.polar_align.sim_az_err);
    }

    fn get_options(&self, options: &mut Options) {
        options.cam.debug.blob_frozen = self.widgets.chb_blob_frozen.is_active();
        options.polar_align.sim_alt_err  = self.widgets.spb_sim_alt_err.value();
        options.polar_align.sim_az_err   = self.widgets.spb_sim_az_err.value();
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
        self.widgets.chb_blob_frozen.connect_active_notify(
            clone!(@weak self as self_ => move |chb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.cam.debug.blob_frozen = chb.is_active();
            })
        );

        self.widgets.spb_sim_alt_err.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.polar_align.sim_alt_err = spb.value();
            })
        );

        self.widgets.spb_sim_az_err.connect_value_changed(
            clone!(@weak self as self_ => move |spb| {
                let Ok(mut options) = self_.options.try_write() else { return; };
                options.polar_align.sim_az_err = spb.value();
            })
        );
    }
}