#!/usr/bin/env bash

# install cross before using of this script:
# cargo install cross --git https://github.com/cross-rs/cross

set -e

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
TARGET_DIR=$SCRIPT_DIR/target_cross

# arm64

TARGET=aarch64-unknown-linux-gnu
cross build --release --target $TARGET --target-dir $TARGET_DIR
cross-util run --target $TARGET -- "python3 $SCRIPT_DIR/scripts/create-deb-package.py --arch=arm64 --bin=$TARGET_DIR/$TARGET/release/astra_lite"

# armhf

TARGET=armv7-unknown-linux-gnueabihf
cross build --release --target $TARGET --target-dir $TARGET_DIR
cross-util run --target $TARGET -- "python3 $SCRIPT_DIR/scripts/create-deb-package.py --arch=armhf --bin=$TARGET_DIR/$TARGET/release/astra_lite"

# amd64

TARGET=x86_64-unknown-linux-gnu
cross build --release --target $TARGET --target-dir $TARGET_DIR
cross-util run --target $TARGET -- "python3 $SCRIPT_DIR/scripts/create-deb-package.py --arch=amd64 --bin=$TARGET_DIR/$TARGET/release/astra_lite"
