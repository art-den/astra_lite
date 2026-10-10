Note for those editing this file: The file should contain only general information, no details!

## About

- This is software for deepsky astrophotography and live stacking on low power PCs (like Raspberry Pi or Orange Pi). It also works on PCs. More information is in `README.md`.
- Written in Rust (edition 2024).
- Uses gtk3 via `gtk3-rs` crate for UI.
- Cargo workspace: root package is the app; `crates/ascom` and `crates/indi` are members. Default member is only the root package (`ascom` is Windows-only).

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
- `src/hal` - Hardware Abstraction Layer for connecting telescopes, cameras, focusers (interfaces and implementations):
  - "INDI" (Instrument-Neutral-Device-Interface)
  - "ASCOM" (Astronomy Common Object Model)
  - "ASCOM Alpaca" (ASCOM over HTTP)
- `src/image` - Image working: raw RAW, FITS, stacking, histograms, stars.
- `src/options` - Serializable settings (JSON) for all components.
- `src/plate_solve` - Common API for plate solving. Implementation for local Astrometry.net
- `src/sky_math` - Sky math: coordinates, Solar system.
- `src/ui` - GTK interface: device panels, preview, sky map, dialogs.
- `src/ui/debug` - UI automation debug mode (`--debug` flag): simulated clicks, screenshots.
- `src/ui/resources` - GTK ui-files, images
- `src/ui/sky_map` - Sky map widget
- `src/utils` - Utilities: IO, logging, math, timers, compression
- `crates/ascom` - ASCOM Classic (COM, late binding) API crate. Windows-only
- `crates/indi` - INDI API crate
- `map_data/` - Data files for sky map: star catalog (binary), DSO/named-star CSVs, constellation GeoJSON
- `tests/` - Integration tests
- `benches/` - Criterion benchmarks
- `scripts/` - Packaging scripts (create .deb package)
- `docs/` - Screenshots for README

## Panic

The program terminates on any panic. (`panic = "abort"` in `Cargo.toml`)

## ASCOM Classic implementation

`IMPL_ASCOM.md` is the design/implementation doc for ASCOM Classic (COM) support in `src/hal/hal_ascom`. Key constraints:
- No Connect/Disconnect buttons for ASCOM in UI; installed drivers appear directly in existing device lists (selection == connection).
- Device switch: deactivate the previous device BEFORE activating the new one (the old driver's disconnect may cascade to the new device's shared underlying driver, e.g. hub drivers).
- HAL event handlers run synchronously on the sender's thread.

## Temporary files

Temporary files (debug screenshots, logs, scripts) must be saved in the project's `.tmp` folder.

## GIT usage

- One commit should contain only one task
- Commit comments should be as short as possible

## UI debug

To debug the UI, write the necessary code in `src/ui/debug/mod.rs` and use the `--debug` argument when running.
