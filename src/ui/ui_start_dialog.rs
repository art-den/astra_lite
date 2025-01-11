use std::rc::Rc;

use gtk::{prelude::*, glib::clone};

use crate::utils::gtk_utils;

pub struct StartDialog {
    dialog: gtk::Dialog,
}

impl StartDialog {
    pub fn new(
        transient_for: &gtk::Window,
        caption:       &str,
        items:         &[(String, String)],
    ) -> Rc<Self> {
        let builder = gtk::Builder::from_string(include_str!("resources/start_dialog.ui"));
        let dialog = builder.object::<gtk::Dialog>("dialog").unwrap();
        dialog.set_transient_for(Some(transient_for));

        dialog.set_title(caption);

        gtk_utils::add_ok_and_cancel_buttons(
            &dialog,
            "Start",  gtk::ResponseType::Ok,
            "Cancel", gtk::ResponseType::Cancel,
        );
        gtk_utils::set_dialog_default_button(&dialog);

        let grid = builder.object::<gtk::Grid>("grd_main").unwrap();

        const START_ROW: usize = 2;

        for (index, (caption, value)) in items.into_iter().enumerate() {
            let row = index + START_ROW;
            grid.insert_row(row as i32);
            let lbl_caption = gtk::Label::builder()
                .label(caption)
                .halign(gtk::Align::Start)
                .visible(true)
                .build();

            let lbl_value = gtk::Label::builder()
                .label(&format!("<b>{}</b>", value))
                .use_markup(true)
                .halign(gtk::Align::Start)
                .visible(true)
                .build();

            grid.attach(&lbl_caption, 0, row as i32, 1, 1);
            grid.attach(&lbl_value, 1, row as i32, 1, 1);
        }

        Rc::new(Self { dialog })
    }

    pub fn exec(self: &Rc<Self>, on_apply: impl Fn() -> anyhow::Result<()> + 'static) {
        self.dialog.connect_response(clone!(@strong self as self_ => move |dlg, resp| {
            match resp {
                gtk::ResponseType::Ok => {
                    dlg.close();
                    gtk_utils::exec_and_show_error(dlg, || {
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
        self.dialog.show();
    }
}