use gtk::glib::translate::ToGlibPtr;
use gtk::{gdk, prelude::*};

pub fn test_init() {
    // gdk_test_simulate_button() works via XSendEvent, but with XI2 enabled
    // GDK selects pointer events via XISelectEvents, so core synthetic
    // button events are never delivered to the client. This is the same
    // trick gtk_test_init() uses. Must be called before the GDK display
    // is opened (i.e. before application.run()).

    unsafe {
        gtk::gdk::ffi::gdk_disable_multidevice();
    }
}

pub fn test_pause_ms(mut time_ms: usize) {
    const PERIOD: usize = 10;
    while time_ms >= PERIOD {
        time_ms -= PERIOD;
        while gtk::events_pending() {
            gtk::main_iteration();
        }
        std::thread::sleep(std::time::Duration::from_millis(PERIOD as _));
    }
}

// The main window is the only GtkApplicationWindow
// (message dialogs are GtkMessageDialog)

fn main_window(app: &gtk::Application) -> gtk::ApplicationWindow {
    app.windows()
        .into_iter()
        .find_map(|w| w.downcast::<gtk::ApplicationWindow>().ok())
        .expect("Main window not found")
}

// Search by widget name property (not by builder id)
pub fn widget_by_name<T: IsA<gtk::Widget>>(app: &gtk::Application, name: &str) -> T {
    let window = main_window(app);
    let widget = find_widget_by_name(window.upcast_ref(), name)
        .unwrap_or_else(|| panic!("Widget with name '{}' not found", name));
    widget.downcast::<T>().unwrap_or_else(|_| {
        panic!(
            "Widget with name '{}' is not of type {}",
            name,
            std::any::type_name::<T>()
        )
    })
}

fn find_widget_by_name(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if widget.widget_name() == name {
        return Some(widget.clone());
    }
    if let Some(notebook) = widget.downcast_ref::<gtk::Notebook>() {
        // Tab labels are internal children of a notebook and are not returned
        // by container.children() (GTK3's gtk_notebook_forall includes them
        // only with include_internals == TRUE).
        for page in notebook.children() {
            if let Some(tab_label) = notebook.tab_label(&page)
            && let Some(found) = find_widget_by_name(&tab_label, name) {
                return Some(found);
            }
        }
    }
    if let Some(container) = widget.downcast_ref::<gtk::Container>() {
        for child in container.children() {
            if let Some(child) = child.downcast_ref::<gtk::Widget>()
            && let Some(found) = find_widget_by_name(child, name) {
                return Some(found);
            }
        }
    }
    None
}

// GTK delivers a button event to the widget that owns the target GdkWindow (see
// gtk_get_event_widget in gtkmain.c), and windowless widgets like GtkButton register an
// input-only event window on their parent window at realize time. So the click must be
// simulated on that window, not on the parent window returned by gtk_widget_get_window(),
// otherwise the event never reaches the widget. This mirrors
// test_find_widget_input_windows() from gtk/gtktestutils.c.
fn widget_input_window(widget: &gtk::Widget) -> Option<gdk::Window> {
    let window = widget.window()?;
    let target: *mut gtk::ffi::GtkWidget = widget.to_glib_none().0;
    let user_data = |win: &gdk::Window| -> Option<*mut gtk::ffi::GtkWidget> {
        let ptr: *mut gdk::ffi::GdkWindow = win.to_glib_none().0;
        let mut data: gtk::glib::ffi::gpointer = std::ptr::null_mut();
        unsafe {
            gdk::ffi::gdk_window_get_user_data(ptr, &mut data);
        }
        (!data.is_null()).then(|| data.cast())
    };
    if user_data(&window) == Some(target) {
        return Some(window);
    }
    window.children().into_iter().find(|child| user_data(child) == Some(target))
}

// Find the GdkWindow a click on `widget` must be sent to: the event window of
// the nearest ancestor (or the widget itself) that handles button events.
// Windowless widgets (like a notebook tab label) own no GdkWindow, so the event
// has to reach the nearest ancestor that handles button presses and hit-tests
// the click point itself (GtkNotebook finds the tab under the point).
fn click_window_for(widget: &gtk::Widget) -> Option<(gdk::Window, gtk::Widget)> {
    let mut current: Option<gtk::Widget> = Some(widget.clone());
    while let Some(w) = current {
        if let Some(window) = widget_input_window(&w) {
            return Some((window, w));
        }
        current = w.parent();
    }
    None
}

// Note: requires gdk_disable_multidevice() to be called before the display is opened
// (see main.rs): with XI2 enabled GDK selects pointer events via XISelectEvents, so the
// core button events sent by gdk_test_simulate_button() are never delivered to the client.
pub fn test_widget_click(name: &str, app: &gtk::Application) {
    let widget = widget_by_name::<gtk::Widget>(app, name);
    let (window, x, y) = match click_window_for(&widget) {
        // The event window is positioned at the handler's allocation origin, so
        // translate the widget origin into the handler's coordinate space and
        // click the widget center.
        Some((window, handler)) => {
            let (dx, dy) = widget.translate_coordinates(&handler, 0, 0).unwrap_or((0, 0));
            (
                window,
                dx + widget.allocated_width() / 2,
                dy + widget.allocated_height() / 2,
            )
        }
        // -1, -1 is the window center, the same convention gtk_test_widget_click uses.
        None => (
            widget.window().expect("Widget is not realized, so it has no GdkWindow"),
            -1,
            -1,
        ),
    };
    // A click is a button press followed by a release.
    let pressed = gdk::test_simulate_button(
        &window,
        x,
        y,
        1,
        gdk::ModifierType::empty(),
        gdk::EventType::ButtonPress,
    );

    test_pause_ms(100);

    let released = gdk::test_simulate_button(
        &window,
        x,
        y,
        1,
        gdk::ModifierType::empty(),
        gdk::EventType::ButtonRelease,
    );

    test_pause_ms(100);

    if !pressed || !released {
        panic!("Failed to simulate click on widget '{}'", name);
    }
}

// Save a PNG screenshot of the main window to investigate UI issues
// (e.g. where a simulated click landed).
#[allow(dead_code)]
pub fn test_save_main_window_screenshot(app: &gtk::Application, path: &str) {
    let main_window = main_window(app);

    let width = main_window.allocated_width();
    let height = main_window.allocated_height();
    let gdk_window = main_window.window().expect("Main window is not realized");
    let pixbuf = gdk_window
        .pixbuf(0, 0, width, height)
        .expect("Failed to read main window pixels");
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    pixbuf.savev(path, "png", &[]).expect("Failed to save screenshot");
}
