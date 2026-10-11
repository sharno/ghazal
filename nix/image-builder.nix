{ runCommand, rustc, stdenv, writeShellApplication, oxmux, busybox, tools, src }:
let
  builder = runCommand "oxmux-image-builder" { nativeBuildInputs = [ rustc stdenv.cc ]; } ''
    mkdir -p $out/bin
    rustc --edition=2021 -O ${src}/tools/image-builder/src/main.rs -o $out/bin/oxmux-image-builder
  '';
in writeShellApplication {
  name = "oxmux-image-builder";
  runtimeInputs = tools;
  text = ''
    export OXMUX_BINARY=${oxmux}/bin/oxmux
    export OXMUX_BUSYBOX=${busybox}/bin/busybox
    exec ${builder}/bin/oxmux-image-builder "$@"
  '';
}
