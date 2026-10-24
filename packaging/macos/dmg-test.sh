#!/usr/bin/env bash
# The app in the macOS DMG, and a copy installed from it, pass `codesign --verify --strict --deep`
# (#3034). Runs package.sh and verify.sh end to end, signed ad hoc, around a stand-in executable
# (no Rust build, no secrets), with a font licence that carries Finder info as the input. Then
# shows the check rejects the image `hdiutil makehybrid -hfs` made of the same files. macOS only;
# CI: .github/workflows/packaging-lint.yml.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
if [[ "$(uname -s)" != Darwin ]]; then
  echo "skip: needs macOS (hdiutil, codesign)"
  exit 0
fi
case "$(uname -m)" in
  arm64) ARCH=aarch64 TRIPLE=aarch64-apple-darwin CC_ARCH=arm64 ;;
  x86_64) ARCH=x86_64 TRIPLE=x86_64-apple-darwin CC_ARCH=x86_64 ;;
  *) fail "unknown machine $(uname -m)" ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# Ad hoc, not notarized, and no Actions annotations or step summary from the scripts under test.
unset MACOS_SIGN_IDENTITY MACOS_KEYCHAIN APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID GITHUB_ACTIONS GITHUB_STEP_SUMMARY
export CARGO_TARGET_DIR="$TMP/target" DIST="$TMP/dist" CRAFT_FONTS_DIR="$TMP/craft-fonts"
# shellcheck source=../env.sh
. "$HERE/../env.sh"
# shellcheck source=dmg.sh
. "$HERE/dmg.sh"

# package.sh only joins, signs and runs (`photocraft-cli --version`) the binaries, so any
# executable stands in for them.
bin="$CARGO_TARGET_DIR/$TRIPLE/release"
mkdir -p "$bin"
printf 'int main(void) { return 0; }\n' | cc -arch "$CC_ARCH" -x c -o "$bin/photocraft" -
cp "$bin/photocraft" "$bin/photocraft-cli"
# A licence with Finder info (what makehybrid gave every file): `cp` brings it into the bundle, so
# the strip before signing is tested too.
mkdir -p "$CRAFT_FONTS_DIR/fonts/test-sans"
echo "test licence" >"$CRAFT_FONTS_DIR/fonts/test-sans/OFL.txt"
/usr/bin/xattr -wx com.apple.FinderInfo 00000000000000000000FFFFFFFF000000000000000000000000000000000000 \
  "$CRAFT_FONTS_DIR/fonts/test-sans/OFL.txt"

"$HERE/package.sh" --arch "$ARCH" --skip-build || fail "package.sh failed"
"$HERE/verify.sh" --arch "$ARCH" || fail "verify.sh failed"
DMG="$DIST/photocraft-$VERSION-macos-$ARCH.dmg"
check_dmg_app "$DMG" PhotoCraft.app || fail "the app in $(basename "$DMG") fails codesign --strict"

# The check must catch the defect it guards against: the same staged files, imaged the old way.
OLD="$TMP/makehybrid.dmg"
hdiutil makehybrid -quiet -hfs -hfs-volume-name PhotoCraft -o "$OLD" "$CARGO_TARGET_DIR/macos-package/dmg"
if out="$(check_dmg_app "$OLD" PhotoCraft.app 2>&1)"; then
  fail "check_dmg_app accepted a makehybrid image"
fi
case "$out" in
  *"Finder information, or similar detritus"*) ;;
  *) fail "check_dmg_app rejected the makehybrid image for another reason: $out" ;;
esac

echo "ok: the app in the DMG passes codesign --strict; a makehybrid image of it is rejected"
