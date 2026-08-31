#!/usr/bin/env bash

set -euo pipefail

source_image="${1:-assets/dupekit-icon.png}"
output_image="assets/dupekit.icns"

if [[ ! -f "$source_image" ]]; then
  echo "Source image not found: $source_image" >&2
  exit 1
fi

for command in sips xxd; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "Required macOS command not found: $command" >&2
    exit 1
  fi
done

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/dupekit-icon.XXXXXX")"
iconset_dir="$temp_dir/dupekit.iconset"
temp_output="$temp_dir/dupekit.icns"
trap 'rm -rf "$temp_dir"' EXIT

mkdir "$iconset_dir"

for size in 16 32 64 128 256 512 1024; do
  sips -z "$size" "$size" "$source_image" --out "$iconset_dir/icon_${size}x${size}.png" >/dev/null
done

append_chunk() {
  local chunk_type="$1"
  local image="$2"
  local image_size
  local chunk_size

  image_size="$(wc -c < "$image" | tr -d ' ')"
  chunk_size=$((image_size + 8))

  printf '%s' "$chunk_type" >> "$temp_output"
  printf '%08x' "$chunk_size" | xxd -r -p >> "$temp_output"
  cat "$image" >> "$temp_output"
}

# ICNS stores PNG images in typed chunks; the type maps to the image's pixel size.
printf 'icns' > "$temp_output"
total_size=8
for image in "$iconset_dir"/*.png; do
  total_size=$((total_size + $(wc -c < "$image" | tr -d ' ') + 8))
done
printf '%08x' "$total_size" | xxd -r -p >> "$temp_output"

append_chunk icp4 "$iconset_dir/icon_16x16.png"
append_chunk icp5 "$iconset_dir/icon_32x32.png"
append_chunk icp6 "$iconset_dir/icon_64x64.png"
append_chunk ic07 "$iconset_dir/icon_128x128.png"
append_chunk ic08 "$iconset_dir/icon_256x256.png"
append_chunk ic09 "$iconset_dir/icon_512x512.png"
append_chunk ic10 "$iconset_dir/icon_1024x1024.png"

mv "$temp_output" "$output_image"

echo "Updated $output_image from $source_image"
