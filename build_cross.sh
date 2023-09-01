#!/usr/bin/env bash

# install cross before using of this script:
# cargo install cross --git https://github.com/cross-rs/cross

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

# arm64

TARGET=aarch64-unknown-linux-gnu
cross build --release --target $TARGET
cross-util run --target $TARGET -- "python3 $SCRIPT_DIR/scripts/create-deb-package.py --arch=arm64 --bin=$SCRIPT_DIR/target/$TARGET/release/astra_lite"

# armhf

TARGET=armv7-unknown-linux-gnueabihf
cross build --release --target $TARGET
cross-util run --target $TARGET -- "python3 $SCRIPT_DIR/scripts/create-deb-package.py --arch=armhf --bin=$SCRIPT_DIR/target/$TARGET/release/astra_lite"

# amd64

TARGET=x86_64-unknown-linux-gnu
cross build --release --target $TARGET
cross-util run --target $TARGET -- "python3 $SCRIPT_DIR/scripts/create-deb-package.py --arch=amd64 --bin=$SCRIPT_DIR/target/$TARGET/release/astra_lite"
