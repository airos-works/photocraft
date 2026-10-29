#!/usr/bin/env bash
# Cross-build and package entirely on Linux with Haiku's host tools.
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
. tools/haiku/env.sh
TOOLS=${HAIKU_HOST_TOOLS:-/mnt/HaikuWork/build/fw13-x86_64/objects/linux/x86_64/release/tools}
RES="$ROOT/apps/photocraft-haiku/resources"
OUT="$ROOT/artifacts/haiku"
VERSION=$(awk '$1 == "version" { print $2; exit }' "$RES/PhotoCraft.PackageInfo")
if [[ ${1:-} != --no-build ]]; then bash tools/haiku/build.sh; fi
mkdir -p "$OUT"
mkdir -p "$ROOT/target/tmp"
STAGE=$(mktemp -d "$ROOT/target/tmp/photocraft-pkg-XXXXXX")
trap 'rm -rf -- "$STAGE"' EXIT
DOCS="$STAGE/documentation/packages/photocraft"
mkdir -p "$STAGE/bin" "$STAGE/apps" "$STAGE/data/deskbar/menu/Applications" "$STAGE/boot/post-install" "$DOCS"
"$HAIKU_CROSS-strip" -o "$STAGE/apps/PhotoCraft" "$CARGO_TARGET_DIR/x86_64-unknown-haiku/release/PhotoCraft"
(cd "$RES" && "$TOOLS/rc/rc" -o "$STAGE/PhotoCraft.rsrc" PhotoCraft.rdef)
"$TOOLS/xres" -o "$STAGE/apps/PhotoCraft" "$STAGE/PhotoCraft.rsrc"
"$TOOLS/resattr/resattr" -O -o "$STAGE/apps/PhotoCraft" "$STAGE/PhotoCraft.rsrc"
rm "$STAGE/PhotoCraft.rsrc"
cp "$RES/PhotoCraft.PackageInfo" "$STAGE/.PackageInfo"
cp "$RES/photocraft-post-install.sh" "$STAGE/boot/post-install/photocraft.sh"
chmod 755 "$STAGE/boot/post-install/photocraft.sh" "$STAGE/apps/PhotoCraft"
cp README.md LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md docs/haiku.md "$DOCS/"
# Redistribute the license texts for the assets embedded by the upstream editor.
while IFS= read -r license; do
    mkdir -p "$DOCS/licenses/$(dirname -- "$license")"
    cp "$license" "$DOCS/licenses/$license"
done < <(rg --files assets crates vendor apps/photocraft-haiku tools/haiku | rg '(^|/)(LICENSE[^/]*|OFL[^/]*|COPYING[^/]*)$')
# The craft-fonts build embeds OFL-1.1 fonts: their licences, one per font family.
if [[ -n ${CRAFT_FONTS_DIR-"$ROOT/../craft-fonts"} ]]; then
    for ofl in "${CRAFT_FONTS_DIR-"$ROOT/../craft-fonts"}"/fonts/*/OFL.txt; do
        mkdir -p "$DOCS/licenses/craft-fonts"
        cp "$ofl" "$DOCS/licenses/craft-fonts/OFL-$(basename -- "$(dirname -- "$ofl")").txt"
    done
fi
mkdir -p "$DOCS/licenses/epaint_default_fonts"
cp "$CARGO_HOME"/registry/src/*/epaint_default_fonts-0.36.2/fonts/*.txt "$DOCS/licenses/epaint_default_fonts/"
ln -s ../../../../apps/PhotoCraft "$STAGE/data/deskbar/menu/Applications/PhotoCraft"
"$TOOLS/package/package" create -C "$STAGE" "$OUT/photocraft-$VERSION-x86_64.hpkg"
