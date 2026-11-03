Note for those editing this file: The file should contain only general information, no details!

## About

- This is software for deepsky astrophotography and live stacking on low power PCs (like Raspberry Pi or Orange Pi). It also works on PCs. More information is in `README.md`.
- Written in Rust (edition 2024).
- Uses gtk3 via `gtk3-rs` crate for UI (custom drawing via Cairo/Pango).
- Supported OS: Linux and MS Windows (Windows build requires the `*-gnu` toolchain, see `README.md`).
- Cargo workspace: root package is the app (library + thin binary); `crates/ascom`, `crates/indi` and `crates/starsolver` are members. Default members are the root package and `crates/starsolver` (`ascom` is Windows-only). `macros/` is a proc-macro crate used as a path dependency (not listed in `members`, so it is built only as a dependency of the app).

## Abbreviations and acronyms

- FITS - Flexible Image Transport System format. Used to store/transfer astronomical data.
- HDU (hdu) - Header Data Unit (The internal structure of a FITS file).
- flt - filter (wheel).
- calibr - Calibration
- sar (SAR) - Search and Remove (for hot pixels)
- recogn - Recognition (for stars on image)
- sens - Sensitivity
- mt = Multi Threading
- mnt - mount (equatorial telescope mount)
- hw - hardware
- wb - White Balance
- dso - Deep Sky Object
- dpmm - dots per millimeter (widget scale, from the monitor's physical size)
- xyls - astrometry.net star-list FITS file (input format of `crates/starsolver`)

## Architecture (Modules)

- `src/core` - System core: engine, working modes, frame processing, events, camera control, guiding by main camera, live stacking, raw calibration.
- `src/guiding` - API for external auto-guiding software (PHD2).
- `src/hal` - Hardware Abstraction Layer for connecting cameras, telescopes (mounts), focusers, filter wheels (interfaces and implementations):
  - "INDI" (Instrument-Neutral-Device-Interface) - all platforms
  - "ASCOM" (Astronomy Common Object Model) - Windows only
  - "ASCOM Alpaca" (ASCOM over HTTP) - Windows only
- `src/image` - Image working: RAW, FITS, stacking, histograms, stars, camera database.
- `src/options` - Serializable settings (JSON) for all components.
- `src/plate_solve` - Common API for plate solving: spawns an external solver process (`solve-field`, or `starsolver` if it is placed next to the app binary).
- `src/sky_math` - Sky math: coordinates, Solar system.
- `src/ui` - GTK interface: device panels, preview, sky map, dialogs. UI layout is in `.ui` files, widget structs are filled by `#[derive(FromBuilder)]`: its field names must match the object IDs in the `.ui` file, which is checked at runtime, not at compile time.
- `src/ui/debug` - UI automation debug mode (`--debug` flag): simulated clicks, screenshots.
- `src/ui/resources` - GTK ui-files, images
- `src/ui/sky_map` - Sky map widget
- `src/utils` - Utilities: IO, logging, math, timers, compression
- `macros/` - proc-macro crate (`FromBuilder` derive for GTK widget structs)
- `crates/ascom` - ASCOM Classic (COM, late binding) API crate. Windows-only
- `crates/indi` - INDI API crate
- `crates/starsolver` - Plate solver binary (spawned as an external process by `src/plate_solve`). When editing it, follow its own `crates/starsolver/AGENTS.md`.
- `map_data/` - Data files for sky map: star catalog (binary), DSO/named-star CSVs, constellation GeoJSON
- `tests/` - Integration tests
- `benches/` - Criterion benchmarks
- `scripts/` - Packaging scripts (create .deb package)
- `mswin_stuff/` - Windows icon and resources
- `docs/` - Screenshots for README

## Build and test

- `cargo check` / `cargo build` builds the default members (the app + `crates/starsolver`).
- Unit tests need no hardware: `cargo test --lib -p astra_lite`.
- Integration tests in `tests/` drive the whole device stack through a local server: `indiserver` with simulator drivers on Linux, ASCOM Alpaca drivers on Windows (a few use ASCOM Classic COM drivers, one needs no hardware at all). They are slow and serialized by a cross-process lock (`tests/common`).
- `crates/ascom` is not a default member (on Windows it is still built as a dependency of the app): test it with `-p ascom` on Windows only (live tests need ASCOM drivers, see `crates/ascom/README.md`).
- Windows-only code (`src/hal/hal_ascom*`, `crates/ascom`, `cfg(windows)` blocks) is not compiled on Linux, so `cargo check` gives no diagnostics for it.
- Cross-compilation for arm64/armhf/x86_64 with `cross` (`Cross.toml`): `build_cross.sh` puts `.deb` packages into `dist/`.

## Panic

The program terminates on any panic. (`panic = "abort"` in `Cargo.toml`)
In a release build a panic hook writes the error to a temp file and spawns a separate instance of the app (`--show-panic-dialog <file>`) to show the crash dialog.

## Temporary files

Temporary files (debug screenshots, logs, scripts) must be saved in the project's `.tmp` folder.

## GIT usage

- One commit should contain only one task
- Commit comments should be as short as possible

## UI debug

To debug the UI, write the necessary code in `src/ui/debug/mod.rs` and use the `--debug` argument when running.

## Misc.

- Raw data calibration (dark/bias, flat, hot-pixel SAR) is applied in the frame processing pipeline, which feeds previews and live stacking. Raw frames are saved to a FITS file in the same unaltered form as they came from the CCD.
