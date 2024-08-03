use core::panic;
use std::{rc::Rc, path::{Path, PathBuf}};
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
) {
    let exec_res = fun();
    if let Err(err) = exec_res {
        let message = if cfg!(debug_assertions) {
            format!("{}\n\nat\n\n{}", err.to_string(), err.backtrace().to_string())
        } else {
            err.to_string()
        };
        show_error_message(window, "Error", &message);
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
    let Some(model) = cb.model() else { return true; };
    model.iter_first().is_none()
}

pub fn combobox_items_count<T: IsA<gtk::ComboBox>>(cb: &T) -> usize {
    let model = cb.model().unwrap();
    get_model_row_count(&model)
}

enum GtkHelperRoot {
    Builder(gtk::Builder),
    Container(gtk::Container),
}

pub struct UiHelper {
    root: GtkHelperRoot,
}

impl UiHelper {
    pub fn new_from_builder(bldr: &gtk::Builder) -> Self {
        Self {
            root: GtkHelperRoot::Builder(bldr.clone())
        }
    }

    pub fn object_by_id(&self, obj_bldr_id: &str) -> glib::Object {
        match &self.root {
            GtkHelperRoot::Builder(bldr) =>
                if let Some(result) = bldr.object(obj_bldr_id) {
                    result
                } else {
                    panic!("Object named {} not found", obj_bldr_id);
                },
            GtkHelperRoot::Container(_) => todo!(),
        }
    }

    ///////////////////////////////////////////////////////////////////////////

    pub fn enable_widgets(&self, force_set: bool, names: &[(&str, bool)]) {
        for (widget_name, enable) in names {
            let object = self.object_by_id(widget_name);
            let widget = object
                .downcast::<gtk::Widget>()
                .expect("Is not gtk::Widget");
            if force_set || widget.is_sensitive() != *enable {
                widget.set_sensitive(*enable);
            }
        }
    }

    pub fn show_widgets(&self, names: &[(&str, bool)]) {
        for (widget_name, visible) in names {
            let object = self.object_by_id(widget_name);
            let widget = object
                .downcast::<gtk::Widget>()
                .expect("Is not gtk::Widget");
            if widget.is_visible() != *visible {
                widget.set_visible(*visible);
            }
        }
    }

    ///////////////////////////////////////////////////////////////////////////

    // bool

    pub fn set_prop_bool(&self, name_and_prop: &str, value: bool) {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .set_property_from_value(prop, &value.into());
    }

    pub fn prop_bool(&self, name_and_prop: &str) -> bool {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .property_value(prop)
            .get::<bool>()
            .expect("Wrong property type")
    }

    pub fn set_prop_bool_ex(&self, obj_bldr_id: &str, prop_name: &str, value: bool) {
        self.object_by_id(obj_bldr_id)
            .set_property_from_value(prop_name, &value.into());
    }

    pub fn prop_bool_ex(&self, obj_bldr_id: &str, prop_name: &str) -> bool {
        self.object_by_id(obj_bldr_id)
            .property_value(prop_name)
            .get::<bool>()
            .expect("Wrong property type")
    }

    // &str /  String

    pub fn set_prop_str(&self, name_and_prop: &str, value: Option<&str>) {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .set_property_from_value(prop, &value.into());
    }

    pub fn prop_string(&self, name_and_prop: &str) -> Option<String> {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .property_value(prop)
            .get::<Option<String>>()
            .expect("Wrong property type")
    }

    pub fn set_prop_str_ex(&self, obj_bldr_id: &str, prop_name: &str, value: Option<&str>) {
        self.object_by_id(obj_bldr_id)
            .set_property_from_value(prop_name, &value.into());
    }

    pub fn prop_string_ex(&self, obj_bldr_id: &str, prop_name: &str) -> Option<String> {
        self.object_by_id(obj_bldr_id)
            .property_value(prop_name)
            .get::<Option<String>>()
            .expect("Wrong property type")
    }

    // f64

    pub fn set_prop_f64(&self, name_and_prop: &str, value: f64) {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .set_property_from_value(prop, &value.into());
    }

    pub fn prop_f64(&self, name_and_prop: &str) -> f64 {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .property_value(prop)
            .get::<f64>()
            .expect("Wrong property type")
    }

    pub fn set_prop_f64_ex(&self, obj_bldr_id: &str, prop_name: &str, value: f64) {
        self.object_by_id(obj_bldr_id)
            .set_property_from_value(prop_name, &value.into());
    }

    pub fn prop_f64_ex(&self, obj_bldr_id: &str, prop_name: &str) -> f64 {
        self.object_by_id(obj_bldr_id)
            .property_value(prop_name)
            .get::<f64>()
            .expect("Wrong property type")
    }

    // i32

    pub fn set_prop_i32(&self, name_and_prop: &str, value: i32) {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .set_property_from_value(prop, &value.into());
    }

    pub fn prop_i32(&self, name_and_prop: &str) -> i32 {
        let (name, prop) = Self::extract_name_and_prop(name_and_prop);
        self.object_by_id(name)
            .property_value(prop)
            .get::<i32>()
            .expect("Wrong property type")
    }

    pub fn set_prop_i32_ex(&self, obj_bldr_id: &str, prop_name: &str, value: i32) {
        self.object_by_id(obj_bldr_id)
            .set_property_from_value(prop_name, &value.into());
    }

    pub fn prop_i32_ex(&self, obj_bldr_id: &str, prop_name: &str) -> i32 {
        self.object_by_id(obj_bldr_id)
            .property_value(prop_name)
            .get::<i32>()
            .expect("Wrong property type")
    }


    ///////////////////////////////////////////////////////////////////////////

    pub fn set_fch_path(&self, obj_bldr_id: &str, path: Option<&Path>) {
        let widget = self.object_by_id(obj_bldr_id);
        let fch = widget
            .downcast::<gtk::FileChooserButton>()
            .expect("Widget is not gtk::FileChooserButton");
        let Some(path) = path else { return; };
        fch.set_filename(path);
    }

    pub fn fch_pathbuf(&self, obj_bldr_id: &str) -> Option<PathBuf> {
        let widget = self.object_by_id(obj_bldr_id);
        let fch = widget
            .downcast::<gtk::FileChooserButton>()
            .expect("Widget is not gtk::FileChooserButton");
        fch.filename()
    }

    pub fn set_range_value(&self, obj_bldr_id: &str, value: f64) {
        let widget = self.object_by_id(obj_bldr_id);
        let range = widget
            .downcast::<gtk::Range>()
            .expect("Widget is not gtk::Range");
        range.set_value(value);
    }

    pub fn range_value(&self, obj_bldr_id: &str) -> f64 {
        let widget = self.object_by_id(obj_bldr_id);
        let range = widget
            .downcast::<gtk::Range>()
            .expect("Widget is not gtk::Range");
        range.value()
    }

    pub fn is_combobox_empty(&self, widget_name: &str) -> bool {
        let widget = self.object_by_id(widget_name);
        let cb = widget
            .downcast::<gtk::ComboBox>()
            .expect("Widget is not gtk::ComboBox");
        is_combobox_empty(&cb)
    }

    pub fn set_color(&self, widget_name: &str, r: f64, g: f64, b: f64, a: f64) {
        let widget = self.object_by_id(widget_name);
        let color_button = widget
            .downcast::<gtk::ColorButton>();
        if let Ok(color_button) = color_button {
            color_button.set_rgba(&gdk::RGBA::new(r, g, b, a));
            return;
        }
        panic!("Setting color for widget '{}' not supported", widget_name);
    }

    pub fn color(&self, widget_name: &str) -> (f64, f64, f64, f64) {
        let widget = self.object_by_id(widget_name);
        let color_button = widget
            .downcast::<gtk::ColorButton>();
        if let Ok(color_button) = color_button {
            let color = color_button.rgba();
            return (color.red(), color.green(), color.blue(), color.alpha());
        }
        panic!("Getting color from widget '{}' not supported", widget_name);
    }

    ///////////////////////////////////////////////////////////////////////////

    fn extract_name_and_prop(name_and_prop: &str) -> (&str, &str) {
        let split_pos = name_and_prop
            .bytes()
            .position(|v| v == b'.')
            .expect("`.` not found");
        let name = &name_and_prop[..split_pos];
        let prop = &name_and_prop[split_pos+1..];
        (name, prop)
    }
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
