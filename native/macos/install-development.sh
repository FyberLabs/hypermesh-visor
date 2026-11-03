#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
app="$HOME/Applications/Hypermesh Visor Development.app"
if [[ -e "$app" ]]; then
  echo "Development app already exists: $app. Quit and remove that development bundle explicitly before reinstalling." >&2
  exit 1
fi
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
bundle="$stage/Hypermesh Visor Development.app"
mkdir -p "$bundle/Contents/MacOS"
swiftc -parse-as-library -O native/macos/Desktop.swift -o "$bundle/Contents/MacOS/hypermesh-native-desktop"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>sh.hyperme.visor.development</string>
<key>CFBundleName</key><string>Hypermesh Visor Development</string>
<key>CFBundleDisplayName</key><string>Hypermesh Visor Development</string>
<key>CFBundleExecutable</key><string>hypermesh-native-desktop</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.1.0-dev</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Local ad-hoc identity only: no Developer ID certificate or notarization.
codesign --force --sign - --identifier sh.hyperme.visor.development "$bundle"
codesign --verify --strict "$bundle"
mkdir -p "$HOME/Applications"
mv "$bundle" "$app"
open "$app" --args --setup
printf 'Installed development permission helper: %s\n' "$app"
