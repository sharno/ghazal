{ runCommand, rustc, stdenv, writeShellApplication, oxmux, kernel, uboot, busybox, tools, src }:
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
    export OXMUX_KERNEL=${kernel}
    export OXMUX_UBOOT=${uboot}/u-boot-sunxi-with-spl.bin
    exec ${builder}/bin/oxmux-image-builder "$@"
  '';
}
