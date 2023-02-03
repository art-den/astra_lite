#!/bin/sh

# Requiriments
# sudo apt install python3 dpkg-dev

set -e

cd ..
cargo build --release

cd scripts
python3 ./create-deb-package.py
