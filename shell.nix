# nix-shell --run './build.sh'
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ rustup curl xz ];
}
