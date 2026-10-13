# RG40XX-V kernel built from the pinned ROCKNIX patch set and H700 config.
#
# The production kernel boots straight into the root filesystem: every driver
# the handheld uses is built in (including the joypad), so there is no
# initramfs, no module loading and no module/kernel version coupling on the
# root filesystem. A short fragment removes subsystems this device never uses;
# a smaller Image loads faster from the SD card.
#
# `debug = true` adds PM/tracing/ramoops for suspend diagnostics; see
# docs/kernel-debugging.md.
{ lib, stdenv, fetchurl, fetchzip, buildPackages, debug ? false }:
let
  revision = "7cffc96617df2a051af091182e8a3d270fd794a4";
  hardware = builtins.listToAttrs (map (line:
    let fields = builtins.match "([^=]+)=(.*)" line;
    in { name = builtins.elemAt fields 0; value = builtins.elemAt fields 1; }
  ) (builtins.filter (line: line != "" && !lib.hasPrefix "#" line) (lib.splitString "\n" (builtins.readFile ../hardware.lock))));
  bundle = fetchurl { inherit (hardware) url sha256; };
  # Wi-Fi and the panel driver are built in and request firmware before the
  # root filesystem exists, so their firmware is embedded in the kernel.
  devices = (lib.importTOML ../config/h700/devices.toml).devices;
  firmware = [ "rtw88/rtw8821c_fw.bin" ] ++ map (name: "panels/anbernic,${name}-panel.panel") [
    "rg28xx" "rg34xx" "rg34xx-sp" "rg34xx-sp-v2" "rg35xx-plus"
    "rg35xx-plus-rev6" "rg35xx-sp-v2" "rg40xx" "rg40xx-v2" "rgcubexx"
  ];
  earlyFirmware = buildPackages.runCommand "ghazal-h700-early-firmware" {
    nativeBuildInputs = with buildPackages; [ gzip mtools squashfsTools ];
  } ''
    gzip -dc ${bundle} > hardware.img
    # Match the image builder's verified 16 MiB bootloader layout.
    start=$(od -An -tu4 -j454 -N4 hardware.img)
    test "$((start * 512))" -eq 16777216
    mcopy -i hardware.img@@16777216 ::/SYSTEM SYSTEM
    prefix=usr/lib/kernel-overlays/base/lib/firmware
    unsquashfs -no-progress -d extracted SYSTEM ${lib.concatMapStringsSep " " (f: "\"$prefix/${f}\"") firmware}
    for file in ${lib.concatStringsSep " " firmware}; do
      test -s "extracted/$prefix/$file"
      mkdir -p "$out/$(dirname "$file")"
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
  # Subsystems the handheld never uses. ROCKNIX's config targets many devices.
  # With modules disabled, ROCKNIX's `=m` options would become built in, so
  # every unwanted one is listed here.
  unused = [
    # Bluetooth is not exposed by the frontend; hci_uart firmware upload alone costs ~0.6 s.
    "BT"
    "VIRTUALIZATION" "KVM" "PCI" "SCSI" "ATA" "MEDIA_SUPPORT" "NFS_FS" "NFSD"
    "BTRFS_FS" "F2FS_FS" "NTFS3_FS" "SQUASHFS" "OVERLAY_FS" "NETFILTER" "BRIDGE"
    "NET_DSA" "WIREGUARD" "TUN" "VETH" "NLMON" "USB_ACM" "USB_WDM"
    "ZRAM" "ZSMALLOC" "CRYPTO_DEV_SUN8I_CE" "CRYPTO_DEV_SUN8I_SS"
    "WLAN_VENDOR_ATH" "WLAN_VENDOR_ATMEL" "WLAN_VENDOR_BROADCOM" "WLAN_VENDOR_INTEL"
    "WLAN_VENDOR_INTERSIL" "WLAN_VENDOR_MARVELL" "WLAN_VENDOR_MEDIATEK"
    "WLAN_VENDOR_MICROCHIP" "WLAN_VENDOR_PURELIFI" "WLAN_VENDOR_QUANTENNA"
    "WLAN_VENDOR_RALINK" "WLAN_VENDOR_RSI" "WLAN_VENDOR_SILABS" "WLAN_VENDOR_ST"
    "WLAN_VENDOR_TI"
  ];
  # Former modules the device does use: iwd needs these AF_ALG algorithms.
  required = [ "CRYPTO_USER_API_AEAD" "CRYPTO_ARC4" "HID_PLAYSTATION" ];
  debugOptions = [
    "PM_DEBUG" "PM_ADVANCED_DEBUG" "PM_SLEEP_DEBUG" "PSTORE_RAM" "PSTORE_CONSOLE"
    "PSTORE_PMSG" "DETECT_HUNG_TASK" "STACKTRACE" "FTRACE" "FUNCTION_TRACER" "DYNAMIC_DEBUG"
  ];
  flags = prefix: names: lib.concatMapStringsSep " " (name: "${prefix} ${name}") names;
in
assert hardware.source_revision == revision;
stdenv.mkDerivation {
  pname = "ghazal-h700-kernel" + lib.optionalString debug "-debug";
  version = "7.2";
  src = fetchurl {
    url = "https://www.kernel.org/pub/linux/kernel/v7.x/linux-7.2.tar.xz";
    sha256 = "f9fef3d14c0df53819026f4be74459835c2a0b0dcbf5b5bbd9ea19f0829402b3";
  };
  nativeBuildInputs = with buildPackages; [ bc bison flex perl python3 openssl pkg-config rsync kmod ];
  depsBuildBuild = [ buildPackages.stdenv.cc ];
  enableParallelBuilding = true;
  # Reproducible version banner: no build time, user or host.
  env = { KBUILD_BUILD_TIMESTAMP = "@1"; KBUILD_BUILD_USER = "ghazal"; KBUILD_BUILD_HOST = "nix"; };
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
      "$base"/devices/H700/patches/linux/*.patch \
      ${./sun8i-opaque-bottom-plane.patch}; do
      patch -p1 < "$patch"
    done
    cp -r "$base"/devices/H700/linux/dts/. arch/arm64/boot/dts/
    chmod -R u+w arch/arm64/boot/dts/allwinner
    for tree in ${lib.concatMapStringsSep " " (device: device.dtb) devices}; do
      grep -q "$tree\\.dtb" arch/arm64/boot/dts/allwinner/Makefile || \
        echo "dtb-\$(CONFIG_ARCH_SUNXI) += $tree.dtb" >> arch/arm64/boot/dts/allwinner/Makefile
    done
  '' + lib.optionalString debug ''
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
  '' + ''
    # Build the joypad driver into the kernel; it is needed before the menu.
    tar -xf ${joypad} --strip-components=1 -C drivers/input/joystick \
      --wildcards '*/rocknix-singleadc-joypad.c' '*/rocknix-joypad.h'
    echo 'obj-y += rocknix-singleadc-joypad.o' >> drivers/input/joystick/Makefile
    cp "$base"/devices/H700/linux/linux.aarch64.conf .config
    patchShebangs scripts
    scripts/config --disable MODULES ${flags "--disable" unused} ${flags "--enable" required} \
      ${lib.optionalString debug (flags "--enable" debugOptions)} \
      --set-str INITRAMFS_SOURCE "" \
      --set-str EXTRA_FIRMWARE "${lib.concatStringsSep " " firmware}" \
      --set-str EXTRA_FIRMWARE_DIR "${earlyFirmware}" \
      --set-str LOCALVERSION "-ghazal${lib.optionalString debug "-debug"}" --disable LOCALVERSION_AUTO
  '';
  configurePhase = ''
    runHook preConfigure
    make ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} olddefconfig
    # Fail rather than silently ship a kernel missing a required driver.
    for option in ${lib.concatStringsSep " " (required ++ [ "DEVTMPFS_MOUNT" "INPUT_JOYSTICK" "DRM_SUN4I" "DRM_PANFROST" "RTW88_8821CS" "SND_SUN4I_CODEC" "EXT4_FS" "EXFAT_FS" "VFAT_FS" ])}; do
      grep -qx "CONFIG_$option=y" .config || { echo "CONFIG_$option is not built in"; exit 1; }
    done
    runHook postConfigure
  '';
  buildPhase = ''
    runHook preBuild
    make -j$NIX_BUILD_CORES ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} Image dtbs DTC_FLAGS=-@
    runHook postBuild
  '';
  installPhase = ''
    mkdir -p $out/boot/device_trees $out/build
    cp arch/arm64/boot/Image $out/boot/KERNEL
    cp arch/arm64/boot/dts/allwinner/sun50i-h700-anbernic-rg40xx-v-v2-panel.dtb $out/boot/dtb.img
    for tree in ${lib.concatMapStringsSep " " (device: device.dtb) devices}; do
      cp "arch/arm64/boot/dts/allwinner/$tree.dtb" $out/boot/device_trees/
    done
    cp .config $out/build/
    make -s ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} kernelrelease > $out/build/kernelrelease
  '';
}
