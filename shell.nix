# nix-shell --run './build.sh --no-install'
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [
    cargo rustc pkg-config gtk4 gtk4-layer-shell fontconfig libxkbcommon
    wayland wayland-protocols meson ninja curl
  ];
}
