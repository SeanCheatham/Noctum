#!/bin/sh
set -eu

project=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
configuration=${1:-release}
case "$configuration" in
    debug|release) ;;
    *) echo "Use debug or release." >&2; exit 1 ;;
esac
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    echo "Build the host helper on an Apple silicon Mac." >&2
    exit 1
fi
swift build --package-path "$project/runtime/host" -c "$configuration" --jobs 2 --disable-automatic-resolution
output=$(swift build --package-path "$project/runtime/host" -c "$configuration" --show-bin-path)
mkdir -p "$project/target/$configuration"
temporary="$project/target/$configuration/.noctum-host-$$"
trap 'if [ -f "$temporary" ]; then rm "$temporary"; fi' EXIT
cp "$output/noctum-host" "$temporary"
codesign --force --sign - --entitlements "$project/runtime/host/NoctumHost.entitlements" "$temporary"
mv "$temporary" "$project/target/$configuration/noctum-host"
