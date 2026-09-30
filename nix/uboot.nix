# Mainline U-Boot for H700 boards (anbernic_rg35xx_h700_defconfig, upstream since
# v2025.01) with TF-A's BL31 for the H616/H700. Written to the SD card at 8 KiB:
#   dd if=u-boot-sunxi-with-spl.bin of=/dev/sdX bs=1k seek=8
# The defconfig targets LPDDR4 units; some H700 devices ship LPDDR3 (ROCKNIX builds both).
{ buildUBoot, armTrustedFirmwareAllwinnerH616 }:

buildUBoot {
  defconfig = "anbernic_rg35xx_h700_defconfig";
  extraMeta.platforms = [ "aarch64-linux" ];
  env.BL31 = "${armTrustedFirmwareAllwinnerH616}/bl31.bin";
  filesToInstall = [ "u-boot-sunxi-with-spl.bin" ];
}
