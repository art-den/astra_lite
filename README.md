# AstraLite

AstraLite is software for deepsky astrophotography and live stacking
on low power PCs (like rapsberry Pi or Orange Pi)

AstraLite uses INDI server to work with astrophotography hardware.
See https://www.indilib.org/download.html to install INDI

# How to build AstraLite
## Prerequisites
* Rust compiler: https://www.rust-lang.org/tools/install
* Libs and tools:
```
sudo apt install gcc libgtk-3-dev build-essential libcfitsio-dev
```
## How to build
To build optimized binaries just type
```
cargo build --release
```
