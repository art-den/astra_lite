use std::{rc::Rc, sync::Arc};
use gtk::{prelude::*, glib::clone};
use crate::{indi, Options};
use super::{gtk_utils, sky_map::painter::Color, ui_skymap::UiOptions};

pub struct SkymapOptionsDialog {
    builder: gtk::Builder,
    dialog:  gtk::Dialog,
    indi:    Arc<indi::Connection>,
}

impl SkymapOptionsDialog {
    pub fn new(indi: &Arc<indi::Connection>) -> Rc<Self> {
        let builder = gtk::Builder::from_string(include_str!("resources/skymap_options.ui"));
        let dialog = builder.object::<gtk::Dialog>("dialog").unwrap();

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
            indi: Arc::clone(indi)
        });

        let btn_get_from_devs = builder.object::<gtk::Button>("btn_get_from_devs").unwrap();
        btn_get_from_devs.connect_clicked(clone!(@strong result as self_ => move |btn| {
            self_.handler_btn_get_from_devs_pressed(btn.upcast_ref::<_>());
        }));

        result
    }

    pub fn show_options(
        self:       &Rc<Self>,
        ui_options: &UiOptions,
        options:    &Options
    ) {
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        ui.set_prop_str("e_lat.text", Some(&indi::value_to_sexagesimal(options.sky_map.latitude, true, 9)));
        ui.set_prop_str("e_long.text", Some(&indi::value_to_sexagesimal(options.sky_map.longitude, true, 9)));
        ui.set_prop_bool("chb_high_qual.active", ui_options.paint.high_quality);
        ui.set_prop_bool("chb_horiz_glow.active", ui_options.paint.horizon_glow.enabled);
        ui.set_prop_f64("spb_horiz_glow_angle.value", ui_options.paint.horizon_glow.angle);

        let c = &ui_options.paint.horizon_glow.color;
        ui.set_color("clrb_horiz_glow", c.r, c.g, c.b, c.a);

        let c = &ui_options.paint.eq_grid_line_color;
        ui.set_color("clrb_eq_grid_line", c.r, c.g, c.b, c.a);

        let c = &ui_options.paint.eq_grid_text_color;
        ui.set_color("clrb_eq_grid_text", c.r, c.g, c.b, c.a);
    }

    pub fn get_options(
        self:       &Rc<Self>,
        ui_options: &mut UiOptions,
        options:    &mut Options
    ) -> anyhow::Result<()> {
        let mut err_str = String::new();
        let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);

        let latitude_str = ui.prop_string("e_lat.text").unwrap_or_default();
        if let Some(latitude) = indi::sexagesimal_to_value(&latitude_str) {
            options.sky_map.latitude = latitude;
        } else {
            err_str += &format!("Wrong latitude: {}\n", latitude_str);
        }
        let longitude_str = ui.prop_string("e_long.text").unwrap_or_default();
        if let Some(longitude) = indi::sexagesimal_to_value(&longitude_str) {
            options.sky_map.longitude = longitude;
        } else {
            err_str += &format!("Wrong longitude: {}\n", longitude_str);
        }
        if !err_str.is_empty() {
            anyhow::bail!(err_str);
        }

        ui_options.paint.high_quality = ui.prop_bool("chb_high_qual.active");
        ui_options.paint.horizon_glow.enabled = ui.prop_bool("chb_horiz_glow.active");
        let (r, g, b, a) = ui.color("clrb_horiz_glow");
        ui_options.paint.horizon_glow.color = Color { r, g, b, a };

        ui_options.paint.horizon_glow.angle = ui.prop_f64("spb_horiz_glow_angle.value");

        let (r, g, b, a) = ui.color("clrb_eq_grid_line");
        ui_options.paint.eq_grid_line_color = Color { r, g, b, a };

        let (r, g, b, a) = ui.color("clrb_eq_grid_text");
        ui_options.paint.eq_grid_text_color = Color { r, g, b, a };

        return Ok(());
    }

    pub fn exec(self: &Rc<Self>, on_apply: impl Fn() -> anyhow::Result<()> + 'static) {
        self.dialog.connect_response(clone!(@strong self as self_ => move |dlg, resp| {
            if resp == gtk::ResponseType::Cancel {
                dlg.close();
            }
            let ok = gtk_utils::exec_and_show_error(dlg, || {
                on_apply()?;
                Ok(())
            });
            if ok && resp == gtk::ResponseType::Ok {
                dlg.close();
            }
        }));
        self.dialog.show();
    }

    fn handler_btn_get_from_devs_pressed(self: &Rc<Self>, menu_widget: &gtk::Widget) {
        gtk_utils::exec_and_show_error(&self.dialog, || {
            let indi = &self.indi;
            if indi.state() != indi::ConnState::Connected {
                anyhow::bail!("INDI is not connected!");
            }
            let devices = indi.get_devices_list_by_interface(
                indi::DriverInterface::GPS |
                indi::DriverInterface::TELESCOPE
            );

            let result: Vec<_> = devices
                .iter()
                .filter_map(|dev|
                    indi.get_geo_lat_long_elev(&dev.name)
                        .ok()
                        .map(|(lat,long,_)| (dev, lat, long))
                )
                .filter(|(_, lat,long)| *lat != 0.0 && *long != 0.0)
                .collect();

            if result.is_empty() {
                anyhow::bail!("No GPS or geographic data found!");
            }

            if result.len() == 1 {
                let (_, latitude, longitude) = result[0];
                let ui = gtk_utils::UiHelper::new_from_builder(&self.builder);
                ui.set_prop_str("e_lat.text", Some(&indi::value_to_sexagesimal(latitude, true, 9)));
                ui.set_prop_str("e_long.text", Some(&indi::value_to_sexagesimal(longitude, true, 9)));
                return Ok(());
            }

            let menu = gtk::Menu::new();
            for (dev, lat, long) in result {
                let mi_text = format!(
                    "{} {} ({})",
                    indi::value_to_sexagesimal(lat, true, 9),
                    indi::value_to_sexagesimal(long, true, 9),
                    dev.name
                );
                let menu_item = gtk::MenuItem::builder().label(mi_text).build();
                menu.append(&menu_item);
                let builder = self.builder.clone();
                menu_item.connect_activate(move |_| {
                    let ui = gtk_utils::UiHelper::new_from_builder(&builder);
                    ui.set_prop_str("e_lat.text", Some(&indi::value_to_sexagesimal(lat, true, 9)));
                    ui.set_prop_str("e_long.text", Some(&indi::value_to_sexagesimal(long, true, 9)));
                });
            }
            menu.set_attach_widget(Some(menu_widget));
            menu.show_all();
            menu.popup_easy(gtk::gdk::ffi::GDK_BUTTON_SECONDARY as u32, 0);

            Ok(())
        });

    }
}