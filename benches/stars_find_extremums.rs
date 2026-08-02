use astra_lite::image::{
    image::ImageLayer,
    stars::StarsFinder,
};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rand::{RngExt, SeedableRng};
use rand::rngs::StdRng;

const IMG_WIDTH: usize = 2048;
const IMG_HEIGHT: usize = 2048;
const STAR_COUNT: usize = 500;
const BACKGROUND_LEVEL: u16 = 100;

/// Generates a synthetic image with Gaussian star profiles on a uniform background,
/// mirroring the approach used in `tests/full_frame_processing.rs`.
fn generate_star_image(seed: u64) -> ImageLayer<u16> {
    let mut rng = StdRng::seed_from_u64(seed);

    let mut data = vec![BACKGROUND_LEVEL; IMG_WIDTH * IMG_HEIGHT];

    const MARGIN: usize = 32;

    for _ in 0..STAR_COUNT {
        let cx = rng.random_range(MARGIN..IMG_WIDTH - MARGIN);
        let cy = rng.random_range(MARGIN..IMG_HEIGHT - MARGIN);
        let peak = (4500 + rng.random_range(0..2000)) as f64;
        let sigma = 1.0_f64;
        let radius = 3;

        for dy in 0..=radius {
            for dx in 0..=radius {
                let dist_sq = (dx * dx + dy * dy) as f64;
                let value = peak * (-dist_sq / (2.0 * sigma * sigma)).exp();
                let v = value as u16;

                let offsets = [
                    (cx as isize - dx as isize, cy as isize - dy as isize),
                    (cx as isize + dx as isize, cy as isize - dy as isize),
                    (cx as isize - dx as isize, cy as isize + dy as isize),
                    (cx as isize + dx as isize, cy as isize + dy as isize),
                ];
                for (x_isize, y_isize) in offsets {
                    if x_isize >= 0 && y_isize >= 0 {
                        let x = x_isize as usize;
                        let y = y_isize as usize;
                        if x < IMG_WIDTH && y < IMG_HEIGHT {
                            let idx = y * IMG_WIDTH + x;
                            data[idx] = data[idx].saturating_add(v);
                        }
                    }
                }
            }
        }
    }

    ImageLayer::new_mono(data, IMG_WIDTH, IMG_HEIGHT)
}

fn bench_find_extremums(c: &mut Criterion) {
    let image = generate_star_image(42);
    let mt = true;

    c.bench_function("StarsFinder::find_extremums", |b| b.iter(|| {
        let mut threshold = 25_u16;
        let result = StarsFinder::find_extremums(
            black_box(&image),
            black_box(&mut threshold),
            mt,
        );
        black_box(&result);
    }));
}

criterion_group!(benches, bench_find_extremums);
criterion_main!(benches);
