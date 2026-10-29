#!/usr/bin/env bash
# Cross-compiles PhotoCraft for air/OS (Haiku x86_64): target/x86_64-unknown-haiku/release/PhotoCraft.
# See docs/haiku.md for the toolchain (rustup stable with rust-src, the Haiku sysroot).
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
. tools/haiku/env.sh
# Release builds embed the craft-fonts collection (CJK, Arabic, Hebrew, Thai, Devanagari…) for
# the text tool and the interface, as upstream's CI does (crates/text/build.rs). Use the commit
# upstream's workflows pin (craft-fonts-ref in .github/workflows/release.yml); CRAFT_FONTS_DIR=
# (empty) builds without it.
export CRAFT_FONTS_DIR=${CRAFT_FONTS_DIR-"$ROOT/../craft-fonts"}
if [[ -n $CRAFT_FONTS_DIR ]]; then
	export CRAFT_FONTS_REQUIRED=1
fi
PHOTOCRAFT_BUILD_SHA=${PHOTOCRAFT_BUILD_SHA:-$(git rev-parse HEAD 2>/dev/null || echo dev)} \
	cargo build --locked --release --target x86_64-unknown-haiku -Zbuild-std=std,panic_abort -j "${JOBS:-4}" -p photocraft-haiku "$@"
