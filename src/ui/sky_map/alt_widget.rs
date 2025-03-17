use std::f64::consts::PI;

use chrono::prelude::*;
use gtk::{cairo, gdk, prelude::*};

use crate::{ui::gtk_utils::*, utils::math::linear_interpolate};

use super::{data::*, math::*, solar_system::*};

pub fn paint_altitude_by_time(
    area:     &gtk::DrawingArea,
    cr:       &cairo::Context,
    dt:       NaiveDateTime,
    dt_local: NaiveDateTime,
    observer: &Observer,
    crd:      &Option<EqCoord>,
) -> anyhow::Result<()> {
    let (_, dpmm_y) = get_widget_dpmm(area)
        .unwrap_or((DEFAULT_DPMM, DEFAULT_DPMM));
    let sc = area.style_context();
    let fg_color = sc.color(gtk::StateFlags::NORMAL);
    let (fg_r, fg_g, fg_b) = (1.0, 1.0, 1.0);
    let bg_color = sc
        .lookup_color("theme_base_color")
        .unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));

    let width = area.allocated_width() as f64;
    let height = area.allocated_height() as f64;

    let font_desc = sc.font(gtk::StateFlags::ACTIVE);
    let pl = area.create_pango_layout(None);
    pl.set_font_description(Some(&font_desc));
    pl.set_text("#");
    let legend_height = 1.5 * pl.pixel_size().1 as f64;
    let legend_rect_size = 1.0 * pl.pixel_size().1 as f64;

    let day_color = (0.45, 0.45, 0.0);
    let twilight_color = (0.3, 0.3, 0.0);
    let night_color = (0.0, 0.0, 0.0);
    let moon_color = (0.02, 0.2, 0.2);

    // Legend

    cr.set_source_rgb(bg_color.red(), bg_color.green(), bg_color.blue());
    cr.rectangle(0.0, 0.0, width, legend_height);
    cr.fill()?;

    let mut x = 3.0;
    let mut draw_legend = |text, (r, g, b)| -> anyhow::Result<()> {
        cr.rectangle(x, 0.5 * (legend_height-legend_rect_size), legend_rect_size, legend_rect_size);
        cr.set_source_rgb(0.5, 0.5, 0.5);
        cr.stroke_preserve()?;
        cr.set_source_rgb(r, g, b);
        cr.fill()?;
        x += legend_rect_size * 1.2;

        pl.set_text(text);
        let text_height = pl.pixel_size().1 as f64;
        cr.move_to(x, 0.5 * legend_height - 0.5 * text_height);
        cr.set_source_rgb(fg_color.red(), fg_color.green(), fg_color.blue());
        pangocairo::show_layout(cr, &pl);
        x += pl.pixel_size().0 as f64 + 0.5 * legend_rect_size;

        Ok(())
    };

    draw_legend("Day", day_color)?;
    draw_legend("Twilight", twilight_color)?;
    draw_legend("Night", night_color)?;
    draw_legend("Moon", moon_color)?;

    let data_height = height - legend_height;

    const PAST_HOUR: i64 = -12;
    const FUTU_HOUR: i64 = 12;
    const STEPS: i64 = 4;

    // Background (with sun and moon)

    let sun_alt_theshold = degree_to_radian(-18.0);
    let sun_bg = gdk::cairo::LinearGradient::new(0.0, 0.0, width, 0.0);
    let mut max_moon_phase = None;
    for x in 0..=area.allocated_width() {
        let hour_diff = linear_interpolate(x as f64, 0.0, width, PAST_HOUR as f64, FUTU_HOUR as f64);
        let pt_diff = chrono::Duration::seconds((60.0 * 60.0 * hour_diff) as i64);
        let pt_time = dt.checked_add_signed(pt_diff).unwrap_or(dt);

        let cvt = EqToSphereCvt::new(observer.longitude, observer.latitude, &pt_time);

        let julian_centuries = calc_julian_centuries(&pt_time);
        let sun_crd = mini_sun(julian_centuries);
        let sun_h_crd = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&sun_crd));
        let moon_crd = mini_moon(julian_centuries);
        let moon_h_crd =  HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&moon_crd));

        let (r, g, b) = if sun_h_crd.alt < sun_alt_theshold {
            if moon_h_crd.alt > 0.0 {
                let phase = moon_phase(julian_centuries);
                max_moon_phase = max_moon_phase
                    .map(|v| f64::max(v, phase))
                    .or_else(|| Some(phase));
                moon_color
            } else {
                night_color
            }
        } else if sun_h_crd.alt < 0.0 {
            twilight_color
        } else {
            day_color
        };

        let offset = x as f64 / area.allocated_width() as f64;
        sun_bg.add_color_stop_rgb(offset, r, g, b);
    }

    cr.set_source(&sun_bg)?;
    cr.rectangle(0.0, legend_height, width, data_height);
    cr.fill()?;

    // Altitude plot

    let mut max_alt = None;
    let mut min_alt = None;
    if let Some(crd) = crd {
        for i in STEPS*PAST_HOUR..=STEPS*FUTU_HOUR {
            let hour_diff = chrono::Duration::minutes(60 * i / STEPS);
            let pt_time = dt.checked_add_signed(hour_diff).unwrap_or(dt);
            let cvt = EqToSphereCvt::new(observer.longitude, observer.latitude, &pt_time);
            let horiz_crd = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&crd));
            let julian_centuries = calc_julian_centuries(&pt_time);
            let sun_crd = mini_sun(julian_centuries);
            let sun_h_crd = HorizCoord::from_sphere_pt(&cvt.eq_to_sphere(&sun_crd));
            if sun_h_crd.alt < sun_alt_theshold {
                max_alt = max_alt
                    .map(|v| f64::max(v, horiz_crd.alt))
                    .or_else(|| Some(horiz_crd.alt));
                min_alt = min_alt
                    .map(|v| f64::min(v, horiz_crd.alt))
                    .or_else(|| Some(horiz_crd.alt));
            }
            let x = linear_interpolate(
                i as f64,
                (STEPS*PAST_HOUR) as f64,
                (STEPS*FUTU_HOUR) as f64,
                0.0,
                width
            );
            let y = linear_interpolate(horiz_crd.alt, 0.0, 0.5 * PI, height, legend_height);
            if i == STEPS*PAST_HOUR { cr.move_to(x, y); } else { cr.line_to(x, y); }
        }
    }

    cr.set_line_width(f64::max(0.5 * dpmm_y, 2.0));
    cr.set_source_rgba(0.0, 1.0, 0.0, 0.6);
    cr.stroke()?;

    // hours scale

    let mut prev_hour = 0;
    for x in 0..=area.allocated_width() {
        let hour_diff = linear_interpolate(x as f64, 0.0, width, PAST_HOUR as f64, FUTU_HOUR as f64);
        let pt_diff = chrono::Duration::seconds((60.0 * 60.0 * hour_diff) as i64);
        let pt_time = dt_local.checked_add_signed(pt_diff).unwrap_or(dt_local);
        let hour = pt_time.hour();
        if x != 0 && (hour / 3) != (prev_hour / 3) {
            cr.move_to(x as f64, legend_height);
            cr.line_to(x as f64, height);
            cr.set_line_width(1.0);
            cr.set_dash(&[2.0, 2.0], 1.0);
            cr.set_source_rgba(fg_r, fg_g, fg_b, 0.5);
            cr.stroke()?;

            let text = format!("{}h", hour);
            pl.set_text(&text);
            let (text_width, text_height) = pl.pixel_size();
            cr.move_to(
                x as f64 - 0.5 * text_width as f64,
                height - text_height as f64
            );
            cr.set_source_rgba(fg_r, fg_g, fg_b, 1.0);
            pangocairo::show_layout(cr, &pl);
        }
        prev_hour = hour;
    }

    // Text

    let mut text = String::new();
    if let (Some(max_alt), Some(min_alt)) = (max_alt, min_alt) {
        text += &format!(
            "Altutude: {:.1}..{:.1}°",
            radian_to_degree(min_alt),
            radian_to_degree(max_alt)
        );
    }
    if let Some(max_moon_phase) = max_moon_phase {
        text += &format!(" Moon phase = {:.0}%", 100.0 * max_moon_phase);
    }
    if !text.is_empty() {
        let mut smaller_font = font_desc.clone();
        smaller_font.set_size(8 * font_desc.size() / 10);
        pl.set_font_description(Some(&smaller_font));

        pl.set_text(&text);
        cr.move_to(3.0, legend_height);
        cr.set_source_rgba(fg_r, fg_g, fg_b, 1.0);
        pangocairo::show_layout(cr, &pl);
    }

    cr.rectangle(0.0, 0.0, width, height);
    cr.set_source_rgba(fg_r, fg_g, fg_b, 0.33);
    cr.set_line_width(f64::max(0.3 * dpmm_y, 1.0));
    cr.set_dash(&[], 0.0);
    cr.stroke()?;

    Ok(())
}