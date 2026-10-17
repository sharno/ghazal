{ lib, rustPlatform, writeShellApplication, bash, ghazal, kernel, uboot, uboot-ddr3, busybox, tools, src, cacert,
  update-runtime, rauc, curl, ubootTools }:
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
    export GHAZAL_UPDATE_CLOSURE=${update-runtime}/store-paths
    export GHAZAL_RAUC=${rauc}/bin/rauc
    export GHAZAL_CURL=${curl}/bin/curl
    export GHAZAL_FW_PRINTENV=${ubootTools}/bin/fw_printenv
    export GHAZAL_FW_SETENV=${ubootTools}/bin/fw_setenv
    export GHAZAL_CA_CERT=${cacert}/etc/ssl/certs/ca-bundle.crt
    exec ${builder}/bin/ghazal-image-builder "$@"
  '';
}
