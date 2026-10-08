#!/usr/bin/env bash
# One-command native build. --check never installs or downloads anything.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
install_deps=true
check_only=false
case ${1:-} in
  --check) check_only=true; install_deps=false ;;
  --no-install) install_deps=false ;;
  --install-deps|'') ;;
  --help|-h)
    printf '%s\n' 'Usage: ./build.sh [--check | --no-install | --install-deps]' \
      'Default: install missing system dependencies, provision Rust if necessary,' \
      'build a private layer-shell library if needed, then cargo build --release --locked.' \
      '--check: report all missing prerequisites, without making changes.' \
      '--no-install: build without installing system packages (private downloads still allowed).'
    exit 0 ;;
  *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { printf '%s\n' 'Only one option is accepted.' >&2; exit 2; }

# rustup can already be installed without its proxies being on this shell's PATH.
# Keep discovery local to this build rather than editing shell startup files.
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -x $cargo_bin/rustup ]]; then export PATH="$cargo_bin:$PATH"; fi

# Development headers pull in GTK's complete transitive C dependency set.
distro_id=unknown
distro_like=''
if [[ -r /etc/os-release ]]; then
  # shellcheck source=/dev/null
  source /etc/os-release
  distro_id=${ID:-unknown}
  distro_like=${ID_LIKE:-}
fi
installer=()
packages=()
case " $distro_id $distro_like " in
  *' ubuntu '*|*' debian '*)
    installer=(apt-get install -y --no-install-recommends)
    packages=(build-essential pkg-config libgtk-4-dev libfontconfig1-dev libxkbcommon-dev libwayland-dev wayland-protocols meson ninja-build curl ca-certificates) ;;
  *' fedora '*|*' rhel '*|*' centos '*)
    installer=(dnf install -y)
    packages=(gcc gcc-c++ make pkgconf-pkg-config gtk4-devel fontconfig-devel libxkbcommon-devel wayland-devel wayland-protocols-devel meson ninja-build curl ca-certificates) ;;
  *' arch '*|*' manjaro '*)
    installer=(pacman -S --needed --noconfirm)
    packages=(base-devel pkgconf gtk4 fontconfig libxkbcommon wayland wayland-protocols meson ninja curl ca-certificates) ;;
  *' opensuse '*|*' opensuse-tumbleweed '*|*' opensuse-leap '*|*' suse '*)
    installer=(zypper --non-interactive install)
    packages=(gcc gcc-c++ make pkgconf-pkg-config gtk4-devel fontconfig-devel libxkbcommon-devel wayland-devel wayland-protocols-devel meson ninja curl ca-certificates) ;;
  *' alpine '*)
    installer=(apk add)
    packages=(build-base pkgconf gtk4.0-dev fontconfig-dev libxkbcommon-dev wayland-dev wayland-protocols meson samurai curl ca-certificates) ;;
  *' void '*)
    installer=(xbps-install -Sy)
    packages=(base-devel pkg-config gtk4-devel fontconfig-devel libxkbcommon-devel wayland-devel wayland-protocols meson ninja curl ca-certificates) ;;
esac

rust_ok=false
if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
  read -r _ rust_version _ < <(rustc --version)
  IFS=. read -r rust_major rust_minor _ <<< "$rust_version"
  if ((rust_major > 1 || (rust_major == 1 && rust_minor >= 92))); then rust_ok=true; fi
fi
if ! $rust_ok; then printf '%s\n' 'Missing toolchain: Rust 1.92+ (the build command provisions it automatically).'; fi
prefix="$PWD/.build/native"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"

missing=()
probe() {
  missing=()
  local tool spec
  for tool in cc pkg-config curl tar sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("command: $tool")
  done
  for spec in 'gtk4 >= 4.6' 'fontconfig' 'xkbcommon' 'wayland-client' 'wayland-protocols >= 1.16'; do
    if ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists "$spec"; then
      missing+=("development library: $spec")
    fi
  done
  if ! command -v pkg-config >/dev/null 2>&1 || ! pkg-config --exists 'gtk4-layer-shell-0 >= 1.0'; then
    for tool in meson ninja wayland-scanner; do
      command -v "$tool" >/dev/null 2>&1 || missing+=("command: $tool (private layer-shell build)")
    done
  fi
}
probe
if ((${#missing[@]})); then
  printf 'Missing build prerequisites on %s:\n' "$distro_id"
  printf '  - %s\n' "${missing[@]}"
  if ((${#installer[@]})); then
    printf '\nComplete package install command:\n  sudo'
    printf ' %q' "${installer[@]}" "${packages[@]}"
    printf '\n'
  fi
  if ! $check_only && $install_deps && ((${#installer[@]})); then
    root=()
    if [[ $(id -u) != 0 ]]; then
      command -v sudo >/dev/null || { printf '%s\n' 'Install the listed packages as root; sudo is not available.' >&2; exit 1; }
      root=(sudo)
    fi
    [[ ${installer[0]} != apt-get ]] || "${root[@]}" apt-get update
    "${root[@]}" "${installer[@]}" "${packages[@]}"
    probe
  fi
  if ((${#missing[@]})); then
    printf '\nStill required:\n' >&2
    printf '  - %s\n' "${missing[@]}" >&2
    printf '%s\n' 'NixOS: nix-shell --run "./build.sh --no-install". Other systems: install these prerequisites, then rerun.' >&2
    exit 1
  fi
fi


if ! $rust_ok; then
  printf '%s\n' 'Rust 1.92+ is required; distro-provided Rust may be too old.'
  if $check_only; then exit 1; fi
  if [[ $distro_id == nixos ]]; then
    printf '%s\n' 'Use shell.nix with a current nixpkgs channel providing Rust 1.92+.' >&2
    exit 1
  fi
  mkdir -p .build
  if ! command -v rustup >/dev/null 2>&1; then
    # Install to the invoking user's normal Rust directories, never under sudo.
    curl --fail --location --show-error --silent --proto '=https' --tlsv1.2 \
      https://sh.rustup.rs -o .build/rustup-init.sh
    sh .build/rustup-init.sh -y --profile minimal --default-toolchain 1.92.0 --no-modify-path
  else
    rustup toolchain install 1.92.0 --profile minimal
  fi
  export PATH="$cargo_bin:$PATH"
  cargo_cmd=(cargo +1.92.0)
else
  cargo_cmd=(cargo)
fi

private_layer=false
if ! pkg-config --exists 'gtk4-layer-shell-0 >= 1.0'; then
  printf '%s\n' 'GTK4 layer-shell is unavailable; building pinned v1.0.4 privately (no system install).'
  if $check_only; then exit 1; fi
  mkdir -p .build
  archive=.build/gtk4-layer-shell-1.0.4.tar.gz
  checksum=7fe327dc3740e4b6f5edfd855e23f84b1ac1ec6854b731047b95df7feb46498b
  if [[ ! -f $archive ]]; then
    curl --fail --location --show-error --silent --proto '=https' --tlsv1.2 \
      https://codeload.github.com/wmww/gtk4-layer-shell/tar.gz/refs/tags/v1.0.4 -o "$archive.tmp"
    mv -- "$archive.tmp" "$archive"
  fi
  actual_checksum=$(sha256sum "$archive")
  [[ ${actual_checksum%% *} == "$checksum" ]] || {
    printf '%s\n' 'Layer-shell source checksum failed. Remove the cached archive and rerun.' >&2; exit 1;
  }
  tar -xzf "$archive" -C .build
  setup_options=()
  [[ ! -f .build/layer-shell-build/meson-private/coredata.dat ]] || setup_options=(--wipe)
  meson setup .build/layer-shell-build .build/gtk4-layer-shell-1.0.4 \
    --prefix="$prefix" --libdir=lib --buildtype=release \
    -Dintrospection=false -Dvapi=false -Ddocs=false -Dexamples=false -Dtests=false "${setup_options[@]}"
  meson compile -C .build/layer-shell-build
  meson install -C .build/layer-shell-build --no-rebuild
fi
[[ ! -f $prefix/lib/pkgconfig/gtk4-layer-shell-0.pc ]] || private_layer=true
if $check_only; then printf '%s\n' 'All native build prerequisites are available.'; exit 0; fi

# Only the private layer-shell library is bundled; GTK stays managed by the distro.
# The relative runpath also works when the whole release directory is copied.
if $private_layer; then
  if [[ -v CARGO_ENCODED_RUSTFLAGS ]]; then
    export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+$CARGO_ENCODED_RUSTFLAGS$'\x1f'}-C"$'\x1f'"link-arg=-Wl,-rpath,\$ORIGIN/lib"
  else
    export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath,\$ORIGIN/lib"
  fi
fi
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
"${cargo_cmd[@]}" build --release --locked
release_dir="${CARGO_TARGET_DIR:-target}/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release"
if $private_layer; then
  mkdir -p "$release_dir/lib"
  cp -a "$prefix"/lib/libgtk4-layer-shell.so* "$release_dir/lib/"
fi
printf '\nBuilt %s/bharta. Run it with --dark --all-outputs.\n' "$release_dir"
