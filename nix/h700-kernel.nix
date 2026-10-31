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
{ lib, stdenv, fetchurl, buildPackages, rocknix, debug ? false }:
let
  # Wi-Fi, cfg80211 and the panel driver are built in and request firmware
  # before the root filesystem exists, so their firmware is embedded in the
  # kernel. Without its regulatory database cfg80211 falls back to the
  # restrictive world domain.
  devices = (lib.importTOML ../config/h700/devices.toml).devices;
  panels = map (name: "panels/anbernic,${name}-panel.panel") [
    "rg28xx" "rg34xx" "rg34xx-sp" "rg34xx-sp-v2" "rg35xx-plus"
    "rg35xx-plus-rev6" "rg35xx-sp-v2" "rg40xx" "rg40xx-v2" "rgcubexx"
  ];
  firmware = [ "rtw88/rtw8821c_fw.bin" "regulatory.db" "regulatory.db.p7s" ] ++ panels;
  earlyFirmware = buildPackages.runCommand "ghazal-h700-early-firmware" { } ''
    install -Dm644 ${buildPackages.linux-firmware}/lib/firmware/rtw88/rtw8821c_fw.bin -t $out/rtw88
    install -Dm644 ${buildPackages.wireless-regdb}/lib/firmware/regulatory.db{,.p7s} -t $out
    for panel in ${lib.concatStringsSep " " panels}; do
      install -Dm644 "${rocknix}/projects/ROCKNIX/packages/linux-firmware/kernel-firmware/extra-firmware/$panel" "$out/$panel"
    done
  '';
  joypad = fetchurl {
    url = "https://github.com/ROCKNIX/rocknix-joypad/archive/d02ed13aae08113f6f9e0e9d699cb29bb3450fa2.tar.gz";
    sha256 = "89ade1769d6eb7b4f37265f26407a422cdbcf517487f80eb1c29964904ca138e";
  };
  # Subsystems the handheld never uses. ROCKNIX's config targets many devices.
  # With modules disabled, ROCKNIX's `=m` options would become built in, so
  # every unwanted one is listed here. U-Boot reads the Image at ~21 MB/s, so
  # each MB removed saves ~50 ms of every boot.
  unused = [
    # Bluetooth is not exposed by the frontend; hci_uart firmware upload alone costs ~0.6 s.
    "BT"
    "VIRTUALIZATION" "KVM" "PCI" "SCSI" "ATA" "MEDIA_SUPPORT" "NFS_FS" "NFSD"
    "BTRFS_FS" "F2FS_FS" "NTFS3_FS" "OVERLAY_FS" "NETFILTER" "BRIDGE"
    "NET_DSA" "WIREGUARD" "TUN" "VETH" "NLMON" "USB_ACM" "USB_WDM"
    "ZRAM" "ZSMALLOC" "CRYPTO_DEV_SUN8I_CE" "CRYPTO_DEV_SUN8I_SS"
    "WLAN_VENDOR_ATH" "WLAN_VENDOR_ATMEL" "WLAN_VENDOR_BROADCOM" "WLAN_VENDOR_INTEL"
    "WLAN_VENDOR_INTERSIL" "WLAN_VENDOR_MARVELL" "WLAN_VENDOR_MEDIATEK"
    "WLAN_VENDOR_MICROCHIP" "WLAN_VENDOR_PURELIFI" "WLAN_VENDOR_QUANTENNA"
    "WLAN_VENDOR_RALINK" "WLAN_VENDOR_RSI" "WLAN_VENDOR_SILABS" "WLAN_VENDOR_ST"
    "WLAN_VENDOR_TI"
    # No NAND/NOR flash, RAID, network filesystems or optical media; RAID
    # autodetection also waits for every device to probe before mounting root.
    "MTD" "BLK_DEV_MD" "NET_9P" "9P_FS" "UDF_FS" "ISO9660_FS" "EXT2_FS" "AUTOFS_FS"
    # No eBPF, io_uring, NUMA, hugetlbfs or same-page merging users, and no
    # cgroup controllers (SCHED_AUTOGROUP keeps per-session CPU fairness).
    "BPF_SYSCALL" "BPF_JIT" "IO_URING" "NUMA" "HUGETLBFS" "KSM" "MEMCG" "BLK_CGROUP"
    "CGROUP_PIDS" "CGROUP_FREEZER" "CPUSETS" "CGROUP_DEVICE" "CGROUP_CPUACCT"
    "CGROUP_PERF" "SECURITY" "IKCONFIG" "DEBUG_FS" "SLUB_DEBUG" "COMPAT"
    # Symbol tables are ~1.2 MB; build/System.map decodes addresses in traces.
    "KALLSYMS"
    # Network block devices and unused device-mapper targets (RAUC needs verity).
    "BLK_DEV_NBD" "DM_MIRROR" "DM_ZERO"
    # Uncore and trace PMUs; the CPU PMU stays.
    "ARM_CCI_PMU" "ARM_DSU_PMU" "ARM_SPE_PMU" "ARM_CORESIGHT_PMU_ARCH_SYSTEM_PMU"
    # Gadget functions; the OTG port only needs the gadget core (see below).
    "USB_CONFIGFS"
    # Algorithms nothing here uses: iwd needs AES, ARC4, DES, MD4/5, SHA and
    # HMAC/CMAC/CCM/GCM through AF_ALG, dm-verity SHA-256. The jitter entropy
    # self-test alone took 60 ms of boot.
    "CRYPTO_USER" "CRYPTO_KRB5" "CRYPTO_KRB5ENC" "CRYPTO_CAMELLIA" "CRYPTO_SM4"
    "CRYPTO_SM4_GENERIC" "CRYPTO_SM3" "CRYPTO_BLAKE2B" "CRYPTO_SHA3" "CRYPTO_XXHASH"
    "CRYPTO_DH" "CRYPTO_ECDH" "CRYPTO_ECC" "CRYPTO_DEFLATE" "CRYPTO_LZO" "CRYPTO_LZ4"
    "CRYPTO_KDF800108_CTR" "CRYPTO_DRBG_MENU" "CRYPTO_DRBG" "CRYPTO_JITTERENTROPY"
    "CRYPTO_ECHAINIV" "CRYPTO_USER_API_RNG" "KEY_DH_OPERATIONS"
    # Suspend to disk: there is no swap to resume from.
    "HIBERNATION"
    # Traffic control, ethtool netlink and wireless extensions: iwd uses nl80211.
    "NET_SCHED" "ETHTOOL_NETLINK" "CFG80211_WEXT"
    # zram is gone; squashfs only decompresses.
    "CRYPTO_ZSTD"
    # Other SoCs and buses: SCMI, CryptoCell, Qualcomm MHI, IOMMUFD, Type-C,
    # non-8250 UARTs, PS/2 mice, MIDI and the i.MX HDMI bridge.
    "ARM_SCMI_PROTOCOL" "CRYPTO_DEV_CCREE" "MHI_BUS" "IOMMUFD" "TYPEC"
    "SERIAL_AMBA_PL011" "SERIAL_XILINX_PS_UART" "SERIAL_FSL_LPUART"
    "SERIAL_FSL_LINFLEXUART" "INPUT_MOUSE" "SND_SEQUENCER" "DRM_ITE_IT6263"
    # Every catalog panel uses panel-mipi-dpi-spi. USB_GADGET stays: the OTG
    # port's dr_mode = "otg" fails to probe in a host-only MUSB build.
    "DRM_PANEL_SIMPLE"
    # Clock and pin controllers of other Allwinner SoCs; the H616/H700 keeps
    # its own, the H6 R_CCU and the display engine's.
    "SUN4I_A10_CCU" "SUN20I_D1_CCU" "SUN20I_D1_R_CCU" "SUN50I_A64_CCU"
    "SUN50I_A100_CCU" "SUN50I_A100_R_CCU" "SUN50I_H6_CCU" "SUN55I_A523_CCU"
    "SUN55I_A523_MCU_CCU" "SUN55I_A523_R_CCU" "SUN6I_A31_CCU" "SUN8I_A23_CCU"
    "SUN8I_A33_CCU" "SUN8I_A83T_CCU" "SUN8I_H3_CCU" "SUN8I_V3S_CCU" "SUN8I_R40_CCU"
    "SUN9I_A80_CCU" "SUN8I_R_CCU" "REGULATOR_SUN20I"
    "PINCTRL_SUN4I_A10" "PINCTRL_SUN5I" "PINCTRL_SUN6I_A31" "PINCTRL_SUN6I_A31_R"
    "PINCTRL_SUN8I_A23" "PINCTRL_SUN8I_A33" "PINCTRL_SUN8I_A83T" "PINCTRL_SUN8I_A83T_R"
    "PINCTRL_SUN8I_A23_R" "PINCTRL_SUN8I_H3" "PINCTRL_SUN8I_H3_R" "PINCTRL_SUN8I_V3S"
    "PINCTRL_SUN9I_A80" "PINCTRL_SUN9I_A80_R" "PINCTRL_SUN20I_D1" "PINCTRL_SUN50I_A64"
    "PINCTRL_SUN50I_A64_R" "PINCTRL_SUN50I_A100" "PINCTRL_SUN50I_A100_R"
    "PINCTRL_SUN50I_H5" "PINCTRL_SUN50I_H6" "PINCTRL_SUN50I_H6_R"
    "PINCTRL_SUN55I_A523" "PINCTRL_SUN55I_A523_R"
  ];
  # 4 cores; a 64 KiB log holds a whole boot (the kernel-log service persists it).
  # A kernel or init hang resets the board unless userspace opens the watchdog
  # within a minute (see sunxi-wdt-start-at-probe.patch).
  # Loop devices are created on demand (RAUC). fbcon takes over the display
  # only when the console is first written: otherwise powering the panel on
  # (~0.3 s) runs inside the probe pass that root mounting waits for.
  # A panic draws the log tail over the current frame (DRM_PANIC), so a photo
  # of the screen is a bug report; U-Boot binds fbcon early on retried boots.
  # Without utilization clamping, schedutil runs the top clock (1.1 V on the
  # RG40XX-V) whenever a realtime task is runnable; system.toml zeroes the RT
  # floor so games stay on the 0.9 V steps.
  values = { NR_CPUS = "4"; LOG_BUF_SHIFT = "16"; WATCHDOG_OPEN_TIMEOUT = "60"; BLK_DEV_LOOP_MIN_COUNT = "0"; };
  strings = { DRM_PANIC_SCREEN = "kmsg"; };
  # HDMI sound: the audio hub driver (ROCKNIX's vendor port, built in so it
  # needs no module loading) and configfs overlays. The hub's device-tree
  # nodes ship disabled; ghazal applies nix/h700-hdmi-audio-overlay.patch's
  # overlay when a game first goes to HDMI, so the card cannot disturb boot
  # or the codec's card number.
  enabled = [ "FRAMEBUFFER_CONSOLE_DEFERRED_TAKEOVER" "DRM_PANIC" "UCLAMP_TASK" "OF_CONFIGFS" "SND_SOC_SUNXI_AHUB" ];
  # Former modules the device does use: iwd needs these AF_ALG algorithms.
  required = [ "CRYPTO_USER_API_AEAD" "CRYPTO_ARC4" "HID_PLAYSTATION"
    "MSDOS_PARTITION" "BLK_DEV_LOOP" "MD" "BLK_DEV_DM" "DM_VERITY"
    "SQUASHFS" "SQUASHFS_ZSTD" "SQUASHFS_ZLIB" "CRYPTO_SHA256"
    "WATCHDOG" "SUNXI_WATCHDOG" ];
  debugOptions = [
    "PM_DEBUG" "PM_ADVANCED_DEBUG" "PM_SLEEP_DEBUG" "PSTORE_RAM" "PSTORE_CONSOLE"
    "PSTORE_PMSG" "DETECT_HUNG_TASK" "STACKTRACE" "FTRACE" "FUNCTION_TRACER" "DYNAMIC_DEBUG"
    "DEBUG_FS" "KALLSYMS" "KALLSYMS_ALL" "DRM_PANIC_DEBUG"
  ];
  flags = prefix: names: lib.concatMapStringsSep " " (name: "${prefix} ${name}") names;
in
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
      "$base"/devices/H700/patches/linux/0203-sound-soc-Add-sunxi_v2-for-h616-ahub.patch.disabled \
      ${./sun8i-opaque-bottom-plane.patch} \
      ${./axp717-power-reporting.patch} \
      ${./sun4i-rgb-panel-orientation.patch} \
      ${./sunxi-wdt-start-at-probe.patch} \
      ${./h700-charge-current.patch} \
      ${./serial-8250-fifo-size.patch} \
      ${./sunxi-mmc-pwrseq-async.patch} \
      ${./sun8i-panic-scanout.patch} \
      ${./sun4i-usb-phy-typec-source.patch} \
      ${./sunxi-ahub-7.2.patch} \
      ${./of-overlay-configfs.patch} \
      ${./h700-hdmi-audio-overlay.patch}; do
      patch -p1 < "$patch"
    done
    cp -r "$base"/devices/H700/linux/dts/. arch/arm64/boot/dts/
    chmod -R u+w arch/arm64/boot/dts/allwinner
    for tree in ${lib.concatMapStringsSep " " (device: device.dtb) devices}; do
      grep -q "$tree\\.dtb" arch/arm64/boot/dts/allwinner/Makefile || \
        echo "dtb-\$(CONFIG_ARCH_SUNXI) += $tree.dtb" >> arch/arm64/boot/dts/allwinner/Makefile
    done
    echo 'dtb-$(CONFIG_ARCH_SUNXI) += ghazal-h616-hdmi-audio.dtbo' >> arch/arm64/boot/dts/allwinner/Makefile
  '' + lib.optionalString debug ''
    # Reserve persistent console RAM in this diagnostic DT only. Linux owns
    # neither this region nor its contents; the normal firmware stays untouched.
    cat >> arch/arm64/boot/dts/allwinner/sun50i-h700-anbernic-rg40xx-v-v2-panel.dts <<'EOF'
    / {
      reserved-memory {
        #address-cells = <2>;
        #size-cells = <2>;
        ranges;
        ramoops@5f000000 {
          compatible = "ramoops";
          reg = <0 0x5f000000 0 0x100000>;
          record-size = <0x80000>;
          console-size = <0x80000>;
          ecc-size = <64>;
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
    scripts/config --disable MODULES ${flags "--disable" unused} ${flags "--enable" (required ++ enabled)} \
      ${lib.optionalString debug (flags "--enable" debugOptions)} \
      ${lib.concatStringsSep " " (lib.mapAttrsToList (name: value: "--set-val ${name} ${value}") values)} \
      ${lib.concatStringsSep " " (lib.mapAttrsToList (name: value: "--set-str ${name} ${value}") strings)} \
      --set-str INITRAMFS_SOURCE "" \
      --set-str EXTRA_FIRMWARE "${lib.concatStringsSep " " firmware}" \
      --set-str EXTRA_FIRMWARE_DIR "${earlyFirmware}" \
      --set-str LOCALVERSION "-ghazal${lib.optionalString debug "-debug"}" --disable LOCALVERSION_AUTO
  '';
  configurePhase = ''
    runHook preConfigure
    make ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} olddefconfig
    # Fail rather than silently ship a kernel missing a required driver.
    for option in ${lib.concatStringsSep " " (required ++ enabled ++ [ "DEVTMPFS_MOUNT" "INPUT_JOYSTICK" "DRM_SUN4I" "DRM_PANFROST" "RTW88_8821CS" "SND_SUN4I_CODEC" "EXT4_FS" "EXFAT_FS" "VFAT_FS" ])}; do
      grep -qx "CONFIG_$option=y" .config || { echo "CONFIG_$option is not built in"; exit 1; }
    done
    for setting in ${lib.concatStringsSep " " (lib.mapAttrsToList (name: value: "${name}=${value}") values ++ lib.mapAttrsToList (name: value: "'${name}=\"${value}\"'") strings)}; do
      grep -qx "CONFIG_$setting" .config || { echo "CONFIG_$setting did not apply"; exit 1; }
    done
    runHook postConfigure
  '';
  buildPhase = ''
    runHook preBuild
    make -j$NIX_BUILD_CORES ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} Image dtbs DTC_FLAGS=-@
    runHook postBuild
  '';
  installPhase = ''
    mkdir -p $out/boot/device_trees $out/boot/overlays $out/build
    cp arch/arm64/boot/dts/allwinner/ghazal-h616-hdmi-audio.dtbo $out/boot/overlays/ghazal-hdmi-audio.dtbo
    cp arch/arm64/boot/Image $out/boot/KERNEL
    cp arch/arm64/boot/dts/allwinner/sun50i-h700-anbernic-rg40xx-v-v2-panel.dtb $out/boot/dtb.img
    for tree in ${lib.concatMapStringsSep " " (device: device.dtb) devices}; do
      cp "arch/arm64/boot/dts/allwinner/$tree.dtb" $out/boot/device_trees/
    done
    cp .config System.map $out/build/
    make -s ARCH=arm64 CROSS_COMPILE=${stdenv.cc.targetPrefix} kernelrelease > $out/build/kernelrelease
  '';
}
