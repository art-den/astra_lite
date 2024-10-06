use std::rc::Rc;
use gtk::{prelude::*, glib::clone};

use crate::options::*;

use super::gtk_utils;

pub struct PlatesolverOptionsDialog {
    builder: gtk::Builder,
    dialog:  gtk::Dialog,
}

impl PlatesolverOptionsDialog {
    pub fn new(transient_for: &gtk::Window) -> Rc<Self> {
        let builder = gtk::Builder::from_string(include_str!("resources/platesolver_options.ui"));
        let dialog = builder.object::<gtk::Dialog>("dialog").unwrap();
        dialog.set_transient_for(Some(transient_for));

        gtk_utils::add_ok_and_cancel_buttons(
            &dialog,
            "Ok",     gtk::ResponseType::Ok,
            "Cancel", gtk::ResponseType::Cancel,
        );
        gtk_utils::set_dialog_default_button(&dialog);

        let result = Rc::new(Self {
            builder,
            dialog
        });

        result.init_widgets();

        result
    }

    fn init_widgets(&self) {
        let spb_timeout = self.builder.object::<gtk::SpinButton>("spb_timeout").unwrap();
        spb_timeout.set_range(5.0, 120.0);
        spb_timeout.set_digits(0);
        spb_timeout.set_increments(5.0, 20.0);

        let spb_blind_timeout = self.builder.object::<gtk::SpinButton>("spb_blind_timeout").unwrap();
        spb_blind_timeout.set_range(5.0, 120.0);
        spb_blind_timeout.set_digits(0);
        spb_blind_timeout.set_increments(5.0, 20.0);
    }

    pub fn show_options(self: &Rc<Self>, options: &PlateSolveOptions) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui.set_prop_str("cbx_solver.active-id", options.solver.to_active_id());
        ui.set_prop_f64("spb_timeout.value", options.timeout as f64);
        ui.set_prop_f64("spb_blind_timeout.value", options.blind_timeout as f64);
    }

    pub fn get_options(self: &Rc<Self>, options: &mut PlateSolveOptions) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        options.solver = PlateSolverType::from_active_id(ui.prop_string("cbx_solver.active-id").as_deref());
        options.timeout = ui.prop_f64("spb_timeout.value") as _;
        options.blind_timeout = ui.prop_f64("spb_blind_timeout.value") as _;
    }

    pub fn exec(self: &Rc<Self>, on_apply: impl Fn() -> anyhow::Result<()> + 'static) {
        self.dialog.connect_response(clone!(@strong self as self_ => move |dlg, resp| {
            match resp {
                gtk::ResponseType::Ok => {
                    let ok = gtk_utils::exec_and_show_error(dlg, || {
                        on_apply()?;
                        Ok(())
                    });
                    if ok { dlg.close(); }
                }
                gtk::ResponseType::Cancel => {
                    dlg.close();
                }
                _ => {},
            }
        }));
        self.dialog.show();
    }

}
