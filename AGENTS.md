## About
- This is software for deepsky astrophotography and live stacking on low power PCs (like rapsberry Pi or Orange Pi). It also works on PCs. More information is in README.md.
- Written in Rust. Sources is in `src` folder, procedural macro is in `macros` folder.

## Architecture (Modules)
- `src/core` - System core: working modes, frame processing, events, camera control.
- `src/guiding` - API for external auto-guiding software (PHD2).
- `src/hal` - Hardware Abstraction Layer — INDI and ASCOM Alpaca for connecting telescopes, cameras, focusers.
- `src/image` - Image working: raw RAW, FITS, stacking, histograms, stars.
- `src/options` - Serializable settings (JSON) for all components.
- `src/plate_solve` - Common API fpr platesolving. Implemetation for local Astrometry.net
- `src/sky_math` - Sky math: coordinates, Solar system.
- `src/ui` - GTK interface: device panels, preview, sky map, dialogs.
- `src/ui/resources` - GTK ui-files, images
- `src/ui/sky_map` - Sky map widget
- `src/utils` - Utilities: IO, logging, math, timers, compression
