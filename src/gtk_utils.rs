use std::{rc::Rc, path::{Path, PathBuf}};
use gtk::{prelude::*, gio, cairo, glib};


pub fn disable_scroll_for_most_of_widgets(builder: &gtk::Builder) {
    for object in builder.objects() {
        if let Some(spin) = object.downcast_ref::<gtk::SpinButton>() {
            spin.connect_scroll_event(|_, _| {
                glib::signal::Inhibit(true)
            });
        }
        if let Some(cb) = object.downcast_ref::<gtk::ComboBox>() {
            cb.connect_scroll_event(|_, _| {
                glib::signal::Inhibit(true)
            });
        }
        if let Some(scale) = object.downcast_ref::<gtk::Scale>() {
            scale.connect_scroll_event(|_, _| {
                glib::signal::Inhibit(true)
            });
        }
        if let Some(btn) = object.downcast_ref::<gtk::FileChooserButton>() {
            btn.connect_scroll_event(|_, _| {
                glib::signal::Inhibit(true)
            });
        }
    }
}

pub fn connect_action<Fun, T: 'static>(
    window:   &gtk::ApplicationWindow,
    data:     &Rc<T>,
    act_name: &str,
    fun:      Fun
) where Fun: Fn(&Rc<T>) + 'static {
    let data_copy = Rc::clone(data);
    let action = gio::SimpleAction::new(act_name, None);
    action.connect_activate(move |_, _| fun(&data_copy));
    window.add_action(&action);
}

pub fn enable_actions(
    window: &gtk::ApplicationWindow,
    items:  &[(&str, bool)]
) {
    for &(action, enabled) in items {
        if let Some(action) = window.lookup_action(action) {
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
}

pub fn show_error_message(
    window:  &gtk::ApplicationWindow,
    title:   &str,
    message: &str,
) {
    let dialog = gtk::MessageDialog::builder()
        .transient_for(window)
        .title(title)
        .text(message)
        .modal(true)
        .message_type(gtk::MessageType::Error)
        .buttons(gtk::ButtonsType::Close)
        .build();
    dialog.show();
    dialog.connect_response(move |dlg, _| {
        dlg.close();
    });
}

pub fn exec_and_show_error(
    window: &gtk::ApplicationWindow,
    fun:    impl FnOnce() -> anyhow::Result<()>
) {
    let exec_res = fun();
    if let Err(err) = exec_res {
        show_error_message(window, "Error", err.to_string().as_str());
    }
}

pub fn enable_widgets(
    builder: &gtk::Builder,
    names:   &[(&str, bool)]
) {
    for (widget_name, enable) in names {
        let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
        if widget.is_sensitive() != *enable {
            widget.set_sensitive(*enable);
        }
    }
}

pub fn show_widgets(
    builder: &gtk::Builder,
    names:   &[(&str, bool)]
) {
    for (widget_name, visible) in names {
        let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
        if widget.is_visible() != *visible {
            widget.set_visible(*visible);
        }
    }
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
    cb.model()
        .unwrap()
        .iter_first()
        .is_none()
}

pub fn combobox_items_count<T: IsA<gtk::ComboBox>>(cb: &T) -> usize {
    let model = cb.model().unwrap();
    get_model_row_count(&model)
}

pub fn set_f64(
    builder:     &gtk::Builder,
    widget_name: &str,
    value:       f64
) {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Some(spin_button) = widget.downcast_ref::<gtk::SpinButton>() {
        spin_button.set_value(value);
    } else if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        scale.set_value(value);
    } else {
        panic!("Widget named {} is not supported", widget_name);
    }
}

pub fn set_bool(
    builder:     &gtk::Builder,
    widget_name: &str,
    value:       bool
) {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Some(checkbutton) = widget.downcast_ref::<gtk::CheckButton>() {
        checkbutton.set_active(value);
    } else if let Some(exp) = widget.downcast_ref::<gtk::Expander>() {
        exp.set_expanded(value);
    } else {
        panic!("Widget named {} is not supported", widget_name);
    }
}

pub fn set_str(
    builder:     &gtk::Builder,
    widget_name: &str,
    text:        &str
) {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_label(text);
    } else if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        entry.set_text(text);
    } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        button.set_label(text);
    } else {
        panic!("Widget named {} is not supported", widget_name);
    }
}

pub fn set_active_id(
    builder:     &gtk::Builder,
    widget_name: &str,
    active_id:   Option<&str>
) {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Ok(combobox) = widget.downcast::<gtk::ComboBox>() {
        if combobox.active_id().as_deref() != active_id {
            combobox.set_active_id(active_id);
        }
    } else {
        panic!("Widget named {} is not supported", widget_name);
    }
}

pub fn set_path(
    builder:     &gtk::Builder,
    widget_name: &str,
    path:        Option<&Path>
) {
    let Some(path) = path else {
        return;
    };
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Ok(fch) = widget.downcast::<gtk::FileChooserButton>() {
        fch.set_filename(path);
    } else {
        panic!("Widget named {} is not supported", widget_name);
    }
}

pub fn get_f64(builder: &gtk::Builder, widget_name: &str) -> f64 {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Some(spin_button) = widget.downcast_ref::<gtk::SpinButton>() {
        return spin_button.value();
    } else if let Some(scale) = widget.downcast_ref::<gtk::Scale>() {
        return scale.value();
    }
    panic!("Widget named {} is not supported", widget_name);
}

pub fn get_bool(builder: &gtk::Builder, widget_name: &str) -> bool {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Some(checkbutton) = widget.downcast_ref::<gtk::CheckButton>() {
        return checkbutton.is_active();
    } else if let Some(exp) = widget.downcast_ref::<gtk::Expander>() {
        return exp.is_expanded();
    }
    panic!("Widget named {} is not supported", widget_name);
}

pub fn get_active_id(builder: &gtk::Builder, widget_name: &str) -> Option<String> {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Ok(combobox) = widget.downcast::<gtk::ComboBox>() {
        return combobox.active_id().map(|s| s.to_string());
    }
    panic!("Widget named {} is not supported", widget_name);
}

pub fn get_string(builder: &gtk::Builder, widget_name: &str) -> String {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Ok(entry) = widget.downcast::<gtk::Entry>() {
        return entry.text().to_string();
    }
    panic!("Widget named {} is not supported", widget_name);
}

pub fn get_pathbuf(builder: &gtk::Builder, widget_name: &str) -> Option<PathBuf> {
    let widget = builder.object::<gtk::Widget>(widget_name).unwrap();
    if let Ok(fch) = widget.downcast::<gtk::FileChooserButton>() {
        return fch.filename();
    }
    panic!("Widget named {} is not supported", widget_name);
}

pub fn draw_progress_bar(
    area:     &gtk::DrawingArea,
    cr:       &cairo::Context,
    progress: f64,
    text:     &str,
) -> anyhow::Result<()> {
    let width = area.allocated_width() as f64;
    let height = area.allocated_height() as f64;
    let style_context = area.style_context();
    let fg = style_context.color(gtk::StateFlags::ACTIVE);
    let br = if fg.green() < 0.5 { 1.0 } else { 0.5 };
    let bg_color = if progress < 1.0 {
        (br, br, 0.0, 0.7)
    } else {
        (0.0, br, 0.0, 0.5)
    };
    cr.set_source_rgba(bg_color.0, bg_color.1, bg_color.2, bg_color.3);
    cr.rectangle(0.0, 0.0, width * progress, height);
    cr.fill()?;

    cr.set_font_size(height);
    let te = cr.text_extents(text)?;

    if !text.is_empty() {
        cr.set_source_rgba(fg.red(), fg.green(), fg.blue(), 0.33);
        cr.rectangle(0.0, 0.0, width, height);
        cr.stroke()?;
    }

    cr.set_source_rgb(fg.red(), fg.green(), fg.blue());
    cr.move_to((width - te.width()) / 2.0, (height - te.height()) / 2.0 - te.y_bearing());
    cr.show_text(text)?;

    Ok(())
}