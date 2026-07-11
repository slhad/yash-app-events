#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output="$root/crates/vision/tests/fixtures/ocr"
font=${YASH_OCR_FIXTURE_FONT:-/usr/share/fonts/noto/NotoSans-Bold.ttf}
mkdir -p "$output"

magick -size 640x120 xc:'#101522' -font "$font" -pointsize 64 -gravity center -fill white -annotate +0+0 'VICTORY' -depth 8 -colorspace Gray "$output/victory.png"
magick -size 640x120 xc:'#101522' -font "$font" -pointsize 52 -gravity center -fill white -annotate +0+0 'NIVEAU ÉTÉ' -depth 8 -colorspace Gray "$output/localized.png"
magick -size 320x60 xc:'#101522' -font "$font" -pointsize 30 -gravity center -fill white -annotate +0+0 'VICTORY' -depth 8 -colorspace Gray "$output/scaled.png"
magick -size 640x120 xc:'#101522' -font "$font" -pointsize 64 -gravity center -fill white -annotate +8+0 'VICTORY' -motion-blur 0x2+12 -depth 8 -colorspace Gray "$output/animated.png"
magick -size 640x120 xc:'#101522' -font "$font" -pointsize 64 -gravity center -stroke '#55aaff' -strokewidth 4 -fill white -annotate +0+0 'VICTORY' -depth 8 -colorspace Gray "$output/glow.png"
