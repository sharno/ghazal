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
{ lib, buildUBoot, fetchFromGitHub, fetchzip, armTrustedFirmwareAllwinnerH616 }:
let
  rocknix = fetchzip {
    url = "https://github.com/ROCKNIX/distribution/archive/7cffc96617df2a051af091182e8a3d270fd794a4.tar.gz";
    sha256 = "1irc0blrc42d7x9asshsdm7g2vrjn76lf07xwjx8yqxgv74xfnag";
  };
  package = "${rocknix}/projects/ROCKNIX/devices/H700/packages/u-boot-DDR4";
  defconfig = "anbernic_rg35xx_h700_lpddr4_defconfig";
  # Compressed-kernel buffer: above the kernel (0x40080000), below the FDT (0x4fa00000).
  bootcommand = lib.concatStringsSep "; " [
    "env set kernel_comp_addr_r 0x46000000"
    "env set kernel_comp_size 0x4000000"
    "env set boot_targets mmc0"
    "run distro_bootcmd"
  ];
in
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
    CONFIG_USE_BOOTCOMMAND=y
    CONFIG_BOOTCOMMAND="${bootcommand}"
  '';
  extraMeta.platforms = [ "aarch64-linux" ];
  env.BL31 = "${armTrustedFirmwareAllwinnerH616}/bl31.bin";
  filesToInstall = [ "u-boot-sunxi-with-spl.bin" ];
}
