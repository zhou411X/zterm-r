#!/bin/sh
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$project_dir"
if [ -x "$HOME/.cargo/bin/cargo" ]; then
    exec "$HOME/.cargo/bin/cargo" run --locked --release -j32 -- "$@"
fi
exec cargo run --locked --release -j32 -- "$@"
