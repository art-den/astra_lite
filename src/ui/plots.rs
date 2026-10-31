#![allow(dead_code)]

use std::f64::consts::PI;

use gtk::{prelude::*, gdk, pango};

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
    /// Points with a non-finite coordinate are ignored.
    pub get_plot_point:      Box<dyn Fn(usize, usize) -> (f64, f64) + 'a>,
    pub area:                PlotAreaStyle,
    pub left_axis:           AxisStyle,
    pub bottom_axis:         AxisStyle,
}

/// Paint the plot area, the axes and the data series. Does nothing when there
/// are no plottable points or the widget leaves no room for the axis labels.
pub fn draw_plots(
    plots: &Plots,
    da:    &gtk::DrawingArea,
    ctx:   &gtk::cairo::Context,
) -> eyre::Result<()> {
    let margin = 8.0;
    let range = calc_data_range(plots);
    let area_rect =
        calc_plot_area(plots, margin, &range, da, ctx)?;
    if !area_rect.is_valid() {
        // No room for the axis labels; scaling axes against such a rect is meaningless.
        return Ok(());
    }
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

    let Some(range) = range else {
        return Ok(());
    };

    let sc = da.style_context();
    let font = sc.font(gtk::StateFlags::NORMAL);
    let layout = da.create_pango_layout(None);
    layout.set_font_description(Some(&font));

    draw_left_axis(plots, margin, &range, &area_rect, ctx, &layout, &def_fg)?;
    draw_bottom_axis(plots, margin, &range, &area_rect, ctx, &layout, &def_fg)?;
    for plot_idx in 0..plots.plot_count {
        draw_plot_lines(plot_idx, plots, &range, &area_rect, ctx)?;
    }
    for plot_idx in 0..plots.plot_count {
        draw_plot_points(plot_idx, plots, &range, &area_rect, ctx)?;
    }
    Ok(())
}

struct DataRange {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

fn is_plottable(x: f64, y: f64) -> bool {
    x.is_finite() && y.is_finite()
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
            if !is_plottable(x, y) {
                continue;
            }
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

impl AreaRect {
    fn is_valid(&self) -> bool {
        self.right > self.left && self.bottom > self.top
    }
}

fn placeholder_area_rect(margin: f64, da: &gtk::DrawingArea) -> AreaRect {
    AreaRect {
        left:   margin,
        top:    margin,
        right:  da.allocation().width() as f64 - margin,
        bottom: da.allocation().height() as f64 - margin,
    }
}

fn calc_plot_area(
    plots:  &Plots,
    margin: f64,
    range:  &Option<DataRange>,
    da:     &gtk::DrawingArea,
    ctx:    &gtk::cairo::Context,
) -> eyre::Result<AreaRect> {
    let Some(range) = range else {
        return Ok(placeholder_area_rect(margin, da));
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
    // May be degenerate on a tiny widget; `draw_plots()` bails out on it.
    Ok(AreaRect {left, top, right, bottom})
}

fn draw_plot_lines(
    plot_idx:  usize,
    plots:     &Plots,
    range:     &DataRange,
    area_rect: &AreaRect,
    ctx:       &gtk::cairo::Context,
) -> eyre::Result<()> {
    let points_count = (plots.get_plot_points_cnt)(plot_idx);
    if points_count < 2 {
        return Ok(());
    }
    let style = (plots.get_plot_style)(plot_idx);
    ctx.set_line_width(style.line_width);
    let mut started = false;
    for pt_idx in 0..points_count {
        let (x, y) = (plots.get_plot_point)(plot_idx, pt_idx);
        if !is_plottable(x, y) {
            continue;
        }
        let (sx, sy) = calc_xy(x, y, range, area_rect);
        // Skipped points leave a gap in the series, so the line bridges them.
        if started {
            ctx.line_to(sx, sy);
        } else {
            ctx.move_to(sx, sy);
            started = true;
        }
    }
    if !started {
        return Ok(());
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
) -> eyre::Result<()> {
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
        if !is_plottable(x, y) {
            continue;
        }
        let (sx, sy) = calc_xy(x, y, range, area_rect);
        match style.point_style {
            PlotPointStyle::Round(diam) =>
                ctx.arc(sx, sy, diam/2.0, 0.0, 2.0 * PI),
            PlotPointStyle::Rect(size) => {
                let size2 = 0.5 * size;
                ctx.rectangle(sx-size2, sy-size2, size, size);
            },
            _ => {},
        }
        ctx.fill()?;
    }
    Ok(())
}

/// First and last grid line index of an axis, padded by one tick on each side.
/// `calc_div()` returns a step proportional to the range, and the saturating
/// casts keep the span small on absurd data, so the loop cannot spin.
fn tick_indices(min: f64, max: f64, div: f64) -> (i64, i64) {
    (
        i64::saturating_add((min / div).round() as i64, -1),
        i64::saturating_add((max / div).round() as i64, 1),
    )
}

fn draw_left_axis(
    plots:     &Plots,
    margin:    f64,
    range:     &DataRange,
    area_rect: &AreaRect,
    ctx:       &gtk::cairo::Context,
    layout:    &pango::Layout,
    def_fg:    &gtk::gdk::RGBA,
) -> eyre::Result<()> {
    let font_height = ctx.text_extents("0")?.height();
    let max_y_cnt = f64::ceil((area_rect.bottom - area_rect.top) / (3.0 * font_height));
    let y_range = range.max_y - range.min_y;
    let y_div = calc_div(y_range, max_y_cnt);
    let line_color = plots.left_axis.line_color.unwrap_or_else(|| {
        let mut color = *def_fg;
        color.set_alpha(0.25);
        color
    });
    let text_color = plots.left_axis.text_color.unwrap_or(*def_fg);
    ctx.set_line_width(1.0);
    let (y_start_idx, y_end_idx) = tick_indices(range.min_y, range.max_y, y_div);
    let min_max_eq = range.min_y == range.max_y;
    for y_idx in y_start_idx..=y_end_idx {
        let y = y_idx as f64 * y_div;
        if !y.is_finite()
            || (!min_max_eq && (y > range.max_y || y < range.min_y))
        {
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
        layout.set_text(&text);
        let (width, height) = layout.pixel_size();
        ctx.move_to(
            area_rect.left - width as f64 - margin,
            y_crd - 0.5 * height as f64
        );
        pangocairo::show_layout(ctx, layout);
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
    layout:    &pango::Layout,
    def_fg:    &gtk::gdk::RGBA,
) -> eyre::Result<()> {
    let dec_digits = plots.bottom_axis.dec_digits;
    let sample_text = format!("{:.dec_digits$}", range.max_x);
    let text_width = ctx.text_extents(&sample_text)?.width();
    let max_x_cnt = f64::ceil((area_rect.right - area_rect.left) / (2.0 * text_width));
    let x_range = range.max_x - range.min_x;
    let x_div = calc_div(x_range, max_x_cnt);
    let line_color = plots.bottom_axis.line_color.unwrap_or_else(|| {
        let mut color = *def_fg;
        color.set_alpha(0.25);
        color
    });
    let text_color = plots.bottom_axis.text_color.unwrap_or(*def_fg);
    ctx.set_line_width(1.0);
    let (x_start_idx, x_end_idx) = tick_indices(range.min_x, range.max_x, x_div);
    let min_max_eq = range.min_x == range.max_x;
    for x_idx in x_start_idx..=x_end_idx {
        let x = x_idx as f64 * x_div;
        if !x.is_finite()
            || (!min_max_eq && (x > range.max_x || x < range.min_x))
        {
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
        layout.set_text(&text);
        let (width, _) = layout.pixel_size();
        ctx.move_to(
            x_crd - width as f64 * 0.5,
            area_rect.bottom + margin
        );
        pangocairo::show_layout(ctx, layout);

        if min_max_eq {
            break;
        }
    }
    Ok(())
}

fn calc_div(range: f64, max_cnt: f64) -> f64 {
    // Flat data: the ladder below cannot handle a zero range, and any step
    // yields the single tick the axis needs.
    if range == 0.0 { return 1.0; }
    // The loop exits only on `range / value < max_cnt`, always false for a
    // non-positive/NaN `max_cnt` or a non-finite/negative `range`. Returning
    // `range` keeps the step proportional; a fixed `1.0` would not.
    if max_cnt <= 0.0 || max_cnt.is_nan() || !range.is_finite() || range < 0.0 {
        return range;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn plots_from(points: Vec<(f64, f64)>) -> Plots<'static> {
        let points = Rc::new(points);
        let cnt_points = Rc::clone(&points);
        let get_points = Rc::clone(&points);
        Plots {
            plot_count: 1,
            get_plot_points_cnt: Box::new(move |_| cnt_points.len()),
            get_plot_style: Box::new(|_| PlotLineStyle::default()),
            get_plot_point: Box::new(move |_, pt_idx| get_points[pt_idx]),
            area: PlotAreaStyle::default(),
            left_axis: AxisStyle::default(),
            bottom_axis: AxisStyle::default(),
        }
    }

    /// Number of grid lines the axis loops would walk, through the production
    /// index math.
    fn tick_cnt(min: f64, max: f64, max_cnt: f64) -> i64 {
        let div = calc_div(max - min, max_cnt);
        let (start, end) = tick_indices(min, max, div);
        i64::max(i64::saturating_add(i64::saturating_sub(end, start), 1), 0)
    }

    #[test]
    fn calc_div_selects_nice_step() {
        assert_eq!(calc_div(0.0, 10.0), 1.0);
        assert_eq!(calc_div(5.0, 10.0), 1.0);
        assert_eq!(calc_div(10.0, 10.0), 2.0);
        assert_eq!(calc_div(250.0, 5.0), 100.0);
    }

    #[test]
    fn calc_div_step_respects_max_cnt() {
        for &range in &[0.7f64, 1.0, 5.0, 10.0, 250.0, 1e12] {
            for &max_cnt in &[1.0f64, 4.0, 10.0, 50.0] {
                let div = calc_div(range, max_cnt);
                assert!(
                    range / div < max_cnt,
                    "range={range} max_cnt={max_cnt} div={div}"
                );
            }
        }
    }

    #[test]
    fn calc_div_bails_out_on_bad_max_cnt() {
        // Without the entry guard these spin in `loop` forever (100% CPU in the
        // GTK draw callback). The step must stay proportional to `range`, else
        // the grid line loop still gets huge: `1e12` with a `1.0` step.
        for &max_cnt in &[0.0f64, -0.0, -3.0, f64::NAN] {
            assert_eq!(calc_div(5.0, max_cnt), 5.0, "max_cnt={max_cnt}");
            assert!(tick_cnt(0.0, 1e12, max_cnt) <= 4, "max_cnt={max_cnt}");
        }
    }

    #[test]
    fn calc_div_bails_out_on_bad_range() {
        assert!(calc_div(f64::NAN, 10.0).is_nan());
        assert_eq!(calc_div(f64::INFINITY, 10.0), f64::INFINITY);
        assert_eq!(calc_div(-5.0, 10.0), -5.0);
        assert!(tick_cnt(0.0, f64::INFINITY, 10.0) < 10);
        assert!(tick_cnt(f64::NAN, 5.0, 10.0) < 10);
        assert!(tick_cnt(5.0, -5.0, 10.0) < 10);
    }

    #[test]
    fn calc_div_handles_infinite_max_cnt() {
        // `font_height == 0.0` makes `max_cnt` `ceil(h / 0.0)` = inf; the ladder
        // must accept its smallest candidate instead of spinning.
        let div = calc_div(10.0, f64::INFINITY);
        assert!(div.is_finite() && div > 0.0, "div={div}");
        assert!(tick_cnt(0.0, 10.0, f64::INFINITY) < 200);
    }

    #[test]
    fn tick_cnt_stays_bounded_on_any_data() {
        // The grid line loops must never spin, whatever the data or `max_cnt`.
        // The ladder never yields more than ~160 ticks per decade.
        for &min in &[-1e308f64, -1e12, -1.0, 0.0, 1e-320, 1e21, 1e308] {
            for &max in &[-1e308f64, 0.0, 1.0, 1e12, 1e21, 1e308] {
                for &max_cnt in &[0.0f64, -3.0, 1.0, 10.0, 5000.0, f64::NAN, f64::INFINITY] {
                    let cnt = tick_cnt(min, max, max_cnt);
                    assert!(cnt <= 200, "min={min} max={max} max_cnt={max_cnt} cnt={cnt}");
                }
            }
        }
    }

    #[test]
    fn calc_data_range_skips_non_finite_points() {
        // A NaN first point used to poison the whole range, which then hung
        // `calc_div()`; an infinite parabola point must do the same no more.
        let plots = plots_from(vec![
            (f64::NAN, f64::NAN),
            (0.0, 1.0),
            (2.0, 3.0),
            (f64::INFINITY, 2.0),
        ]);
        let range = calc_data_range(&plots).unwrap();
        assert_eq!((range.min_x, range.max_x), (0.0, 2.0));
        assert_eq!((range.min_y, range.max_y), (1.0, 3.0));
    }

    #[test]
    fn calc_data_range_none_without_finite_points() {
        let plots = plots_from(vec![(f64::NAN, 1.0), (1.0, f64::NAN)]);
        assert!(calc_data_range(&plots).is_none());
    }

    #[test]
    fn area_rect_requires_positive_size() {
        assert!(AreaRect { left: 1.0, top: 1.0, right: 2.0, bottom: 2.0 }.is_valid());
        assert!(!AreaRect { left: 1.0, top: 1.0, right: 1.0, bottom: 2.0 }.is_valid());
        assert!(!AreaRect { left: 1.0, top: 1.0, right: 2.0, bottom: 1.0 }.is_valid());
    }
}
