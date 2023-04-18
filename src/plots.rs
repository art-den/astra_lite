use std::f64::consts::PI;

use gtk::{prelude::*, gdk};

#[derive(PartialEq)]
pub enum PlotPointStyle {
    None,
    Round(f64),
    Rect(f64),
}

pub struct PlotLineStyle {
    pub line_width:  f64,
    pub line_color:  gdk::RGBA,
    pub point_style: PlotPointStyle,
}

impl Default for PlotLineStyle {
    fn default() -> Self {
        Self {
            line_width:  2.0,
            line_color:  gdk::RGBA::new(0.0, 0.0, 1.0, 1.0),
            point_style: PlotPointStyle::Round(8.0),
        }
    }
}

pub struct AxisStyle {
    pub dec_digits: usize,
    pub line_color: Option<gdk::RGBA>,
    pub text_color: Option<gdk::RGBA>,
    pub name:       Option<String>,
}

impl Default for AxisStyle {
    fn default() -> Self {
        Self {
            dec_digits: 1,
            line_color: None,
            text_color: None,
            name: None,
        }
    }
}

pub struct PlotAreaStyle {
    pub line_width: f64,
    pub line_color: Option<gdk::RGBA>,
    pub bg_color: Option<gdk::RGBA>,
}

impl Default for PlotAreaStyle {
    fn default() -> Self {
        Self {
            line_width: 1.0,
            line_color: None,
            bg_color: None
        }
    }
}

pub struct Plots<'a> {
    pub plot_count:          usize,
    pub get_plot_points_cnt: Box<dyn Fn(usize) -> usize + 'a>,
    pub get_plot_style:      Box<dyn Fn(usize) -> PlotLineStyle + 'a>,
    pub get_plot_point:      Box<dyn Fn(usize, usize) -> (f64, f64) + 'a>,
    pub area:                PlotAreaStyle,
    pub left_axis:           AxisStyle,
    pub bottom_axis:         AxisStyle,
}

pub fn draw_plots(
    plots: &Plots,
    da:    &gtk::DrawingArea,
    ctx:   &gtk::cairo::Context,
) -> anyhow::Result<()> {
    let range = calc_data_range(plots);
    let margin = 8.0;
    let area_rect =
        calc_plot_area(plots, margin, &range, da, ctx)?;
    let area_bg = plots.area.bg_color.unwrap_or_else(|| {
        da.style_context().lookup_color("theme_base_color")
            .unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0))
    });
    let def_fg = da.style_context().color(gtk::StateFlags::NORMAL);
    let area_line_color = plots.area.line_color.unwrap_or(def_fg);
    ctx.set_source_rgba(
        area_bg.red(),
        area_bg.green(),
        area_bg.blue(),
        area_bg.alpha()
    );
    ctx.rectangle(
        area_rect.left,
        area_rect.top,
        area_rect.right-area_rect.left,
        area_rect.bottom-area_rect.top
    );
    ctx.fill()?;
    ctx.set_source_rgba(
        area_line_color.red(),
        area_line_color.green(),
        area_line_color.blue(),
        area_line_color.alpha()
    );
    ctx.set_line_width(plots.area.line_width);
    ctx.rectangle(
        area_rect.left,
        area_rect.top,
        area_rect.right-area_rect.left,
        area_rect.bottom-area_rect.top
    );
    ctx.stroke()?;

    let Some(range) = calc_data_range(plots) else {
        return Ok(());
    };

    draw_left_axis(plots, margin, &range, &area_rect, ctx, &def_fg)?;
    draw_bottom_axis(plots, margin, &range, &area_rect, ctx, &def_fg)?;
    for plot_idx in 0..plots.plot_count {
        draw_plot_lines(plot_idx, plots, &range, &area_rect, ctx)?;
    }
    for plot_idx in 0..plots.plot_count {
        draw_plot_points(plot_idx, plots, &range, &area_rect, ctx)?;
    }
    return Ok(());
}

struct DataRange {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

fn calc_data_range(plots: &Plots) -> Option<DataRange> {
    let mut min_x = None;
    let mut max_x = None;
    let mut min_y = None;
    let mut max_y = None;
    for plot_idx in 0..plots.plot_count {
        let pts_count = (plots.get_plot_points_cnt)(plot_idx);
        for pt_idx in 0..pts_count {
            let (x, y) = (plots.get_plot_point)(plot_idx, pt_idx);
            match &mut min_x {
                Some(min_x) => if x < *min_x { *min_x = x; },
                None        => min_x = Some(x),
            }
            match &mut max_x {
                Some(max_x) => if x > *max_x { *max_x = x; },
                None        => max_x = Some(x),
            }
            match &mut min_y {
                Some(min_y) => if y < *min_y { *min_y = y; },
                None        => min_y = Some(y),
            }
            match &mut max_y {
                Some(max_y) => if y > *max_y { *max_y = y; },
                None        => max_y = Some(y),
            }
        }
    }
    if let (Some(min_x), Some(max_x), Some(min_y), Some(max_y)) = (min_x, max_x, min_y, max_y) {
        Some(DataRange {min_x, max_x, min_y, max_y})
    } else {
        None
    }
}

struct AreaRect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

fn calc_plot_area(
    plots:  &Plots,
    margin: f64,
    range:  &Option<DataRange>,
    da:     &gtk::DrawingArea,
    ctx:    &gtk::cairo::Context,
) -> anyhow::Result<AreaRect> {
    let Some(range) = range else {
        return Ok(AreaRect{
            left: margin,
            top: margin,
            right: da.allocation().width() as f64 - margin,
            bottom: da.allocation().height() as f64 - margin
        })
    };
    let fmt_width = plots.left_axis.dec_digits;
    let min_y_str = format!("{:.fmt_width$}", range.min_y);
    let max_y_str = format!("{:.fmt_width$}", range.max_y);
    let min_y_str_width = ctx.text_extents(&min_y_str)?.width();
    let max_y_str_width = ctx.text_extents(&max_y_str)?.width();
    let y_str_width = f64::max(min_y_str_width, max_y_str_width);
    let font_height = ctx.text_extents("0")?.height();
    let left = margin * 2.0 + y_str_width;
    let right = da.allocation().width() as f64 - margin;
    let top = margin;
    let bottom = da.allocation().height() as f64 - font_height - 2.0*margin;
    Ok(AreaRect {left, top, right, bottom})
}

fn draw_plot_lines(
    plot_idx:  usize,
    plots:     &Plots,
    range:     &DataRange,
    area_rect: &AreaRect,
    ctx:       &gtk::cairo::Context,
) -> anyhow::Result<()> {
    let points_count = (plots.get_plot_points_cnt)(plot_idx);
    if points_count < 2 {
        return Ok(());
    }
    let style = (plots.get_plot_style)(plot_idx);
    ctx.set_line_width(style.line_width);
    let (x, y) = (plots.get_plot_point)(plot_idx, 0);
    let (sx, sy) = calc_xy(x, y, range, area_rect);
    ctx.move_to(sx, sy);
    for pt_idx in 1..points_count {
        let (x, y) = (plots.get_plot_point)(plot_idx, pt_idx);
        let (sx, sy) = calc_xy(x, y, range, area_rect);
        ctx.line_to(sx, sy);
    }
    ctx.set_source_rgba(
        style.line_color.red(),
        style.line_color.green(),
        style.line_color.blue(),
        style.line_color.alpha()
    );
    ctx.stroke()?;
    Ok(())
}

fn draw_plot_points(
    plot_idx:  usize,
    plots:     &Plots,
    range:     &DataRange,
    area_rect: &AreaRect,
    ctx:       &gtk::cairo::Context,
) -> anyhow::Result<()> {
    let points_count = (plots.get_plot_points_cnt)(plot_idx);
    if points_count == 0 {
        return Ok(());
    }
    let style = (plots.get_plot_style)(plot_idx);
    if style.point_style == PlotPointStyle::None {
        return Ok(());
    }
    ctx.set_line_width(0.0);
    ctx.set_source_rgba(
        style.line_color.red(),
        style.line_color.green(),
        style.line_color.blue(),
        style.line_color.alpha()
    );
    for pt_idx in 0..points_count {
        let (x, y) = (plots.get_plot_point)(plot_idx, pt_idx);
        let (sx, sy) = calc_xy(x, y, range, area_rect);
        match &style.point_style {
            &PlotPointStyle::Round(diam) =>
                ctx.arc(sx, sy, diam/2.0, 0.0, 2.0 * PI),
            &PlotPointStyle::Rect(size) => {
                let size2 = 0.5 * size;
                ctx.rectangle(sx-size2, sy-size2, size, size);
            },
            _ => {},
        }
        ctx.fill()?;
    }
    Ok(())
}

fn draw_left_axis(
    plots:     &Plots,
    margin:    f64,
    range:     &DataRange,
    area_rect: &AreaRect,
    ctx:       &gtk::cairo::Context,
    def_fg:    &gtk::gdk::RGBA,
) -> anyhow::Result<()> {
    let font_height = ctx.text_extents("0")?.height();
    let max_y_cnt = f64::ceil((area_rect.bottom - area_rect.top) / (3.0 * font_height));
    let y_range = range.max_y - range.min_y;
    let y_div = calc_div(y_range, max_y_cnt);
    let line_color = plots.left_axis.line_color.unwrap_or_else(|| {
        let mut color = def_fg.clone();
        color.set_alpha(0.25);
        color
    });
    let text_color = plots.left_axis.text_color.unwrap_or(*def_fg);
    ctx.set_line_width(1.0);
    let y_start_idx = (range.min_y / y_div).round() as i64 - 1;
    let y_end_idx = (range.max_y / y_div).round() as i64 + 1;
    let min_max_eq = range.min_y == range.max_y;
    for y_idx in y_start_idx..=y_end_idx {
        let y = y_idx as f64 * y_div;
        if !min_max_eq && (y > range.max_y || y < range.min_y) {
            continue;
        }
        let y_crd = calc_y(y, range, area_rect);
        ctx.set_source_rgba(
            line_color.red(),
            line_color.green(),
            line_color.blue(),
            line_color.alpha()
        );
        ctx.move_to(area_rect.left - margin*0.5, y_crd);
        ctx.line_to(area_rect.right, y_crd);
        ctx.stroke()?;

        ctx.set_source_rgba(
            text_color.red(),
            text_color.green(),
            text_color.blue(),
            text_color.alpha()
        );
        let dec_digits = plots.left_axis.dec_digits;
        let text = format!("{:.dec_digits$}", y);
        let extents = ctx.text_extents(&text)?;
        ctx.move_to(
            area_rect.left - extents.width() - margin,
            y_crd + extents.y_advance() + extents.height() / 2.0
        );
        ctx.show_text(&text)?;
        if min_max_eq {
            break;
        }
    }
    Ok(())
}

fn draw_bottom_axis(
    plots:     &Plots,
    margin:    f64,
    range:     &DataRange,
    area_rect: &AreaRect,
    ctx:       &gtk::cairo::Context,
    def_fg:    &gtk::gdk::RGBA,
) -> anyhow::Result<()> {
    let dec_digits = plots.bottom_axis.dec_digits;
    let sample_text = format!("{:.dec_digits$}", range.max_x);
    let text_width = ctx.text_extents(&sample_text)?.width();
    let max_x_cnt = f64::ceil((area_rect.right - area_rect.left) / (2.0 * text_width));
    let x_range = range.max_x - range.min_x;
    let x_div = calc_div(x_range, max_x_cnt);
    let line_color = plots.bottom_axis.line_color.unwrap_or_else(|| {
        let mut color = def_fg.clone();
        color.set_alpha(0.25);
        color
    });
    let text_color = plots.bottom_axis.text_color.unwrap_or(*def_fg);
    ctx.set_line_width(1.0);
    let x_start_idx = (range.min_x / x_div).round() as i64 - 1;
    let x_end_idx = (range.max_x / x_div).round() as i64 + 1;
    let min_max_eq = range.min_x == range.max_x;
    for x_idx in x_start_idx..=x_end_idx {
        let x = x_idx as f64 * x_div;
        if !min_max_eq && (x > range.max_x || x < range.min_x) {
            continue;
        }
        let x_crd = calc_x(x, range, area_rect);
        ctx.set_source_rgba(
            line_color.red(),
            line_color.green(),
            line_color.blue(),
            line_color.alpha()
        );
        ctx.move_to(x_crd, area_rect.top);
        ctx.line_to(x_crd, area_rect.bottom + margin * 0.5);
        ctx.stroke()?;
        ctx.set_source_rgba(
            text_color.red(),
            text_color.green(),
            text_color.blue(),
            text_color.alpha()
        );
        let text = format!("{:.dec_digits$}", x);
        let extents = ctx.text_extents(&text)?;
        ctx.move_to(
            x_crd - extents.width() * 0.5,
            area_rect.bottom - extents.y_bearing() + margin
        );
        ctx.show_text(&text)?;
        if min_max_eq {
            break;
        }
    }
    Ok(())
}

fn calc_div(range: f64, max_cnt: f64) -> f64 {
    if range == 0.0 { return 1.0; }
    let div_set = [ 1.0, 0.5, 0.4, 0.25, 0.2 ];
    let mut mul = f64::powf(10_f64, range.log10().round()) / 10.0;
    loop {
        for &div in div_set.iter().rev() {
            let value = mul * div;
            if range / value < max_cnt {
                return value;
            }
        }
        mul *= 10.0;
    }
}

fn calc_x(x: f64, range: &DataRange, area_rect: &AreaRect) -> f64 {
    if range.min_x != range.max_x {
        linear_interpol(x, range.min_x, range.max_x, area_rect.left, area_rect.right)
    } else {
        0.5 * (area_rect.left + area_rect.right)
    }
}

fn calc_y(y: f64, range: &DataRange, area_rect: &AreaRect) -> f64 {
    if range.min_y != range.max_y {
        linear_interpol(y, range.min_y, range.max_y, area_rect.bottom, area_rect.top)
    } else {
        0.5 * (area_rect.bottom + area_rect.top)
    }
}

fn calc_xy(x: f64, y: f64, range: &DataRange, area_rect: &AreaRect) -> (f64, f64) {
    let x_crd = calc_x(x, range, area_rect);
    let y_crd = calc_y(y, range, area_rect);
    (x_crd, y_crd)
}

#[inline(always)]
fn linear_interpol(x: f64, x1: f64, x2: f64, y1: f64, y2: f64) -> f64 {
    (x - x1) * (y2 - y1) / (x2 - x1) + y1
}
