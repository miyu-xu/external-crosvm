#!/bin/bash

set -e -u

if ! [ -x "$(command -v bpfmt)" ]; then
  echo 'Error: bpfmt not found.' >&2
  exit 1
fi

# Tell C2A to use the specific rust version that crosvm upstream expects.
#
# TODO: Consider reading the toolchain from external/crosvm/rust-toolchain
#
# TODO: Consider using android's prebuilt rust binaries. Currently doesn't work
# because they try to incorrectly use system clang and llvm.
RUST_TOOLCHAIN="1.62.0"
rustup which --toolchain $RUST_TOOLCHAIN cargo || \
  rustup toolchain install $RUST_TOOLCHAIN
CARGO_BIN="$(dirname $(rustup which --toolchain $RUST_TOOLCHAIN cargo))"

# TODO: build it first

C2A=

../../development/scripts/c2a/target/debug/c2a --cargo_bin $CARGO_BIN --cfg ./c2a.toml --reuse-cargo-out
# rm -f cargo.out
rm -rf target.tmp || /bin/true

# Fix workstation specific path in "metrics" crate's generated files.
# TODO(b/232150148): Find a better solution for protobuf generated files.
sed --in-place 's/path = ".*\/out/path = "./' metrics/out/generated.rs
