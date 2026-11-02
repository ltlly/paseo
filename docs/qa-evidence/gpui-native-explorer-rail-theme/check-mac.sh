#!/bin/bash
set -eu -o pipefail
sha='344203bd5cf43ba082c59df118c333c93128b3b040f484e33a13a8c6cd5ccb90'
short=${sha:0:12}
archive="$HOME/.cache/paseo-gpui-current-$short.tar.gz"
src="$HOME/.cache/paseo-gpui-src-$short"
log="$HOME/.cache/paseo-gpui-check-$short.log"
exit_file="$HOME/.cache/paseo-gpui-check-$short.exit"
: > "$log"
say(){ printf '%s\n' "$*" | tee -a "$log"; }
trap 'rc=$?; printf "%s\n" "$rc" > "$exit_file"' EXIT
actual=$(shasum -a 256 "$archive" | cut -d' ' -f1)
say "archive=$archive"
say "expected_sha256=$sha"
say "actual_sha256=$actual"
[ "$actual" = "$sha" ] || { say "ERROR: Source hash mismatch"; exit 1; }
mkdir "$src"
tar -xzf "$archive" -C "$src"
say "extracted=$src"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=2
export CARGO_TERM_COLOR=never
export CARGO_TARGET_DIR="$HOME/.cache/paseo-gpui-target"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
cd "$src"
say "cargo=$(cargo --version 2>&1)"
say "rustc=$(rustc --version 2>&1)"
say "STEP=cargo_clean"
cargo clean -p paseo-desktop 2>&1 | tee -a "$log"
say "STEP=cargo_check"
cargo check --locked -p paseo-desktop --features runtime-shaders 2>&1 | tee -a "$log"
say "TEST_FILTER=workspace_tab_layout::tests"
cargo test --locked -p paseo-desktop --no-default-features --lib workspace_tab_layout::tests -- --test-threads=1 2>&1 | tee -a "$log"
say "STEP=cargo_native_build"
cargo build --locked -p paseo-desktop --bin paseo-desktop --features runtime-shaders 2>&1 | tee -a "$log"
say "RESULT=0"
