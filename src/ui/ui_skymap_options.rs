use std::rc::Rc;
use gtk::{prelude::*, glib::clone, gdk};
use macros::FromBuilder;
use super::{gtk_utils::*, sky_map::painter::Color, ui_skymap::UiOptions};

#[derive(FromBuilder)]
struct Widgets {
    dialog:               gtk::Dialog,
    chb_high_qual:        gtk::CheckButton,
    chb_horiz_glow:       gtk::CheckButton,
    clrb_horiz_glow:      gtk::ColorButton,
    spb_horiz_glow_angle: gtk::SpinButton,
    clrb_eq_grid_line:    gtk::ColorButton,
    clrb_eq_grid_text:    gtk::ColorButton,
}

pub struct SkymapOptionsDialog {
    widgets: Widgets,
}

impl SkymapOptionsDialog {
    pub fn new(transient_for: &gtk::Window) -> Rc<Self> {
        let widgets = Widgets::from_builder_str(include_str!("resources/skymap_options.ui"));

        widgets.dialog.set_transient_for(Some(transient_for));

        add_ok_cancel_and_apply_buttons(
            &widgets.dialog,
            "Ok",     gtk::ResponseType::Ok,
            "Cancel", gtk::ResponseType::Cancel,
            "Apply",  gtk::ResponseType::Apply,
        );
        set_dialog_default_button(&widgets.dialog);

        widgets.spb_horiz_glow_angle.set_range(1.0, 45.0);
        widgets.spb_horiz_glow_angle.set_digits(0);
        widgets.spb_horiz_glow_angle.set_increments(1.0, 5.0);

        let result = Rc::new(SkymapOptionsDialog { widgets });

        result
    }

    pub fn show_options(
        self:       &Rc<Self>,
        ui_options: &UiOptions,
    ) {
        self.widgets.chb_high_qual.set_active(ui_options.paint.high_quality);
        self.widgets.chb_horiz_glow.set_active(ui_options.paint.horizon_glow.visible);
        self.widgets.spb_horiz_glow_angle.set_value(ui_options.paint.horizon_glow.angle);

        let c = &ui_options.paint.horizon_glow.color;
        self.widgets.clrb_horiz_glow.set_rgba(&gdk::RGBA::new(c.r, c.g, c.b, c.a));

        let c = &ui_options.paint.eq_grid.line_color;
        self.widgets.clrb_eq_grid_line.set_rgba(&gdk::RGBA::new(c.r, c.g, c.b, c.a));

        let c = &ui_options.paint.eq_grid.text_color;
        self.widgets.clrb_eq_grid_text.set_rgba(&gdk::RGBA::new(c.r, c.g, c.b, c.a));
    }

    pub fn get_options(
        self:       &Rc<Self>,
        ui_options: &mut UiOptions,
    ) -> anyhow::Result<()> {

        ui_options.paint.high_quality = self.widgets.chb_high_qual.is_active();
        ui_options.paint.horizon_glow.visible = self.widgets.chb_horiz_glow.is_active();

        let c = self.widgets.clrb_horiz_glow.rgba();

        ui_options.paint.horizon_glow.color = Color { r: c.red(), g: c.green(), b: c.blue(), a: c.alpha() };


        ui_options.paint.horizon_glow.angle = self.widgets.spb_horiz_glow_angle.value();

        let c = self.widgets.clrb_eq_grid_line.rgba();
        ui_options.paint.eq_grid.line_color = Color { r: c.red(), g: c.green(), b: c.blue(), a: c.alpha() };

        let c = self.widgets.clrb_eq_grid_text.rgba();
        ui_options.paint.eq_grid.text_color = Color { r: c.red(), g: c.green(), b: c.blue(), a: c.alpha() };

        return Ok(());
    }

    pub fn exec(self: &Rc<Self>, on_apply: impl Fn() -> anyhow::Result<()> + 'static) {
        self.widgets.dialog.connect_response(clone!(@strong self as self_ => move |dlg, resp| {
            match resp {
                gtk::ResponseType::Ok | gtk::ResponseType::Apply => {
                    let ok = exec_and_show_error(dlg, || {
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
        self.widgets.dialog.show();
    }
}