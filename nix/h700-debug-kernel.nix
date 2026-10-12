{ lib, stdenv, fetchurl, fetchzip, buildPackages }:
let
  revision = "7cffc96617df2a051af091182e8a3d270fd794a4";
  hardware = builtins.listToAttrs (map (line:
    let fields = builtins.match "([^=]+)=(.*)" line;
    in { name = builtins.elemAt fields 0; value = builtins.elemAt fields 1; }
  ) (builtins.filter (line: line != "" && !lib.hasPrefix "#" line) (lib.splitString "\n" (builtins.readFile ../hardware.lock))));
  bundle = fetchurl { inherit (hardware) url sha256; };
  earlyFirmware = buildPackages.runCommand "oxmux-h700-early-firmware" {
    nativeBuildInputs = with buildPackages; [ gzip mtools squashfsTools ];
  } ''
    gzip -dc ${bundle} > hardware.img
    # Match the image builder's verified 16 MiB bootloader layout.
    start=$(od -An -tu4 -j454 -N4 hardware.img)
    test "$((start * 512))" -eq 16777216
    mcopy -i hardware.img@@16777216 ::/SYSTEM SYSTEM
    prefix=usr/lib/kernel-overlays/base/lib/firmware
    unsquashfs -no-progress -d extracted SYSTEM \
      "$prefix/rtw88/rtw8821c_fw.bin" \
      "$prefix/rtl_bt/rtl8821cs_config.bin" \
      "$prefix/rtl_bt/rtl8821cs_fw.bin" \
      "$prefix/panels/anbernic,rg40xx-v2-panel.panel"
    mkdir -p $out/rtw88 $out/rtl_bt $out/panels
    for file in rtw88/rtw8821c_fw.bin rtl_bt/rtl8821cs_config.bin \
      rtl_bt/rtl8821cs_fw.bin panels/anbernic,rg40xx-v2-panel.panel; do
      test -s "extracted/$prefix/$file"
      cp "extracted/$prefix/$file" "$out/$file"
    done
  '';
  rocknix = fetchzip {
    url = "https://github.com/ROCKNIX/distribution/archive/${revision}.tar.gz";
    sha256 = "1irc0blrc42d7x9asshsdm7g2vrjn76lf07xwjx8yqxgv74xfnag";
  };
  joypad = fetchurl {
    url = "https://github.com/ROCKNIX/rocknix-joypad/archive/d02ed13aae08113f6f9e0e9d699cb29bb3450fa2.tar.gz";
    sha256 = "89ade1769d6eb7b4f37265f26407a422cdbcf517487f80eb1c29964904ca138e";
  };
in
assert hardware.source_revision == revision;
stdenv.mkDerivation {
  pname = "oxmux-h700-debug-kernel";
  version = "7.2";
  src = fetchurl {
    url = "https://www.kernel.org/pub/linux/kernel/v7.x/linux-7.2.tar.xz";
    sha256 = "f9fef3d14c0df53819026f4be74459835c2a0b0dcbf5b5bbd9ea19f0829402b3";
  };
  nativeBuildInputs = with buildPackages; [ bc bison flex perl python3 openssl pkg-config rsync kmod ];
  depsBuildBuild = [ buildPackages.stdenv.cc ];
  enableParallelBuilding = true;
  hardeningDisable = [ "all" ];
  dontStrip = true;
  dontFixup = true;
  postPatch = ''
    shopt -s nullglob
    base=${rocknix}/projects/ROCKNIX
    for patch in \
      "$base"/packages/linux/patches/*.patch \
      "$base"/packages/linux/patches/aarch64/*.patch \
      "$base"/packages/linux/patches/mainline/*.patch \
      "$base"/packages/linux/patches/H700/*.patch \
      "$base"/packages/linux/patches/default/*.patch \
      "$base"/packages/linux/patches/rocknix-joypad/*.patch \
      "$base"/packages/linux/patches/7.2/*.patch \
      "$base"/devices/H700/patches/linux/*.patch; do
      patch -p1 < "$patch"
    done
    cp -r "$base"/devices/H700/linux/dts/. arch/arm64/boot/dts/
    chmod -R u+w arch/arm64/boot/dts/allwinner
    cat >> arch/arm64/boot/dts/allwinner/Makefile <<'EOF'
    dtb-$(CONFIG_ARCH_SUNXI) += sun50i-h700-anbernic-rg40xx-v-v2-panel.dtb
    EOF
    # Reserve persistent console RAM in this diagnostic DT only. Linux owns
    # neither this region nor its contents; the normal firmware stays untouched.
    cat >> arch/arm64/boot/dts/allwinner/sun50i-h700-anbernic-rg40xx-v-v2-panel.dts <<'EOF'
    / {
      reserved-memory {
        #address-cells = <2>;
        #size-cells = <2>;
        ranges;
        ramoops@7fb00000 {
          compatible = "ramoops";
          reg = <0 0x7fb00000 0 0x100000>;
          record-size = <0x80000>;
          console-size = <0x80000>;
        };
      };
    };
    EOF
    mkdir oxmux-joypad
    tar -xf ${joypad} --strip-components=1 -C oxmux-joypad
    cp "$base"/devices/H700/linux/linux.aarch64.conf .config
    patchShebangs scripts
    scripts/config --enable PM_DEBUG --enable PM_ADVANCED_DEBUG \
      --enable PM_SLEEP_DEBUG --enable PSTORE_RAM --enable PSTORE_CONSOLE \
      --enable PSTORE_PMSG --enable DETECT_HUNG_TASK --enable STACKTRACE \
      --enable FTRACE --enable FUNCTION_TRACER --enable DYNAMIC_DEBUG \
      --set-str INITRAMFS_SOURCE "" \
      --set-str EXTRA_FIRMWARE "rtw88/rtw8821c_fw.bin rtl_bt/rtl8821cs_config.bin rtl_bt/rtl8821cs_fw.bin panels/anbernic,rg40xx-v2-panel.panel" \
      --set-str EXTRA_FIRMWARE_DIR "${earlyFirmware}" \
      --set-str LOCALVERSION "-oxmux-debug" --disable LOCALVERSION_AUTO
  '';
  configurePhase = ''
    runHook preConfigure
    make ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} olddefconfig
    runHook postConfigure
  '';
  buildPhase = ''
    runHook preBuild
    make -j$NIX_BUILD_CORES ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} \
      Image modules dtbs DTC_FLAGS=-@
    make ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} DEVICE=H700 \
      M="$PWD/oxmux-joypad" modules
    runHook postBuild
  '';
  installPhase = ''
    mkdir -p $out/boot $out/build
    cp arch/arm64/boot/Image $out/boot/KERNEL
    cp arch/arm64/boot/dts/allwinner/sun50i-h700-anbernic-rg40xx-v-v2-panel.dtb $out/boot/dtb.img
    cp .config Module.symvers $out/build/
    make ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} INSTALL_MOD_PATH=$out modules_install
    release=$(make -s ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} kernelrelease)
    mkdir -p "$out/lib/modules/$release/extra"
    cp oxmux-joypad/rocknix-singleadc-joypad.ko "$out/lib/modules/$release/extra/"
    depmod -b "$out" "$release"
    find $out/lib/modules -type l -delete
  '';
}
