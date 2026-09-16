#!/bin/sh
# Builds "Tartarus Driver.app" (universal: Apple Silicon + Intel) from the
# tartarus_driver crate and code-signs it.
#
#   scripts/macos/make-app.sh [output-dir]           # default: dist/
#   SIGN_IDENTITY="Tartarus Driver Dev" scripts/macos/make-app.sh
#
# Why a bundle at all: macOS ties its Accessibility / Input Monitoring
# grants to the code identity of the process. A bare, unsigned binary gets a
# fresh identity on every rebuild and loses the grants; a signed bundle keeps
# them as long as the signing identity stays the same. So:
#   - SIGN_IDENTITY unset  -> ad-hoc signature ("-"): fine for trying it out,
#                             but grants must be re-done after every rebuild.
#   - SIGN_IDENTITY set    -> a certificate from your login keychain. For a
#                             stable local identity create a self-signed
#                             "Code Signing" certificate in Keychain Access
#                             (Certificate Assistant > Create a Certificate…,
#                             Identity Type: Self Signed Root, Certificate
#                             Type: Code Signing) and pass its name here.
#                             A Developer ID certificate works the same way.
# Either way the binary inside the bundle is exactly the CLI binary: run it
# with a subcommand (`configui`, `emulate`, `tray`) or `sudo` it for D-pad /
# Hyper Shift — see USAGE.md's macOS section. With no argument a bundled
# launch (Finder double-click) means `tray`.
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CRATE="$ROOT/tartarus_driver"
OUT="${1:-$ROOT/dist}"
APP="$OUT/Tartarus Driver.app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$CRATE/Cargo.toml" | head -1)"
IDENTITY="${SIGN_IDENTITY:--}"

cd "$CRATE"
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target list --installed | grep -q "^$t\$" || rustup target add "$t"
    cargo build --release --locked --target "$t"
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create \
    "target/aarch64-apple-darwin/release/tartarus_driver" \
    "target/x86_64-apple-darwin/release/tartarus_driver" \
    -output "$APP/Contents/MacOS/tartarus_driver"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>      <string>en</string>
    <key>CFBundleExecutable</key>             <string>tartarus_driver</string>
    <key>CFBundleIdentifier</key>             <string>io.github.open-tartarus-driver</string>
    <key>CFBundleInfoDictionaryVersion</key>  <string>6.0</string>
    <key>CFBundleName</key>                   <string>Tartarus Driver</string>
    <key>CFBundleDisplayName</key>            <string>Tartarus Driver</string>
    <key>CFBundlePackageType</key>            <string>APPL</string>
    <key>CFBundleShortVersionString</key>     <string>$VERSION</string>
    <key>CFBundleVersion</key>                <string>$VERSION</string>
    <key>LSMinimumSystemVersion</key>         <string>11.0</string>
    <!-- Menu-bar only: no Dock icon, no app menu (the tray draws its own). -->
    <key>LSUIElement</key>                    <true/>
    <key>NSHumanReadableCopyright</key>       <string>GPL-3.0-or-later — ultramonaka and contributors</string>
</dict>
</plist>
PLIST

# --options runtime (hardened runtime) is what a Developer ID / notarized
# build wants; harmless for ad-hoc and self-signed too.
codesign --force --sign "$IDENTITY" --options runtime --timestamp=none "$APP"
codesign --verify --verbose=2 "$APP"

echo
echo "Built: $APP (v$VERSION, universal, signed by '$IDENTITY')"
lipo -info "$APP/Contents/MacOS/tartarus_driver"
