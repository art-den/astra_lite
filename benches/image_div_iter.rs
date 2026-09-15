use astra_lite::image::image::{Image, ImageLayer};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rand::{RngExt, SeedableRng};
use rand::rngs::StdRng;

const IMG_WIDTH: usize = 4096;
const IMG_HEIGHT: usize = 4096;

fn generate_image(seed: u64) -> Image {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut random_pixels = || {
        (0..IMG_WIDTH * IMG_HEIGHT).map(|_| rng.random_range(0u16..4096)).collect()
    };
    let mut img = Image::new_empty();
    img.make_monochrome(IMG_WIDTH, IMG_HEIGHT, 0, 4095);
    img.l = ImageLayer::new_mono(random_pixels(), IMG_WIDTH, IMG_HEIGHT);
    img.r = ImageLayer::new_mono(random_pixels(), IMG_WIDTH, IMG_HEIGHT);
    img.g = ImageLayer::new_mono(random_pixels(), IMG_WIDTH, IMG_HEIGHT);
    img.b = ImageLayer::new_mono(random_pixels(), IMG_WIDTH, IMG_HEIGHT);
    img
}

// naive reference: bounds-checked index access, like the pre-pointer version
fn ref_div_n(layer: &ImageLayer<u16>, n: usize) -> Vec<u16> {
    let w = layer.width() / n;
    let h = layer.height() / n;
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in 0..n {
                let row = layer.row(y * n + dy);
                for dx in 0..n {
                    sum += row[x * n + dx] as u32;
                }
            }
            let k = (n * n) as u32;
            out.push(((sum + k / 2) / k) as u16);
        }
    }
    out
}

fn ref_div_n_rgb(image: &Image, n: usize) -> Vec<(u16, u16, u16)> {
    let w = image.r.width() / n;
    let h = image.r.height() / n;
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            for dy in 0..n {
                let r_row = image.r.row(y * n + dy);
                let g_row = image.g.row(y * n + dy);
                let b_row = image.b.row(y * n + dy);
                for dx in 0..n {
                    r += r_row[x * n + dx] as u32;
                    g += g_row[x * n + dx] as u32;
                    b += b_row[x * n + dx] as u32;
                }
            }
            let k = (n * n) as u32;
            out.push((
                ((r + k / 2) / k) as u16,
                ((g + k / 2) / k) as u16,
                ((b + k / 2) / k) as u16,
            ));
        }
    }
    out
}

fn bench_div_iter(c: &mut Criterion) {
    let img = generate_image(42);

    c.bench_function("l_iter_div2", |b| b.iter(|| black_box(img.iter_div2_l().collect::<Vec<u16>>())));
    c.bench_function("l_iter_div3", |b| b.iter(|| black_box(img.iter_div3_l().collect::<Vec<u16>>())));
    c.bench_function("l_iter_div4", |b| b.iter(|| black_box(img.iter_div4_l().collect::<Vec<u16>>())));

    c.bench_function("l_index_div2", |b| b.iter(|| black_box(ref_div_n(&img.l, 2))));
    c.bench_function("l_index_div3", |b| b.iter(|| black_box(ref_div_n(&img.l, 3))));
    c.bench_function("l_index_div4", |b| b.iter(|| black_box(ref_div_n(&img.l, 4))));

    c.bench_function("rgb_iter_div2", |b| b.iter(|| black_box(img.iter_div2_rgb().collect::<Vec<_>>())));
    c.bench_function("rgb_iter_div3", |b| b.iter(|| black_box(img.iter_div3_rgb().collect::<Vec<_>>())));
    c.bench_function("rgb_iter_div4", |b| b.iter(|| black_box(img.iter_div4_rgb().collect::<Vec<_>>())));

    c.bench_function("rgb_index_div2", |b| b.iter(|| black_box(ref_div_n_rgb(&img, 2))));
    c.bench_function("rgb_index_div3", |b| b.iter(|| black_box(ref_div_n_rgb(&img, 3))));
    c.bench_function("rgb_index_div4", |b| b.iter(|| black_box(ref_div_n_rgb(&img, 4))));
}

criterion_group!(benches, bench_div_iter);
criterion_main!(benches);
