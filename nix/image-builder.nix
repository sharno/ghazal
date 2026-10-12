{ lib, rustPlatform, writeShellApplication, bash, oxmux, kernel, uboot, uboot-ddr3, busybox, tools, src }:
let
  builder = rustPlatform.buildRustPackage {
    pname = "oxmux-image-builder";
    version = "0.1.0";
    src = lib.cleanSource src;
    cargoRoot = "tools/image-builder";
    buildAndTestSubdir = "tools/image-builder";
    cargoLock.lockFile = ../tools/image-builder/Cargo.lock;
  };
in writeShellApplication {
  name = "oxmux-image-builder";
  runtimeInputs = tools ++ [ bash ];
  text = ''
    export OXMUX_BINARY=${oxmux}/bin/oxmux
    export OXMUX_BUSYBOX=${busybox}/bin/busybox
    export OXMUX_KERNEL=${kernel}
    export OXMUX_UBOOT=${uboot}/u-boot-sunxi-with-spl.bin
    export OXMUX_UBOOT_DDR3=${uboot-ddr3}/u-boot-sunxi-with-spl.bin
    export OXMUX_BUILDER_SHELL=${bash}/bin/bash
    exec ${builder}/bin/oxmux-image-builder "$@"
  '';
}
