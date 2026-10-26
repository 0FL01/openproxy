#!/bin/sh
# Dioxus 0.7.10 optimizes public PNGs, stripping color-profile chunks. Restore
# the source bytes in the actual served output after every release build.
set -eu

cd "$(dirname "$0")/.."
if [ -n "${DX:-}" ]; then
    dx=$DX
elif command -v dx >/dev/null 2>&1; then
    dx=dx
elif [ -x .tools/dx ]; then
    dx=./.tools/dx
else
    echo 'Dioxus CLI not found; set DX to its executable path' >&2
    exit 1
fi

"$dx" build --web --release --cargo-args=--locked
source_dir=public/providers
output_dir=target/dx/openproxy-dashboard/release/web/public/providers
mkdir -p "$output_dir"

# Incremental dx builds can retain deleted files. Refuse to produce a staged
# build with extra icons rather than silently serving intentionally absent ones.
for file in "$output_dir"/*.png; do
    [ -f "$file" ] || continue
    if [ ! -f "$source_dir/${file##*/}" ]; then
        echo "Unexpected stale provider icon in build output: $file" >&2
        exit 1
    fi
done

for file in "$source_dir"/*.png; do
    [ -f "$file" ] || continue
    cp "$file" "$output_dir/${file##*/}"
    cmp -s "$file" "$output_dir/${file##*/}"
done

echo "Release build with original provider icons: $output_dir/.."
