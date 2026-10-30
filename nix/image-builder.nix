{ lib, rustPlatform, writeShellApplication, bash, ghazal, kernel, uboot, uboot-ddr3, busybox, tools, src,
  runtime, sources }:
let
  builder = rustPlatform.buildRustPackage {
    pname = "ghazal-image-builder";
    version = "0.1.0";
    src = lib.cleanSource src;
    cargoRoot = "tools/image-builder";
    buildAndTestSubdir = "tools/image-builder";
    cargoLock.lockFile = ../tools/image-builder/Cargo.lock;
  };
in writeShellApplication {
  name = "ghazal-image-builder";
  runtimeInputs = tools ++ [ bash ];
  text = ''
    export GHAZAL_BINARY=${ghazal}/bin/ghazal
    export GHAZAL_BUSYBOX=${busybox}/bin/busybox
    export GHAZAL_KERNEL=${kernel}
    export GHAZAL_UBOOT=${uboot}/u-boot-sunxi-with-spl.bin
    export GHAZAL_UBOOT_DDR3=${uboot-ddr3}/u-boot-sunxi-with-spl.bin
    export GHAZAL_BUILDER_SHELL=${bash}/bin/bash
    export GHAZAL_RUNTIME=${runtime.tree}
    export GHAZAL_RUNTIME_CLOSURE=${runtime.closure}/store-paths
    export GHAZAL_PPSSPP_DEFAULTS=${runtime.ppsspp-defaults}
    export GHAZAL_SOURCES='${builtins.toJSON sources}'
    exec ${builder}/bin/ghazal-image-builder "$@"
  '';
}
