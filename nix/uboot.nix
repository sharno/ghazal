# U-Boot for LPDDR4 H700 handhelds (ROCKNIX calls these "DDR4"), matching the
# pinned ROCKNIX bootloader: mainline v2026.01, its DRAM and AXP717 charge-only
# patches and its defconfig, with TF-A's BL31 for the H616/H700.
# Written to the SD card at 8 KiB:
#   dd if=u-boot-sunxi-with-spl.bin of=/dev/sdX bs=1k seek=8
#
# Boot change from ROCKNIX: only the SD card is tried, so a failed boot isn't
# delayed by PXE/DHCP attempts. booti can also take an LZ4 frame-format kernel,
# but on the RG40XX-V that saved nothing (decompression cost what the smaller
# read saved), so images ship the uncompressed Image.
#
# This exact configuration has booted the handheld. A broken bootloader only
# recovers by rewriting the card from another computer (an earlier build with
# CONFIG_BOOTSTAGE_FDT never started): test changes on a spare card first.
{ lib, stdenv, buildUBoot, fetchFromGitHub, fetchzip, armTrustedFirmwareAllwinnerH616, memory ? "DDR4" }:
let
  rocknix = fetchzip {
    url = "https://github.com/ROCKNIX/distribution/archive/7cffc96617df2a051af091182e8a3d270fd794a4.tar.gz";
    sha256 = "1irc0blrc42d7x9asshsdm7g2vrjn76lf07xwjx8yqxgv74xfnag";
  };
  package = "${rocknix}/projects/ROCKNIX/devices/H700/packages/u-boot-${memory}";
  defconfig = "anbernic_rg35xx_h700_${if memory == "DDR3" then "lpddr3" else "lpddr4"}_defconfig";
  # Compressed-kernel buffer: above the kernel (0x40080000), below the FDT (0x4fa00000).
  bootcommand = lib.replaceStrings [ "\"" ] [ "\\\"" ] (lib.concatMapStrings
    (line: line + (if lib.hasSuffix "then" line || lib.hasSuffix "do" line || line == "else" then " " else "; "))
    (builtins.filter (line: line != "" && !lib.hasPrefix "#" line)
      (map lib.trim (lib.splitString "\n" (builtins.readFile ../config/updates/boot.cmd)))));
in
assert builtins.elem memory [ "DDR3" "DDR4" ];
buildUBoot {
  version = "2026.01";
  src = fetchFromGitHub {
    owner = "u-boot";
    repo = "u-boot";
    rev = "v2026.01";
    hash = "sha256-ym3yM0InFlmZp0zRlLWBmfCKQHPJNGKqm99bdPWOy5E=";
  };
  extraPatches = [
    "${package}/patches/0001-Update-dram_sun50i_h616.c.patch"
    "${package}/patches/0002-sunxi-axp717-charge-only-boot.patch"
  ];
  # buildUBoot owns postPatch (it patches tool shebangs).
  prePatch = ''
    cp ${package}/sources/configs/${defconfig} configs/
  '';
  inherit defconfig;
  extraConfig = ''
    CONFIG_LZ4=y
    CONFIG_ENV_IS_IN_MMC=y
    # CONFIG_ENV_IS_IN_FAT is not set
    # CONFIG_ENV_IS_NOWHERE is not set
    CONFIG_ENV_REDUNDANT=y
    CONFIG_ENV_SIZE=0x20000
    CONFIG_ENV_OFFSET=0x400000
    CONFIG_ENV_OFFSET_REDUND=0x420000
    CONFIG_ENV_MMC_DEVICE_INDEX=0
    CONFIG_ENV_MMC_EMMC_HW_PARTITION=0
    # CONFIG_ENV_MMC_USE_DT is not set
    # CONFIG_ENV_MMC_USE_SW_PARTITION is not set
    CONFIG_CMD_SAVEENV=y
    CONFIG_CMD_SETEXPR=y
    CONFIG_CMD_EXT4=y
    CONFIG_DOS_PARTITION=y
    CONFIG_USE_BOOTCOMMAND=y
    CONFIG_BOOTCOMMAND="${bootcommand}"
  '';
  postConfigure = ''
    make CROSS_COMPILE=${stdenv.cc.targetPrefix} olddefconfig
    # Validate the resolved Kconfig, not just the requested fragment. U-Boot
    # 2026 uses ENV_REDUNDANT; the former SYS_REDUNDAND_ENVIRONMENT is ignored.
    for setting in ENV_IS_IN_MMC=y ENV_REDUNDANT=y ENV_SIZE=0x20000 \
      ENV_OFFSET=0x400000 ENV_OFFSET_REDUND=0x420000 ENV_MMC_DEVICE_INDEX=0 \
      ENV_MMC_EMMC_HW_PARTITION=0; do
      grep -qx "CONFIG_$setting" .config || { echo "Missing boot-state setting: $setting" >&2; exit 1; }
    done
  '';
  extraMeta.platforms = [ "aarch64-linux" ];
  env.BL31 = "${armTrustedFirmwareAllwinnerH616}/bl31.bin";
  filesToInstall = [ "u-boot-sunxi-with-spl.bin" ".config" ];
}
