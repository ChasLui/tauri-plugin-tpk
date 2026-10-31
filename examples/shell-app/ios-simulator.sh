#!/bin/sh
# Run with: sh examples/shell-app/ios-simulator.sh check|init|build [Tauri options]
set -eu

mode=${1:-check}
if [ "$#" -gt 0 ]; then shift; fi
case "$mode" in
  check|init|build) ;;
  *) echo 'Usage: ios-simulator.sh check|init|build [Tauri options]' >&2; exit 2 ;;
esac
if [ "$mode" = check ] && [ "$#" -ne 0 ]; then
  echo 'check does not accept additional options' >&2
  exit 2
fi
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo 'This entry point requires an Apple Silicon Mac with Xcode.' >&2
  exit 1
fi

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
export RUSTUP_HOME="$repo_dir/target/ios-rustup"
export RUSTUP_TOOLCHAIN=1.88.0
export CARGO_TARGET_DIR="$repo_dir/target/ios-smoke"
# Background builds must not inherit a terminal session's short-lived TMPDIR.
export TMPDIR="$CARGO_TARGET_DIR/tmp"
mkdir -p "$TMPDIR"
rustup_bin=${TPK_RUSTUP_BIN:-$HOME/.cargo/bin/rustup}
if [ ! -x "$rustup_bin" ]; then
  echo 'Set TPK_RUSTUP_BIN to the rustup executable (not a version-manager shim).' >&2
  exit 1
fi
cargo_bin=$("$rustup_bin" which cargo)
toolchain_bin=$(dirname -- "$cargo_bin")
if ! "$rustup_bin" target list --installed | /usr/bin/grep -qx aarch64-apple-ios-sim; then
  echo 'Install aarch64-apple-ios-sim in target/ios-rustup first; see docs/local-testing.md.' >&2
  exit 1
fi

# swift-rs invokes `swift build`. Xcode 27's default backend mixes macOS SDK
# flags into the simulator build; select SwiftPM's native backend locally.
tools_dir="$repo_dir/target/ios-tools"
mkdir -p "$tools_dir"
cat > "$tools_dir/swift" <<'SWIFT'
#!/bin/sh
if [ "$1" = build ]; then
  exec /usr/bin/swift "$@" --build-system native --disable-keychain --disable-netrc
fi
exec /usr/bin/swift "$@"
SWIFT
chmod 755 "$tools_dir/swift"
export PATH="$tools_dir:$toolchain_bin:$(dirname -- "$rustup_bin"):$PATH"

case "$mode" in
  check)
    exec "$cargo_bin" check --manifest-path "$repo_dir/examples/shell-app/src-tauri/Cargo.toml" \
      --target aarch64-apple-ios-sim --locked
    ;;
  init)
    missing=0
    for dependency in xcodegen idevicesyslog pod; do
      if ! command -v "$dependency" >/dev/null 2>&1; then
        echo "Missing iOS init prerequisite: $dependency" >&2
        missing=1
      fi
    done
    if [ "$missing" -ne 0 ]; then
      echo 'Stopped before Tauri can install global dependencies. See docs/local-testing.md.' >&2
      exit 1
    fi
    exec pnpm -C "$repo_dir/examples/shell-app" tauri ios init --ci --skip-targets-install "$@"
    ;;
  build)
    if [ ! -f "$repo_dir/examples/shell-app/src-tauri/gen/apple/project.yml" ]; then
      echo 'Apple project is absent. Complete ios-simulator.sh init first.' >&2
      exit 1
    fi
    exec pnpm -C "$repo_dir/examples/shell-app" tauri ios build \
      --debug --target aarch64-sim --no-sign --ci "$@"
    ;;
esac
