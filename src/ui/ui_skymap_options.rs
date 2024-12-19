use std::rc::Rc;
use gtk::{prelude::*, glib::clone};
use crate::utils::gtk_utils;
use super::{sky_map::painter::Color, ui_skymap::UiOptions};

pub struct SkymapOptionsDialog {
    builder: gtk::Builder,
    dialog:  gtk::Dialog,
}

impl SkymapOptionsDialog {
    pub fn new(transient_for: &gtk::Window) -> Rc<Self> {
        let builder = gtk::Builder::from_string(include_str!("resources/skymap_options.ui"));
        let dialog = builder.object::<gtk::Dialog>("dialog").unwrap();
        dialog.set_transient_for(Some(transient_for));

        gtk_utils::add_ok_cancel_and_apply_buttons(
            &dialog,
            "Ok",     gtk::ResponseType::Ok,
            "Cancel", gtk::ResponseType::Cancel,
            "Apply",  gtk::ResponseType::Apply,
        );
        gtk_utils::set_dialog_default_button(&dialog);

        let spb_horiz_glow_angle = builder.object::<gtk::SpinButton>("spb_horiz_glow_angle").unwrap();
        spb_horiz_glow_angle.set_range(1.0, 45.0);
        spb_horiz_glow_angle.set_digits(0);
        spb_horiz_glow_angle.set_increments(1.0, 5.0);

        let result = Rc::new(SkymapOptionsDialog {
            builder: builder.clone(),
            dialog,
        });

        result
    }

    pub fn show_options(
        self:       &Rc<Self>,
        ui_options: &UiOptions,
    ) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        ui.set_prop_bool("chb_high_qual.active", ui_options.paint.high_quality);
        ui.set_prop_bool("chb_horiz_glow.active", ui_options.paint.horizon_glow.visible);
        ui.set_prop_f64("spb_horiz_glow_angle.value", ui_options.paint.horizon_glow.angle);

        let c = &ui_options.paint.horizon_glow.color;
        ui.set_color("clrb_horiz_glow", c.r, c.g, c.b, c.a);

        let c = &ui_options.paint.eq_grid.line_color;
        ui.set_color("clrb_eq_grid_line", c.r, c.g, c.b, c.a);

        let c = &ui_options.paint.eq_grid.text_color;
        ui.set_color("clrb_eq_grid_text", c.r, c.g, c.b, c.a);
    }

    pub fn get_options(
        self:       &Rc<Self>,
        ui_options: &mut UiOptions,
    ) -> anyhow::Result<()> {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
        ui_options.paint.high_quality = ui.prop_bool("chb_high_qual.active");
        ui_options.paint.horizon_glow.visible = ui.prop_bool("chb_horiz_glow.active");
        let (r, g, b, a) = ui.color("clrb_horiz_glow");
        ui_options.paint.horizon_glow.color = Color { r, g, b, a };

        ui_options.paint.horizon_glow.angle = ui.prop_f64("spb_horiz_glow_angle.value");

        let (r, g, b, a) = ui.color("clrb_eq_grid_line");
        ui_options.paint.eq_grid.line_color = Color { r, g, b, a };

        let (r, g, b, a) = ui.color("clrb_eq_grid_text");
        ui_options.paint.eq_grid.text_color = Color { r, g, b, a };

        return Ok(());
    }

    pub fn exec(self: &Rc<Self>, on_apply: impl Fn() -> anyhow::Result<()> + 'static) {
        self.dialog.connect_response(clone!(@strong self as self_ => move |dlg, resp| {
            match resp {
                gtk::ResponseType::Ok | gtk::ResponseType::Apply => {
                    let ok = gtk_utils::exec_and_show_error(dlg, || {
                        on_apply()?;
                        Ok(())
                    });
                    if ok && resp == gtk::ResponseType::Ok{
                        dlg.close();
                    }
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