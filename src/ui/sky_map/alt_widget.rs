use std::f64::consts::PI;

use chrono::prelude::*;
use gtk::{cairo, gdk, prelude::*};

use crate::{ui::gtk_utils::{self, font_size_to_pixels, FontSize, DEFAULT_DPMM}, utils::math::linear_interpolate};

use super::{data::*, utils::*, solar_system::*};

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

    let font_size_pt = 8.0;
    let font_size_px = font_size_to_pixels(FontSize::Pt(font_size_pt), dpmm_y);

    let width = area.allocated_width() as f64;
    let height = area.allocated_height() as f64;

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
        let eq_hor_cvt = EqToHorizCvt::new(&observer, &pt_time);
        let julian_centuries = calc_julian_centuries(&pt_time);
        let sun_crd = mini_sun(julian_centuries);
        let sun_h_crd = eq_hor_cvt.eq_to_horiz(&sun_crd);
        let moon_crd = mini_moon(julian_centuries);
        let moon_h_crd = eq_hor_cvt.eq_to_horiz(&moon_crd);

        let (r, g, b) = if sun_h_crd.alt > sun_alt_theshold {
            let ratio = if sun_h_crd.alt >= 0.0 { 1.0 } else { 0.66 };
            (ratio * 0.4, ratio * 0.4, 0.0)
        } else if moon_h_crd.alt > 0.0 {
            let phase = moon_phase(julian_centuries);
            if let Some(max_moon_phase) = &mut max_moon_phase {
                if phase > *max_moon_phase {
                    *max_moon_phase = phase;
                }
            } else {
                max_moon_phase = Some(phase);
            }
            (0.02, 0.2, 0.2)
        } else {
            (0.0, 0.0, 0.0)
        };

        let offset = x as f64 / area.allocated_width() as f64;
        sun_bg.add_color_stop_rgb(offset, r, g, b);
    }

    cr.set_source(&sun_bg)?;
    cr.rectangle(0.0, 0.0, width, height);
    cr.fill()?;

    // Altitude plot

    let mut max_alt = None;
    let mut min_alt = None;
    if let Some(crd) = crd {
        for i in STEPS*PAST_HOUR..=STEPS*FUTU_HOUR {
            let hour_diff = chrono::Duration::minutes(60 * i / STEPS);
            let pt_time = dt.checked_add_signed(hour_diff).unwrap_or(dt);
            let eq_hor_cvt = EqToHorizCvt::new(&observer, &pt_time);
            let horiz_crd = eq_hor_cvt.eq_to_horiz(&crd);
            let julian_centuries = calc_julian_centuries(&pt_time);
            let sun_crd = mini_sun(julian_centuries);
            let sun_h_crd = eq_hor_cvt.eq_to_horiz(&sun_crd);
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
            let y = linear_interpolate(horiz_crd.alt, 0.0, 0.5 * PI, height, 0.0);
            if i == STEPS*PAST_HOUR { cr.move_to(x, y); } else { cr.line_to(x, y); }
        }
    }

    cr.set_line_width(f64::max(0.5 * dpmm_y, 2.0));
    cr.set_source_rgba(0.0, 1.0, 0.0, 0.6);
    cr.stroke()?;

    // hours scale
    cr.set_font_size(font_size_px);
    let mut prev_hour = 0;
    for x in 0..=area.allocated_width() {
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
        let te = cr.text_extents(&text)?;
        cr.move_to(3.0, te.height() + 0.25 * te.height());
        cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 1.0);
        cr.show_text(&text)?;
    }

    cr.rectangle(0.0, 0.0, width, height);
    cr.set_source_rgba(fg_color.red(), fg_color.green(), fg_color.blue(), 0.33);
    cr.set_line_width(f64::max(0.3 * dpmm_y, 1.0));
    cr.set_dash(&[], 0.0);
    cr.stroke()?;

    Ok(())
}