use std::{rc::Rc, cell::{RefCell, Cell}, time::Duration, collections::HashMap, hash::Hash};
use gtk::{prelude::*, glib, glib::clone, cairo, gdk};
use crate::image::histogram::*;

pub struct ExclusiveCaller {
    busy: Cell<bool>,
}

impl ExclusiveCaller {
    pub fn new() -> Self {
        Self {
            busy: Cell::new(false),
        }
    }

    pub fn exec(&self, mut fun: impl FnMut()) {
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        fun();
        self.busy.set(false);
    }
}

const DELAYED_ACTIONS_TIMER_PERIOD_MS: u64 = 100;

struct DelayedActionsData<Action: Hash+Eq + 'static> {
    items:         HashMap<Action, u64>,
    period:        u64,
    event_handler: Option<Box<dyn Fn(&Action) + 'static>>,
}

pub struct DelayedActions<Action: Hash+Eq + 'static> {
    data: Rc<RefCell<DelayedActionsData<Action>>>,
}

impl<Action: Hash+Eq + 'static> DelayedActions<Action> {
    pub fn new(period: u64) -> Self {
        let data = Rc::new(RefCell::new(DelayedActionsData {
            items:         HashMap::new(),
            event_handler: None,
            period,
        }));
        glib::timeout_add_local(
            Duration::from_millis(DELAYED_ACTIONS_TIMER_PERIOD_MS),
            clone!(@weak data => @default-return glib::ControlFlow::Break,
            move || {
                let mut data = data.borrow_mut();
                if let Some(event_handler) = data.event_handler.take() {
                    for (key, value) in &mut data.items {
                        if *value > DELAYED_ACTIONS_TIMER_PERIOD_MS {
                            *value -= DELAYED_ACTIONS_TIMER_PERIOD_MS;
                        } else {
                            *value = 0;
                            event_handler(key);
                        }
                    }
                    data.event_handler = Some(event_handler);
                    data.items.retain(|_, v| { *v != 0 });
                }
                glib::ControlFlow::Continue
            })
        );
        DelayedActions { data }
    }

    pub fn set_event_handler(&self, event_handler: impl Fn(&Action) + 'static) {
        let mut data = self.data.borrow_mut();
        data.event_handler = Some(Box::new(event_handler));
    }

    pub fn schedule(&self, item: Action) {
        let mut data = self.data.borrow_mut();
        let period = data.period;
        data.items.insert(item, period);
    }

    pub fn schedule_ex(&self, item: Action, period: u64) {
        let mut data = self.data.borrow_mut();
        data.items.insert(item, period);
    }
}

pub fn draw_histogram(
    hist:   &Histogram,
    area:   &gtk::DrawingArea,
    cr:     &cairo::Context,
    width:  i32,
    height: i32,
    log_y:  bool,
) -> anyhow::Result<()> {
    if width == 0 { return Ok(()); }

    let p0 = "0%";
    let p25 = "25%";
    let p50 = "50%";
    let p75 = "75%";
    let p100 = "100%";

    let fg = area.style_context().color(gtk::StateFlags::NORMAL);
    let bg = area.style_context().lookup_color("theme_base_color").unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));

    let max_y_text_te = cr.text_extents(p100)?;
    let left_margin = max_y_text_te.width() + 5.0;
    let right_margin = 3.0;
    let top_margin = 3.0;
    let bottom_margin = cr.text_extents(p0)?.width() + 3.0;
    let area_width = width as f64 - left_margin - right_margin;
    let area_height = height as f64 - top_margin - bottom_margin;

    cr.set_line_width(1.0);

    cr.set_source_rgb(bg.red(), bg.green(), bg.blue());
    cr.rectangle(left_margin, top_margin, area_width, area_height);
    cr.fill()?;

    cr.set_source_rgba(fg.red(), fg.green(), fg.blue(), 0.5);
    cr.rectangle(left_margin, top_margin, area_width, area_height);
    cr.stroke()?;

    let hist_chans = [ hist.r.as_ref(), hist.g.as_ref(), hist.b.as_ref(), hist.l.as_ref() ];

    let max_count = hist_chans.iter()
        .filter_map(|v| v.map(|v| v.count))
        .max()
        .unwrap_or(0);

    let total_max_v = hist_chans.iter()
        .filter_map(|v| v.map(|v| (v.count, v.freq.iter().max())))
        .map(|(cnt, v)| *v.unwrap_or(&0) as u64 * max_count as u64 / cnt as u64)
        .max()
        .unwrap_or(0);

    if total_max_v != 0 && max_count != 0 {
        let mut total_max_v = total_max_v as f64;
        if log_y {
            total_max_v = f64::log10(total_max_v);
        }

        let paint_channel = |chan: &Option<HistogramChan>, r, g, b, a| -> anyhow::Result<()> {
            let Some(chan) = chan.as_ref() else { return Ok(()); };
            let k = max_count as f64 / chan.count as f64;
            let max_x = hist.max as f64;
            cr.set_source_rgba(r, g, b, a);
            cr.set_line_width(2.0);
            let div = usize::max(hist.max as usize / width as usize, 1);
            cr.move_to(left_margin, top_margin + area_height);
            for (id, chunk) in chan.freq.chunks(div).enumerate() {
                let idx = id * div + chunk.len() / 2;
                let max_v = *chunk.iter().max().unwrap_or(&0);
                let mut max_v_f = k * max_v as f64;
                if log_y && max_v_f != 0.0 {
                    max_v_f = f64::log10(max_v_f);
                }
                let x = area_width * idx as f64 / max_x;
                let y = area_height - area_height * max_v_f / total_max_v;
                cr.line_to(x + left_margin, y + top_margin);
            }
            cr.line_to(left_margin + area_width, top_margin + area_height);
            cr.close_path();
            cr.fill_preserve()?;
            cr.stroke()?;
            Ok(())
        };

        paint_channel(&hist.r, 1.0, 0.0, 0.0, 1.0)?;
        paint_channel(&hist.g, 0.0, 2.0, 0.0, 0.5)?;
        paint_channel(&hist.b, 0.0, 0.0, 3.3, 0.33)?;
        paint_channel(&hist.l, 0.5, 0.5, 0.5, 1.0)?;
    }

    cr.set_line_width(1.0);
    cr.set_source_rgb(fg.red(), fg.green(), fg.blue());
    cr.move_to(0.0, top_margin+max_y_text_te.height());
    cr.show_text(p100)?;
    cr.move_to(0.0, height as f64 - bottom_margin);
    cr.show_text(p0)?;

    let paint_x_percent = |x, text| -> anyhow::Result<()> {
        let te = cr.text_extents(text)?;
        let mut tx = x-te.width()/2.0;
        if tx + te.width() > width as f64 {
            tx = width as f64 - te.width();
        }

        cr.move_to(x, top_margin+area_height-3.0);
        cr.line_to(x, top_margin+area_height+3.0);
        cr.stroke()?;

        cr.move_to(tx, top_margin+area_height-te.y_bearing()+3.0);
        cr.show_text(text)?;
        Ok(())
    };

    paint_x_percent(left_margin, p0)?;
    paint_x_percent(left_margin+area_width*0.25, p25)?;
    paint_x_percent(left_margin+area_width*0.50, p50)?;
    paint_x_percent(left_margin+area_width*0.75, p75)?;
    paint_x_percent(left_margin+area_width, p100)?;

    Ok(())
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
    let area_bg = area
        .style_context()
        .lookup_color("theme_base_color")
        .unwrap_or(gtk::gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
    cr.set_source_rgb(area_bg.red(), area_bg.green(), area_bg.blue());
    cr.rectangle(width * progress, 0.0, width * (1.0 - progress), height);
    cr.fill()?;

    cr.set_font_size(height);
    let te = cr.text_extents(text)?;

    if !text.is_empty() {
        cr.set_source_rgba(fg.red(), fg.green(), fg.blue(), 0.45);
        cr.rectangle(0.0, 0.0, width, height);
        cr.stroke()?;
    }

    cr.set_source_rgb(fg.red(), fg.green(), fg.blue());
    cr.move_to((width - te.width()) / 2.0, (height - te.height()) / 2.0 - te.y_bearing());
    cr.show_text(text)?;

    Ok(())
}

pub fn fill_devices_list_into_combobox(
    list:       &Vec<String>,
    cb:         &gtk::ComboBoxText,
    cur_id:     Option<&str>,
    connected:  bool,
    set_id_fun: impl Fn(&str)
) -> bool {
    cb.remove_all();

    for item in list {
        cb.append(Some(item), item);
    }

    let mut device_selected_in_cb = false;
    if let Some(cur_id) = cur_id {
        cb.set_active_id(Some(cur_id));
        if cb.active().is_none() {
            cb.insert(0, Some(&cur_id), cur_id);
            cb.set_active(Some(0));
            device_selected_in_cb = true;
        }
    } else if list.len() != 0 {
        cb.set_active(Some(0));
        set_id_fun(list[0].as_str());
        device_selected_in_cb = true;
    }

    cb.set_sensitive(list.len() != 0 && connected);

    device_selected_in_cb
}