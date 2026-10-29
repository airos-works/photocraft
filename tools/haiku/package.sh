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
# egui's default fonts come from the storytold/egui commit patched in Cargo.toml (a git checkout).
mkdir -p "$DOCS/licenses/epaint_default_fonts"
EGUI_REV=$(sed -n 's/^epaint_default_fonts = .*rev = "\([0-9a-f]*\)".*/\1/p' Cargo.toml)
cp "$CARGO_HOME"/git/checkouts/egui-*/"${EGUI_REV:0:7}"/crates/epaint_default_fonts/fonts/*.txt "$DOCS/licenses/epaint_default_fonts/"
# hayro, the PDF renderer the print backend uses, embeds fonts, CMaps and colour profiles.
for crate in hayro-cmap hayro-interpret hayro-jpeg2000; do
	version=$(awk -F '"' -v name="$crate" '$0 == "name = \"" name "\"" { found = 1; next }
		found && /^version = / { print $2; exit }' Cargo.lock)
	mkdir -p "$DOCS/licenses/$crate"
	cp "$CARGO_HOME"/registry/src/*/"$crate-$version"/assets/*LICENSE* "$DOCS/licenses/$crate/"
done
ln -s ../../../../apps/PhotoCraft "$STAGE/data/deskbar/menu/Applications/PhotoCraft"
"$TOOLS/package/package" create -C "$STAGE" "$OUT/photocraft-$VERSION-x86_64.hpkg"
