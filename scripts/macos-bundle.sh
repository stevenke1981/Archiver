#!/usr/bin/env bash
# Build a universal (arm64 + x86_64) "Archiver.app" and zip it into dist/.
set -euo pipefail
VERSION=$(grep -m1 '^version' gui/Cargo.toml | cut -d '"' -f2)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin
mkdir -p dist/universal
for b in archiver rarc; do
  lipo -create -output dist/universal/$b \
    target/aarch64-apple-darwin/release/$b target/x86_64-apple-darwin/release/$b
done
lipo -info dist/universal/archiver

APP="dist/Archiver.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp dist/universal/archiver dist/universal/rarc "$APP/Contents/MacOS/"
sed "s/__VERSION__/$VERSION/g" scripts/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
# ad-hoc signature so Gatekeeper reports "unidentified developer" rather than "damaged"
codesign --force --deep --sign - "$APP" || true

cd dist
ditto -c -k --sequesterRsrc --keepParent "Archiver.app" archiver-macos-universal-app.zip
mkdir -p archiver-macos-universal
cp universal/archiver universal/rarc ../README.md ../LICENSE archiver-macos-universal/
tar czf archiver-macos-universal.tar.gz archiver-macos-universal
ls -la
