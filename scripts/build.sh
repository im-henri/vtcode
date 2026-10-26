#!/bin/sh
set -eu
build_script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$build_script_dir/build.py" "$@"
