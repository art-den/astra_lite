//! Headless benchmark of `SkyMapPainter::paint`.
//! Paints into a cairo ImageSurface (no window needed) to measure frame cost.
//!
//! A scenario keeps every other layer on, so `--config dso` is not the ellipse
//! phase alone: it still paints the DSO names, the eq grid and the ground. To
//! isolate a phase, subtract two scenarios (e.g. `--config all` minus
//! `--config no_dso`).

use std::{rc::Rc, time::Instant};

use astra_lite::ui::sky_map::{
    data::{ItemsToShow, Observer, SkyMap},
    painter::{PaintRequest, PaintConfig, SkyMapPainter, ViewPoint},
    utils::{Rect, Screen},
};
use gtk::{cairo, pango};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const ITERATIONS: usize = 20;

fn arg(name: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)) {
        Some(value) => value.clone(),
        None => default.to_string(),
    }
}

fn make_screen() -> Screen {
    let width = WIDTH as f64;
    let height = HEIGHT as f64;
    let dpmm_x = 3.78; // ~96 dpi
    Screen {
        rect:      Rect { left: 0.0, top: 0.0, right: width, bottom: height },
        tolerance: Rect {
            left:   -20.0 * dpmm_x,
            top:    -20.0 * dpmm_x,
            right:  width + 20.0 * dpmm_x,
            bottom: height + 20.0 * dpmm_x,
        },
        main_size: 0.5 * f64::max(width, height),
        center_x:  0.5 * width,
        center_y:  0.5 * height,
        dpmm_x,
        font_size: 12.0,
    }
}

fn print_stats(map: &SkyMap) {
    let mut total_stars = 0_usize;
    let mut named_stars = 0_usize;
    let mut by_mag = [0_usize; 16];
    let mut max_zone_len = 0_usize;
    for zone in map.stars().zones().values() {
        total_stars += zone.stars().len();
        named_stars += zone.named_stars().len();
        max_zone_len = usize::max(max_zone_len, zone.stars().len());
        for star in zone.stars() {
            let m = f64::from(star.data.mag.get()).clamp(-1.0, 15.999);
            by_mag[m as usize] += 1;
        }
    }

    let mut const_polygons = 0_usize;
    let mut const_bounds_pts = 0_usize;
    let mut const_line_pts = 0_usize;
    let mut const_polylines = 0_usize;
    for c in map.constellations().values() {
        const_polygons += c.bounds.len();
        const_polylines += c.lines.len();
        for p in &c.bounds {
            const_bounds_pts += p.len();
        }
        for l in &c.lines {
            const_line_pts += l.len();
        }
    }

    println!("stars:          {total_stars} (named {named_stars}), biggest zone {max_zone_len}");
    println!("mag histogram:  {by_mag:?} (index = magnitude)");
    println!("dso objects:    {}", map.objects().len());
    println!(
        "constellations: {} polygons ({const_bounds_pts} pts), {const_polylines} polylines ({const_line_pts} pts)",
        const_polygons
    );
}

fn bench(
    name:       &str,
    painter:    &mut SkyMapPainter,
    map:        &Rc<SkyMap>,
    config:     &PaintConfig,
    mag_factor: f64,
    observer:   &Observer,
) {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, WIDTH, HEIGHT).unwrap();
    let cr = cairo::Context::new(&surface).unwrap();
    let layout = pangocairo::functions::create_layout(&cr);
    layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 10")));

    let screen = make_screen();
    let utc_time = chrono::Utc::now().naive_utc();
    let selection = None;
    let tele_pos = None;
    let cam_frame = None;
    let plate_solve = None;
    let sky_map = Some(Rc::clone(map));
    let mut vp = ViewPoint::new();
    vp.mag_factor = mag_factor;

    let paint = |painter: &mut SkyMapPainter| {
        painter.paint(PaintRequest {
            sky_map:     &sky_map,
            selection:   &selection,
            tele_pos:    &tele_pos,
            cam_frame:   &cam_frame,
            plate_solve: &plate_solve,
            observer,
            utc_time:    &utc_time,
            config,
            view_point:  &vp,
            screen:      &screen,
            cairo:       &cr,
            layout:      &layout,
        })
    };

    for _ in 0..3 {
        paint(painter).unwrap();
    }

    let mut best = f64::MAX;
    let mut worst = 0.0;
    let mut sum = 0.0;
    for _ in 0..ITERATIONS {
        let t = Instant::now();
        paint(painter).unwrap();
        let dt = t.elapsed().as_secs_f64() * 1000.0;
        best = f64::min(best, dt);
        worst = f64::max(worst, dt);
        sum += dt;
    }

    println!(
        "{name:<14} mag_factor={mag_factor:<6} min={best:7.2} ms  avg={:7.2} ms  max={worst:7.2} ms",
        sum / ITERATIONS as f64
    );
}

fn degree(deg: f64) -> f64 {
    std::f64::consts::PI * deg / 180.0
}

fn main() {
    let mut data_dir = std::path::Path::new("map_data").to_path_buf();
    if !data_dir.is_dir() {
        // Bench targets may be launched from outside the repo root
        data_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("map_data");
    }

    let mut t = Instant::now();
    let mut map = SkyMap::new();
    map.load_constellations(data_dir.join("constellations.bounds.geojson"), true).unwrap();
    map.load_constellations(data_dir.join("constellations.lines.geojson"), false).unwrap();
    map.load_dso(data_dir.join("dso.csv")).unwrap();
    map.load_named_stars(data_dir.join("named_stars.csv")).unwrap();
    println!("base map loaded in {:.2} s", t.elapsed().as_secs_f64());

    t = Instant::now();
    map.load_stars(data_dir.join("stars.bin")).unwrap();
    println!("stars loaded in {:.2} s", t.elapsed().as_secs_f64());
    let map = Rc::new(map);

    print_stats(&map);

    let observer = Observer {
        latitude:  degree(55.0),
        longitude: degree(37.0),
    };

    let mut painter = SkyMapPainter::new();

    let all = PaintConfig::default();

    // Single-scenario mode for profiling: --mag 1 --config all --iters 200
    let mag: f64 = arg("--mag", "").parse().unwrap_or(0.0);
    if mag > 0.0 {
        let config_name = arg("--config", "all");
        let mut config = PaintConfig::default();
        const DSO_FLAGS: ItemsToShow = ItemsToShow::from_bits_truncate(
            ItemsToShow::DSO.bits()
                | ItemsToShow::NEBULAS.bits()
                | ItemsToShow::CLUSTERS.bits()
                | ItemsToShow::GALAXIES.bits(),
        );
        match config_name.as_str() {
            "none"        => config.items_filter = ItemsToShow::empty(),
            "stars"       => config.items_filter = ItemsToShow::STARS,
            "consts"      => config.items_filter = ItemsToShow::CONSTS,
            "dso"         => config.items_filter = DSO_FLAGS,
            "no_stars"    => config.items_filter = ItemsToShow::all() - ItemsToShow::STARS,
            "no_consts"   => config.items_filter = ItemsToShow::all() - ItemsToShow::CONSTS,
            "no_dso"      => config.items_filter = ItemsToShow::all() - DSO_FLAGS,
            "low_quality" => config.high_quality = false,
            _ => {}
        }
        config.max_dso_mag = arg("--dso-mag", "10").parse().unwrap_or(10.0);
        config.eq_grid.visible = arg("--grid", "1") == "1";
        config.horizon_glow.visible = arg("--glow", "1") == "1";
        let iters: usize = arg("--iters", "200").parse().unwrap_or(200);
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, WIDTH, HEIGHT).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let layout = pangocairo::functions::create_layout(&cr);
        layout.set_font_description(Some(&pango::FontDescription::from_string("Sans 10")));
        let screen = make_screen();
        let utc_time = chrono::Utc::now().naive_utc();
        let selection = None;
        let tele_pos = None;
        let cam_frame = None;
        let plate_solve = None;
        let sky_map = Some(Rc::clone(&map));
        let mut vp = ViewPoint::new();
        vp.mag_factor = mag;
        let total_start = Instant::now();
        for _ in 0..iters {
            painter.paint(PaintRequest {
                sky_map: &sky_map, selection: &selection, tele_pos: &tele_pos,
                cam_frame: &cam_frame, plate_solve: &plate_solve, observer: &observer,
                utc_time: &utc_time, config: &config, view_point: &vp,
                screen: &screen, cairo: &cr, layout: &layout,
            }).unwrap();
        }
        let total_elapsed = total_start.elapsed();
        println!("{config_name:<12} mag={mag} avg={:.2} ms", total_elapsed.as_secs_f64() * 1000.0 / iters as f64);
        return;
    }
    println!("\nhigh_quality = {}", all.high_quality);

    let no_stars = PaintConfig {
        items_filter: ItemsToShow::DSO | ItemsToShow::CONSTS | ItemsToShow::NEBULAS
            | ItemsToShow::CLUSTERS | ItemsToShow::GALAXIES,
        ..PaintConfig::default()
    };

    let only_stars = PaintConfig { items_filter: ItemsToShow::STARS, ..PaintConfig::default() };

    for (name, config) in [("all", &all), ("no stars", &no_stars), ("only stars", &only_stars)] {
        println!("\n--- {name} ---");
        for mag_factor in [1.0, 3.0, 10.0, 30.0, 100.0] {
            bench(name, &mut painter, &map, config, mag_factor, &observer);
        }
    }
}
