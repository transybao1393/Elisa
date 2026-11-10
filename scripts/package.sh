#!/bin/bash
# Build a release of Elisa: Elisa.app (the SwiftUI window with the Rust core inside) in a .dmg.
#
#   scripts/package.sh                 build, ad-hoc sign, make dist/Elisa-<version>.dmg
#
# Environment (all optional):
#   VERSION         the version written into Info.plist and the file name (default: elisa/Cargo.toml's version)
#   SIGN_IDENTITY   a "Developer ID Application: …" identity in the keychain: sign with it (hardened runtime,
#                   secure timestamp, scripts/entitlements.plist). Unset: ad-hoc signature ("-"), which runs on this
#                   Mac but Gatekeeper blocks on others.
#   NOTARY_PROFILE  a `xcrun notarytool store-credentials` profile name: notarise the .dmg and staple the ticket.
#                   Needs SIGN_IDENTITY (Apple only notarises Developer ID signed code).
#   BUNDLE_ID       CFBundleIdentifier (default io.github.transybao1393.elisa)
#   OUT_DIR         where the .app and .dmg go (default: dist/ at the repo root)
#   SKIP_BUILD=1    reuse the existing release builds (elisa/target/release/elisa, app/ElisaApp/.build/release)
#
# Layout of Elisa.app:
#   Contents/MacOS/ElisaApp                              the window (swift build -c release)
#   Contents/MacOS/elisa                                 the core (cargo build --release); Link.swift finds it here
#   Contents/Resources/ElisaApp_ElisaApp.bundle/…        the SwiftPM resources: the OFL fonts and the brand files
#   Contents/Resources/design/                           the brand kit (elisa/design); Kit.swift finds it here
#   Contents/Resources/Elisa.icns                        the app icon
# See docs/release.md.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CORE="$ROOT/elisa"
APP_SRC="$ROOT/app/ElisaApp"
OUT_DIR="${OUT_DIR:-$ROOT/dist}"
BUNDLE_ID="${BUNDLE_ID:-io.github.transybao1393.elisa}"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$CORE/Cargo.toml" | head -1)}"
VERSION="${VERSION#v}"
SIGN_IDENTITY="${SIGN_IDENTITY:-}"
NOTARY_PROFILE="${NOTARY_PROFILE:-}"
ENTITLEMENTS="$ROOT/scripts/entitlements.plist"
PLB=/usr/libexec/PlistBuddy

say() { printf '[package] %s\n' "$*"; }
die() { printf '[package] error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || die "macOS only"
[ -n "$VERSION" ] || die "no version (set VERSION)"
# Info.plist takes numbers only: 0.1.0-beta.1 → 0.1.0 (the full version stays in the file name)
PLIST_VERSION="${VERSION%%-*}"
if [ -n "$NOTARY_PROFILE" ] && [ -z "$SIGN_IDENTITY" ]; then die "NOTARY_PROFILE needs SIGN_IDENTITY (Apple notarises only Developer ID signed apps)"; fi

# ---- 1. build ----
if [ "${SKIP_BUILD:-}" != 1 ]; then
  say "building the core (cargo build --release --locked)"
  (cd "$CORE" && cargo build --release --locked)
  say "building the app (swift build -c release)"
  (cd "$APP_SRC" && swift build -c release)
fi
CORE_BIN="$CORE/target/release/elisa"
APP_BUILD="$(cd "$APP_SRC" && swift build -c release --show-bin-path)"
APP_BIN="$APP_BUILD/ElisaApp"
RES_BUNDLE="$APP_BUILD/ElisaApp_ElisaApp.bundle"
[ -x "$CORE_BIN" ] || die "missing $CORE_BIN"
[ -x "$APP_BIN" ] || die "missing $APP_BIN"
[ -d "$RES_BUNDLE" ] || die "missing $RES_BUNDLE"

# ---- 2. assemble Elisa.app ----
APP="$OUT_DIR/Elisa.app"
say "assembling $APP (version $VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$APP_BIN" "$APP/Contents/MacOS/ElisaApp"
cp "$CORE_BIN" "$APP/Contents/MacOS/elisa"
chmod 755 "$APP/Contents/MacOS/ElisaApp" "$APP/Contents/MacOS/elisa"

# Info.plist: the app's own (microphone string, LSMinimumSystemVersion) plus the bundle keys
PL="$APP/Contents/Info.plist"
cp "$APP_SRC/Info.plist" "$PL"
setkey() { "$PLB" -c "Delete :$1" "$PL" >/dev/null 2>&1 || true; "$PLB" -c "Add :$1 $2 $3" "$PL"; }
setkey CFBundleIdentifier string "$BUNDLE_ID"
setkey CFBundleName string Elisa
setkey CFBundleDisplayName string Elisa
setkey CFBundleExecutable string ElisaApp
setkey CFBundlePackageType string APPL
setkey CFBundleInfoDictionaryVersion string 6.0
setkey CFBundleShortVersionString string "$PLIST_VERSION"
setkey CFBundleVersion string "$PLIST_VERSION"
setkey CFBundleIconFile string Elisa
setkey NSHighResolutionCapable bool true
setkey NSPrincipalClass string NSApplication
setkey LSApplicationCategoryType string public.app-category.productivity
"$PLB" -c "Print :LSMinimumSystemVersion" "$PL" >/dev/null 2>&1 || setkey LSMinimumSystemVersion string 14.0
plutil -lint "$PL" >/dev/null

# SwiftPM's flat resource bundle, rebuilt as a real bundle (Contents/Info.plist + Contents/Resources) so codesign
# accepts it; Kit.swift's resourceBundle() reads Contents/Resources inside it
RB="$APP/Contents/Resources/ElisaApp_ElisaApp.bundle"
mkdir -p "$RB/Contents/Resources"
cp -R "$RES_BUNDLE/." "$RB/Contents/Resources/"
cat > "$RB/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key><string>$BUNDLE_ID.resources</string>
	<key>CFBundleName</key><string>ElisaApp_ElisaApp</string>
	<key>CFBundlePackageType</key><string>BNDL</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>CFBundleShortVersionString</key><string>$PLIST_VERSION</string>
	<key>CFBundleVersion</key><string>$PLIST_VERSION</string>
</dict>
</plist>
EOF

# the brand kit the window draws Shade and its colours from (read at launch, Kit.swift)
cp -R "$CORE/design" "$APP/Contents/Resources/design"
cp "$APP_SRC/Sources/ElisaApp/Resources/Brand/Elisa.icns" "$APP/Contents/Resources/Elisa.icns"
# licences travel with the binaries
for f in LICENSE NOTICE THIRD_PARTY_NOTICES.md; do [ -f "$ROOT/$f" ] && cp "$ROOT/$f" "$APP/Contents/Resources/"; done
find "$APP" -name .DS_Store -delete
xattr -cr "$APP"

# ---- 3. sign (inside out: the resource bundle, the core, then the app) ----
if [ -n "$SIGN_IDENTITY" ]; then
  say "signing with $SIGN_IDENTITY (hardened runtime)"
  SIGN=(codesign --force --timestamp --options runtime --sign "$SIGN_IDENTITY")
  "${SIGN[@]}" "$RB"
  "${SIGN[@]}" "$APP/Contents/MacOS/elisa"
  "${SIGN[@]}" --entitlements "$ENTITLEMENTS" "$APP"
else
  say "signing ad-hoc (no SIGN_IDENTITY): runs on this Mac; Gatekeeper blocks it elsewhere"
  codesign --force --sign - "$RB"
  codesign --force --sign - "$APP/Contents/MacOS/elisa"
  codesign --force --sign - --entitlements "$ENTITLEMENTS" "$APP"
fi
codesign --verify --strict --verbose=2 "$APP"

# ---- 4. the .dmg ----
DMG="$OUT_DIR/Elisa-$VERSION.dmg"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
say "building $DMG"
hdiutil create -volname "Elisa" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
[ -n "$SIGN_IDENTITY" ] && codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"

# ---- 5. notarise ----
if [ -n "$NOTARY_PROFILE" ]; then
  say "notarising (notarytool profile $NOTARY_PROFILE); this can take minutes"
  xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG" || true
fi

say "done: $DMG ($(du -h "$DMG" | cut -f1))"
