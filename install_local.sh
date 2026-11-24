#!/bin/sh
# Build and install Noctum from this checkout.
set -eu

usage() {
    echo "Usage: $0 [--debug] [--bin-dir PATH]"
    echo "Build and install both host tools."
    printf 'Default directory: %s\n' "${CARGO_HOME:-$HOME/.cargo}/bin"
}

configuration=release
install_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --debug) configuration=debug; shift ;;
        --bin-dir)
            if [ "$#" -lt 2 ] || [ -z "$2" ]; then
                usage >&2
                exit 1
            fi
            install_dir=$2
            shift 2
            ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 1 ;;
    esac
done

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    echo "Noctum requires an Apple silicon Mac." >&2
    exit 1
fi
if [ "$(sw_vers -productVersion | cut -d. -f1)" -lt 26 ]; then
    echo "Noctum requires macOS 26 or later." >&2
    exit 1
fi

case "$install_dir" in
    /*) ;;
    *) install_dir="$PWD/$install_dir" ;;
esac
project=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$project"

sh scripts/build-host.sh "$configuration"
set -- build --locked --bin noctum --target aarch64-apple-darwin --target-dir "$project/target"
if [ "$configuration" = release ]; then
    set -- "$@" --release
fi
cargo "$@"

binary="$project/target/aarch64-apple-darwin/$configuration/noctum"
helper="$project/target/$configuration/noctum-host"
codesign --verify --strict "$helper"

mkdir -p "$install_dir"
install -m 755 "$binary" "$helper" "$install_dir/"

printf 'Installed both host tools into %s.\n' "$install_dir"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) printf 'Add this directory to PATH: %s\n' "$install_dir" ;;
esac
