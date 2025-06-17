use std::rc::Rc;

use gtk::{prelude::*, glib::clone};
use macros::FromBuilder;

use super::gtk_utils::*;

#[derive(FromBuilder)]
struct Widgets {
    dialog:   gtk::Dialog,
    grd_main: gtk::Grid,
}

pub struct StartDialog {
    widgets: Widgets,
}

impl StartDialog {
    pub fn new(
        transient_for: &gtk::Window,
        caption:       &str,
        items:         &[(String, String)],
    ) -> Rc<Self> {
        let widgets = Widgets::from_builder_str(include_str!("resources/start_dialog.ui"));

        widgets.dialog.set_transient_for(Some(transient_for));
        widgets.dialog.set_title(caption);

        add_ok_and_cancel_buttons(
            &widgets.dialog,
            "Start",  gtk::ResponseType::Ok,
            "Cancel", gtk::ResponseType::Cancel,
        );
        set_dialog_default_button(&widgets.dialog);

        const START_ROW: usize = 2;

        for (index, (caption, value)) in items.iter().enumerate() {
            let row = index + START_ROW;
            widgets.grd_main.insert_row(row as i32);
            let lbl_caption = gtk::Label::builder()
                .label(caption)
                .halign(gtk::Align::Start)
                .visible(true)
                .build();

            let lbl_value = gtk::Label::builder()
                .label(format!("<b>{}</b>", value))
                .use_markup(true)
                .halign(gtk::Align::Start)
                .visible(true)
                .build();

                widgets.grd_main.attach(&lbl_caption, 0, row as i32, 1, 1);
                widgets.grd_main.attach(&lbl_value, 1, row as i32, 1, 1);
        }

        Rc::new(Self { widgets })
    }

    pub fn exec(self: &Rc<Self>, on_apply: impl Fn() -> anyhow::Result<()> + 'static) {
        self.widgets.dialog.connect_response(clone!(@strong self as self_ => move |dlg, resp| {
            match resp {
                gtk::ResponseType::Ok => {
                    dlg.close();
                    exec_and_show_error(Some(dlg), || {
                        on_apply()?;
                        Ok(())
                    });
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