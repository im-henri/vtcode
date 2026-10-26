#!/usr/bin/env bash
set -euo pipefail
mkdir -p "$HOME" "$CARGO_HOME"
# Build-only mode needs neither the ACP peers nor their Python environment.
for argument in "$@"; do
    if [[ "$argument" == "--skip-tests" ]]; then
        exec python3 scripts/local.py release "$@"
    fi
done
export VTCODE_ACP_TEST_PYTHON=/cache/venv/bin/python
if [[ ! -x "$VTCODE_ACP_TEST_PYTHON" ]]; then
    python3 -m venv /cache/venv
fi
python3 scripts/local.py setup --glibc "$2"
exec python3 scripts/local.py release "$@"
