#!/usr/bin/env bash
# Static release build: no system compiler, headers, pkg-config, or native libraries.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
check=false
case ${1:-} in
  --check) check=true ;;
  --no-install|--install-deps|'') ;;
  --help|-h)
    printf '%s\n' 'Usage: ./build.sh [--check | --no-install]' \
      'Build a static dist/bharta; provision Rust 1.92 and a private Zig compiler as needed.' \
      'Requires Bash, curl, tar, xz and sha256sum. Never installs system packages.' \
      '--check: report missing tools without downloading or changing anything.'
    exit 0 ;;
  *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { printf '%s\n' 'Only one option is accepted.' >&2; exit 2; }
[[ $(uname -s) == Linux ]] || { printf '%s\n' 'This Wayland bar targets Linux.' >&2; exit 1; }
arch=$(uname -m)
case $arch in
  x86_64) zig_sha=24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c ;;
  aarch64) zig_sha=f7a654acc967864f7a050ddacfaa778c7504a0eca8d2b678839c21eea47c992b ;;
  *) printf 'Unsupported build architecture: %s\n' "$arch" >&2; exit 1 ;;
esac
target="$arch-unknown-linux-musl"
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
[[ ! -x $cargo_bin/rustup ]] || export PATH="$cargo_bin:$PATH"
missing=()
for tool in curl tar xz sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if ((${#missing[@]})); then
  printf 'Missing bootstrap tools: %s\n' "${missing[*]}" >&2
  exit 1
fi
rust_ok=false
if command -v rustc >/dev/null && command -v cargo >/dev/null; then
  read -r _ version _ < <(rustc --version)
  IFS=. read -r major minor _ <<< "$version"
  if ((major > 1 || (major == 1 && minor >= 92))); then rust_ok=true; fi
fi
zig_dir="$PWD/.build/zig-$arch-linux-0.14.1"
if $check; then
  $rust_ok || { printf '%s\n' 'Missing Rust 1.92+ (automatically provisioned by ./build.sh).'; exit 1; }
  [[ -x $zig_dir/zig ]] || printf '%s\n' 'The build will download a checksum-verified private Zig compiler.'
  printf 'Bootstrap tools available. Static target: %s. No native development packages required.\n' "$target"
  exit 0
fi
mkdir -p .build
cargo_cmd=(cargo)
if ! $rust_ok; then
  if ! command -v rustup >/dev/null; then
    curl -fsSL --proto '=https' --tlsv1.2 https://sh.rustup.rs -o .build/rustup-init.sh
    sh .build/rustup-init.sh -y --profile minimal --default-toolchain 1.92.0 --no-modify-path
  else
    rustup toolchain install 1.92.0 --profile minimal
  fi
  export PATH="$cargo_bin:$PATH"
  cargo_cmd=(cargo +1.92.0)
fi
command -v rustup >/dev/null || { printf 'Install the %s Rust standard library with your toolchain manager.\n' "$target" >&2; exit 1; }
if [[ ${cargo_cmd[1]:-} == +1.92.0 ]]; then
  rustup target add --toolchain 1.92.0 "$target"
else
  rustup target add "$target"
fi
if [[ ! -x $zig_dir/zig ]]; then
  archive="$PWD/.build/zig-$arch-linux-0.14.1.tar.xz"
  curl -fsSL --retry 3 --proto '=https' --tlsv1.2 \
    "https://ziglang.org/download/0.14.1/zig-$arch-linux-0.14.1.tar.xz" -o "$archive"
  printf '%s  %s\n' "$zig_sha" "$archive" | sha256sum -c -
  tar -xJf "$archive" -C .build
  rm "$archive"
fi
# Zig brings its own C headers and musl; ring's bundled crypto is built statically.
export BHARTA_ZIG="$zig_dir/zig" BHARTA_CC_MUSL="$arch-linux-musl"
export ZIG_GLOBAL_CACHE_DIR="$PWD/.build/zig-cache"
printf '%s\n' '#!/usr/bin/env bash' \
  'exec "${BHARTA_ZIG:?}" cc -target "${BHARTA_CC_HOST:?}" "$@"' > .build/cc-host
# cc-rs passes a Rust triple to clang; Zig uses its own target spelling instead.
printf '%s\n' '#!/usr/bin/env bash' 'args=()' \
  'for arg; do [[ $arg == --target=* ]] || args+=("$arg"); done' \
  'exec "${BHARTA_ZIG:?}" cc -target "${BHARTA_CC_MUSL:?}" "${args[@]}"' > .build/cc-musl
chmod +x .build/cc-host .build/cc-musl
rustc_cmd=(rustc "${cargo_cmd[@]:1}")
host=$("${rustc_cmd[@]}" -vV | sed -n 's/^host: //p')
case $host in
  *-linux-gnu) export BHARTA_CC_HOST="$arch-linux-gnu.2.17" ;;
  *-linux-musl) export BHARTA_CC_HOST="$arch-linux-musl" ;;
  *) printf 'Unsupported Rust host: %s\n' "$host" >&2; exit 1 ;;
esac
host_key=${host^^}; host_key=${host_key//-/_}
target_key=${target^^}; target_key=${target_key//-/_}
export "CARGO_TARGET_${host_key}_LINKER=$PWD/.build/cc-host"
export "CARGO_TARGET_${target_key}_LINKER=rust-lld"
export "CC_${target//-/_}=$PWD/.build/cc-musl"
printf '%s\n' '#!/usr/bin/env bash' 'exec "${BHARTA_ZIG:?}" ar "$@"' > .build/ar
chmod +x .build/ar
export "AR_${target//-/_}=$PWD/.build/ar"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$PWD/.build/target"}
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
"${cargo_cmd[@]}" build --release --locked --target "$target"
mkdir -p dist
cp "$CARGO_TARGET_DIR/$target/release/bharta" dist/bharta.new
mv -f dist/bharta.new dist/bharta
printf '\nBuilt static dist/bharta. Copy that single file to another Linux system of the same architecture.\n'
