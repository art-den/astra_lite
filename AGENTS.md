## About

- This is software for deepsky astrophotography and live stacking on low power PCs (like Raspberry Pi or Orange Pi). It also works on PCs. More information is in `README.md`.
- Written in Rust. Sources is in `src` folder, procedural macro is in `macros` folder. 
- Uses gtk3 via `gtk3-rs` crate for UI.

## Abbreviations and acronyms

- FITS - Flexible Image Transport System format. Used to store/transfer astronomical data.
- HDU (hdu) - Header Data Unit (The internal structure of a FITS file).
- flt - filter (wheel).
- calibr - Calibration
- sar (SAR) - Search and Replace (for hot pixels)
- recogn - Recognition (for stars on image)
- sens - Sensitivity
- mt = Multi Threading

## Architecture (Modules)

- `src/core` - System core: working modes, frame processing, events, camera control.
- `src/guiding` - API for external auto-guiding software (PHD2).
- `src/hal` - Hardware Abstraction Layer — INDI and ASCOM Alpaca for connecting telescopes, cameras, focusers.
- `src/image` - Image working: raw RAW, FITS, stacking, histograms, stars.
- `src/options` - Serializable settings (JSON) for all components.
- `src/plate_solve` - Common API for plate solving. Implementation for local Astrometry.net
- `src/sky_math` - Sky math: coordinates, Solar system.
- `src/ui` - GTK interface: device panels, preview, sky map, dialogs.
- `src/ui/debug` - UI automation debug mode (`--debug` flag): simulated clicks, screenshots.
- `src/ui/resources` - GTK ui-files, images
- `src/ui/sky_map` - Sky map widget
- `src/utils` - Utilities: IO, logging, math, timers, compression
- `tests/` - Integration tests
- `benches/` - Criterion benchmarks

## Panic

The program terminates on any panic. (`panic = "abort"` in `Cargo.toml`)

## Temporary files

Temporary files (debug screenshots, logs, scripts) must be saved in the project's `.tmp` folder.

## GIT usage

- One commit should contain only one task
- Commit comments should be as short as possible

## UI debug

To debug the UI, write the necessary code in `src/ui/debug/mod.rs` and use the `--debug` argument when running.
