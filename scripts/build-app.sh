#!/bin/zsh
set -euo pipefail

root="${0:A:h:h}"
bundle="$root/dist/Glow.app"
contents="$bundle/Contents"

cd "$root"
cargo build --release --locked

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "$root/target/release/glow" "$contents/MacOS/Glow"
cp "$root/macos/Info.plist" "$contents/Info.plist"
cp "$root/macos/AppIcon.icns" "$contents/Resources/AppIcon.icns"
printf 'APPL????' > "$contents/PkgInfo"
chmod 755 "$contents/MacOS/Glow"

# An ad-hoc signature makes the local bundle behave like an ordinary Finder-launched app.
codesign --force --sign - "$bundle"
plutil -lint "$contents/Info.plist"

print "Built $bundle"
