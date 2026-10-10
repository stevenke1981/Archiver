#!/usr/bin/env bash
# Build a universal (arm64 + x86_64) "Rust Archiver.app" and zip it into dist/.
set -euo pipefail
VERSION=$(grep -m1 '^version' gui/Cargo.toml | cut -d '"' -f2)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --locked --target aarch64-apple-darwin
cargo build --release --locked --target x86_64-apple-darwin
mkdir -p dist/universal
for b in rust-archiver rarc; do
  lipo -create -output dist/universal/$b \
    target/aarch64-apple-darwin/release/$b target/x86_64-apple-darwin/release/$b
done
lipo -info dist/universal/rust-archiver

APP="dist/Rust Archiver.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp dist/universal/rust-archiver dist/universal/rarc "$APP/Contents/MacOS/"
sed "s/__VERSION__/$VERSION/g" scripts/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
# ad-hoc signature so Gatekeeper reports "unidentified developer" rather than "damaged"
codesign --force --deep --sign - "$APP" || true

cd dist
ditto -c -k --sequesterRsrc --keepParent "Rust Archiver.app" rust-archiver-macos-universal-app.zip
mkdir -p rust-archiver-macos-universal
cp universal/rust-archiver universal/rarc ../README.md ../LICENSE rust-archiver-macos-universal/
tar czf rust-archiver-macos-universal.tar.gz rust-archiver-macos-universal
ls -la
