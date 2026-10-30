# ROCKNIX's distribution source at one pinned commit: H700 kernel patches and
# device trees, U-Boot DRAM patches, panel firmware descriptions and PPSSPP's
# H700 defaults. A GitHub source archive; no ROCKNIX binaries are used.
{ fetchzip }:
fetchzip {
  url = "https://github.com/ROCKNIX/distribution/archive/7cffc96617df2a051af091182e8a3d270fd794a4.tar.gz";
  sha256 = "1irc0blrc42d7x9asshsdm7g2vrjn76lf07xwjx8yqxgv74xfnag";
  passthru.revision = "7cffc96617df2a051af091182e8a3d270fd794a4";
}
