# AstraLite
AstraLite is software for deepsky astrophotography and live stacking
on low power PCs (like rapsberry Pi or Orange Pi)

```diff
! The program is under active development !
```

AstraLite uses INDI server to work with astrophotography hardware.
See https://www.indilib.org/download.html to install INDI

Compiled binaries and discussion here:
https://www.indilib.org/forum/clients/13006-astralite-simple-indi-client-for-astrophotography.html

# Features
* UI for INDI devices control
* Live images preview
* Saving RAW frames
* Live stacking
* Dark and flat files creation
* Light frame quality filter
* Simple guiding by main camera
* Dithering
* Autofocus
* Sky map
* Manual mount control
* PHD2 support for dithering

![](./docs/screenshot1.jpg)

# Future plans
* INDI driver crash recovery
* Plate solving
* Sigma clipping for live staking (not sure this is possible with low memory usage)
* Live view from camera in video mode

# How to build AstraLite
## Prerequisites for Linux
* Rust compiler: https://www.rust-lang.org/tools/install
* Libs and tools:
```
sudo apt install gcc libgtk-3-dev build-essential
```

## Prerequisites for MS Windows
* Rust compiler (i686-pc-windows-**gnu**): https://www.rust-lang.org/tools/install
  Note! You have to install *-gnu (not *-msvc) toolchain:
```
rustup-init.exe --default-toolchain=stable-x86_64-pc-windows-gnu --default-host=x86_64-pc-windows-gnu
```
* MSYS: https://www.msys2.org/
* Libs and tools (type inside MSYS command line):
```
pacman -S mingw-w64-x86_64-gtk3
pacman -S mingw-w64-x86_64-pkg-config base-devel mingw-w64-x86_64-gcc
```

Don't forget to set your `PATH` environment variable to point to the `mingw64\bin` directory of MSYS

# How to build for you platform
To build optimized binaries for your current platform, just type
```
cargo build --release
```
# Building and creating deb-packages for arm64, armhf and x64_86 platforms
## Prerequisites
* Install podman or docker (I prefer podman):
```
sudo apt install podman
```
* Install `cross` https://github.com/cross-rs/cross :
```
cargo install cross --git https://github.com/cross-rs/cross
```
## How to build
Execute `build_cross.sh`. Once `build_cross.sh` has finished running, you will find the deb packages in the `dist` folder.

# Data sources
DSO:
* Messier, NGC and IC catalogue from OpenNGC - https://github.com/mattiaverga/OpenNGC
* Caldwell catalogue - http://www.hawastsoc.org/deepsky/caldwell.html
* DSO nicknames list - https://www.astrobin.com/fg7b5l/
Stars:
* Tycho-2 catalogue - https://www.cosmos.esa.int/web/hipparcos/tycho-2
* HYG v3 catalogue - https://github.com/astronexus/HYG-Database/tree/main/hyg/v3
