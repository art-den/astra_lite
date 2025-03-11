#![allow(dead_code)]

use core::panic;
use std::{rc::Rc, path::PathBuf};
use gtk::{prelude::*, gio, glib, glib::clone, gdk};

pub fn set_dialog_default_button<T: IsA<gtk::Dialog>>(dialog: &T) {
    use gtk::ResponseType::*;
    for resp in [Ok, Yes, Accept, Apply] {
        if let Some(btn) = dialog.widget_for_response(resp) {
            btn.grab_default();
            btn.style_context().add_class("suggested-action");
            break;
        }
    }
}

pub fn add_ok_and_cancel_buttons(
    dialog:      &gtk::Dialog,
    ok_cap:      &str,
    ok_type:     gtk::ResponseType,
    cancel_cap:  &str,
    cancel_type: gtk::ResponseType,
) {
    if cfg!(target_os = "windows") {
        dialog.add_buttons(&[(ok_cap, ok_type), (cancel_cap, cancel_type)]);
    } else {
        dialog.add_buttons(&[(cancel_cap, cancel_type), (ok_cap, ok_type)]);
    }
}

pub fn add_ok_cancel_and_apply_buttons(
    dialog:      &gtk::Dialog,
    ok_cap:      &str,
    ok_type:     gtk::ResponseType,
    cancel_cap:  &str,
    cancel_type: gtk::ResponseType,
    apply_cap:   &str,
    apply_type:  gtk::ResponseType,
) {
    if cfg!(target_os = "windows") {
        dialog.add_buttons(&[
            (ok_cap,     ok_type),
            (cancel_cap, cancel_type),
            (apply_cap,  apply_type),
        ]);
    } else {
        dialog.add_buttons(&[
            (apply_cap,  apply_type),
            (cancel_cap, cancel_type),
            (ok_cap,     ok_type)
        ]);
    }
}


pub fn disable_scroll_for_most_of_widgets(builder: &gtk::Builder) {
    for object in builder.objects() {
        if let Some(spin) = object.downcast_ref::<gtk::SpinButton>() {
            spin.connect_scroll_event(|_, _| {
                glib::Propagation::Stop
            });
        }
        if let Some(cb) = object.downcast_ref::<gtk::ComboBox>() {
            cb.connect_scroll_event(|_, _| {
                glib::Propagation::Stop
            });
        }
        if let Some(scale) = object.downcast_ref::<gtk::Scale>() {
            scale.connect_scroll_event(|_, _| {
                glib::Propagation::Stop
            });
        }
        if let Some(btn) = object.downcast_ref::<gtk::FileChooserButton>() {
            btn.connect_scroll_event(|_, _| {
                glib::Propagation::Stop
             });
        }
    }
}

pub fn connect_action_rc<Fun, T>(
    action_map: &impl IsA<gio::ActionMap>,
    data:       &Rc<T>,
    act_name:   &str,
    fun:        Fun
) where
    Fun: Fn(&Rc<T>) + 'static,
    T: 'static,
{
    let action = gio::SimpleAction::new(act_name, None);
    action.connect_activate(clone!(@weak data => move |_, _|
        fun(&data);
    ));
    action_map.add_action(&action);
}

pub fn connect_action<Fun, T>(
    action_map: &impl IsA<gio::ActionMap>,
    data:       &Rc<T>,
    act_name:   &str,
    fun:        Fun
) where
    Fun: Fn(&T) + 'static,
    T: 'static,
{
    let action = gio::SimpleAction::new(act_name, None);
    action.connect_activate(clone!(@weak data => move |_, _|
        fun(&data);
    ));
    action_map.add_action(&action);
}

pub fn enable_action(
    action_map: &impl IsA<gio::ActionMap>,
    action:     &str,
    enabled:    bool,
) {
    if let Some(action) = action_map.lookup_action(action) {
        let sa = action
            .downcast::<gio::SimpleAction>()
            .expect("Is not gio::SimpleAction");
        if sa.is_enabled() != enabled {
            sa.set_enabled(enabled);
        }
    } else {
        panic!("Action {} not found", action);
    }
}

pub fn enable_actions(
    window: &gtk::ApplicationWindow,
    items:  &[(&str, bool)]
) {
    for &(action, enabled) in items {
        enable_action(window, action, enabled);
    }
}

pub fn show_message(
    window:   &impl IsA<gtk::Window>,
    title:    &str,
    text:     &str,
    msg_type: gtk::MessageType,
) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(window)
        .title(title)
        .text(text)
        .modal(true)
        .message_type(msg_type)
        .buttons(gtk::ButtonsType::Close)
        .build();

    dialog.show();

    dialog.connect_response(move |dlg, _| {
        dlg.close();
    });
}

pub fn show_error_message(
    window:  &impl IsA<gtk::Window>,
    title:   &str,
    message: &str,
) {
    show_message(window, title, message, gtk::MessageType::Error);
}

pub fn exec_and_show_error(
    window: &impl IsA<gtk::Window>,
    fun:    impl FnOnce() -> anyhow::Result<()>
) -> bool {
    let exec_res = fun();
    if let Err(err) = exec_res {
        let message = if cfg!(debug_assertions) {
            format!("{}\n\nat\n\n{}", err.to_string(), err.backtrace().to_string())
        } else {
            err.to_string()
        };
        show_error_message(window, "Error", &message);
        return false;
    }
    return true;
}

pub fn get_model_row_count(model: &gtk::TreeModel) -> usize {
    let Some(iter) = model.iter_first() else {
        return 0;
    };
    let mut result = 1;
    while model.iter_next(&iter) {
        result += 1;
    }
    result
}

pub fn get_list_view_selected_row(tree: &gtk::TreeView) -> Option<i32> {
    if let [before_selection] =
    tree.selection().selected_rows().0.as_slice() {
        if let &[row] = before_selection.indices().as_slice() {
            Some(row)
        } else {
            None
        }
    } else {
        None
    }
}

pub fn is_combobox_empty<T: IsA<gtk::ComboBox>>(cb: &T) -> bool {
    let Some(model) = cb.model() else { return true; };
    model.iter_first().is_none()
}

pub fn combobox_items_count<T: IsA<gtk::ComboBox>>(cb: &T) -> usize {
    let model = cb.model().unwrap();
    get_model_row_count(&model)
}

pub fn select_file_name_to_save(
    parent:        &impl IsA<gtk::Window>,
    title:         &str,
    filter_name:   &str,
    filter_ext:    &str,
    ext:           &str,
    def_file_name: &str,
) -> Option<PathBuf> {
    let ff = gtk::FileFilter::new();
    ff.set_name(Some(filter_name));
    ff.add_pattern(filter_ext);
    let fc = gtk::FileChooserDialog::builder()
        .action(gtk::FileChooserAction::Save)
        .title(title)
        .filter(&ff)
        .modal(true)
        .transient_for(parent)
        .build();
    fc.set_current_name(def_file_name);
    add_ok_and_cancel_buttons(
        fc.upcast_ref::<gtk::Dialog>(),
        "_Save",   gtk::ResponseType::Accept,
        "_Cancel", gtk::ResponseType::Cancel,
    );
    let resp = fc.run();
    fc.close();
    if resp != gtk::ResponseType::Accept {
        None
    } else {
        Some(fc.file()?
        .path()?
        .with_extension(ext))
    }
}

pub const DEFAULT_DPMM: f64 = 3.8;

pub fn get_widget_dpmm(widget: &impl IsA<gtk::Widget>) -> Option<(f64, f64)> {
    widget.window()
        .and_then(|window|
            widget.display().monitor_at_window(&window)
        )
        .map(|monitor| {
            let g = monitor.geometry();
            (g.height() as f64 / monitor.height_mm() as f64,
            g.width() as f64 / monitor.width_mm() as f64)
        })
}

pub enum FontSize {
    Pt(f64)
}

pub fn font_size_to_pixels(size: FontSize, dpmm_y: f64) -> f64 {
    match size {
        FontSize::Pt(pt) => 25.4 * dpmm_y * pt / 72.272
    }
}

pub fn init_list_store_model_for_treeview(
    tv:      &gtk::TreeView,
    columns: &[(&str, glib::types::Type, &str)]
) -> gtk::ListStore {
    let types = columns.iter().map(|(_, tp, _)| *tp).collect::<Vec<_>>();
    let model = gtk::ListStore::new(&types);
    for (idx, (col_name, _, attr)) in columns.into_iter().enumerate() {
        let cell_text = gtk::CellRendererText::new();
        let col = gtk::TreeViewColumn::builder()
            .title(*col_name)
            .resizable(true)
            .clickable(true)
            .visible(true)
            .build();
        TreeViewColumnExt::pack_start(&col, &cell_text, true);
        TreeViewColumnExt::add_attribute(&col, &cell_text, *attr, idx as i32);
        tv.append_column(&col);
    }
    tv.set_model(Some(&model));
    model
}

pub fn limit_pixbuf_by_longest_size(
    pixbuf: gdk::gdk_pixbuf::Pixbuf,
    max_size: i32,
) -> gdk::gdk_pixbuf::Pixbuf {
    if pixbuf.width() > max_size
    || pixbuf.height() > max_size {
        let longest_size = i32::max(pixbuf.width(), pixbuf.height());
        let new_width = max_size * pixbuf.width() / longest_size;
        let new_height = max_size * pixbuf.height() / longest_size;
        pixbuf.scale_simple(
            new_width as _,
            new_height as _,
            gtk::gdk_pixbuf::InterpType::Tiles,
        ).unwrap()
    } else {
        pixbuf
    }
}

pub fn clear_container(container: &impl IsA<gtk::Container>) {
    let children = container.children();
    for child in children.iter().rev() {
        if let Some(child) = child.downcast_ref::<gtk::Widget>() {
            container.remove(child);
        }
    }
}