use std::f64::consts::PI;

use chrono::prelude::*;
use gtk::{cairo, gdk, prelude::*};

use crate::{ui::gtk_utils::{self, font_size_to_pixels, FontSize, DEFAULT_DPMM}, utils::math::linear_interpolate};

use super::{data::*, utils::*};

pub fn paint_altitude_by_time(
    area:     &gtk::DrawingArea,
    cr:       &cairo::Context,
    dt:       NaiveDateTime,
    dt_local: NaiveDateTime,
    observer: &Observer,
    crd:      &Option<EqCoord>,
) -> anyhow::Result<()> {
    let (_, dpmm_y) = gtk_utils::get_widget_dpmm(area)
        .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
    let sc = area.style_context();
    let fg_color = sc.color(gtk::StateFlags::NORMAL);
    let bg_color = sc.lookup_color("theme_base_color").unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));

    let font_size_pt = 8.0;
    let font_size_px = font_size_to_pixels(FontSize::Pt(font_size_pt), dpmm_y);

    let width = area.allocated_width() as f64;
    let height = area.allocated_height() as f64;

    cr.set_font_size(font_size_px);
    cr.set_source_rgb(bg_color.red(), bg_color.green(), bg_color.blue());
    cr.paint()?;

    const PAST_HOUR: i64 = -12;
    const FUTU_HOUR: i64 = 12;

    let mut max_alt = None;
    if let Some(crd) = crd {
        const STEPS: i64 = 4;
        let mut max_alt_v = f64::MIN;
        for i in STEPS*PAST_HOUR..=STEPS*FUTU_HOUR {
            let hour_diff = chrono::Duration::minutes(60 * i / STEPS);
            let pt_time = dt.checked_add_signed(hour_diff).unwrap_or(dt);
            let eq_hor_cvt = EqToHorizCvt::new(&observer, &pt_time);
            let horiz_crd = eq_hor_cvt.eq_to_horiz(&crd);
            if horiz_crd.alt > max_alt_v { max_alt_v = horiz_crd.alt; }
            let x = linear_interpolate(i as f64, (STEPS*PAST_HOUR) as f64, (STEPS*FUTU_HOUR) as f64, 0.0, width);
            let y = linear_interpolate(horiz_crd.alt, 0.0, 0.5 * PI, height, 0.0);
            if i == STEPS*PAST_HOUR { cr.move_to(x, y); } else { cr.line_to(x, y); }
        }
        max_alt = Some(radian_to_degree(max_alt_v));
    }

    cr.set_line_width(f64::max(0.5 * dpmm_y, 1.5));
    cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 0.6);
    cr.stroke()?;

    let mut prev_hour = 0;
    for x in 0..area.allocated_width() {
        let hour_diff = linear_interpolate(x as f64, 0.0, width, PAST_HOUR as f64, FUTU_HOUR as f64);
        let pt_diff = chrono::Duration::seconds((60.0 * 60.0 * hour_diff) as i64);
        let pt_time = dt_local.checked_add_signed(pt_diff).unwrap_or(dt_local);
        let hour = pt_time.hour();
        if x != 0 && (hour / 3) != (prev_hour / 3) {
            cr.move_to(x as f64, 0.0);
            cr.line_to(x as f64, height);
            cr.set_line_width(1.0);
            cr.set_dash(&[2.0, 2.0], 1.0);
            cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 0.5);
            cr.stroke()?;

            let text = format!("{}h", hour);
            let te = cr.text_extents(&text)?;
            cr.move_to(x as f64, height - 0.33 * te.height());
            cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 1.0);
            cr.show_text(&text)?;
        }
        prev_hour = hour;
    }

    if let Some(max_alt) = max_alt {
        let max_alt_text = format!("max.alt. = {:.1}°", max_alt);
        let te = cr.text_extents(&max_alt_text)?;
        cr.move_to(2.0, te.height() + 0.25 * te.height());
        cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 1.0);
        cr.show_text(&max_alt_text)?;
    }

    cr.rectangle(0.0, 0.0, width, height);
    cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 0.33);
    cr.set_line_width(f64::max(0.3 * dpmm_y, 1.0));
    cr.set_dash(&[], 0.0);
    cr.stroke()?;

    Ok(())
}