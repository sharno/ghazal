# One package from ROCKNIX's tree, read from its package.mk: its pinned
# revision and source, unconditional `NAME="value"` fields and patches.
# package.mk is shell; conditionals and hooks are the caller's to restate.
{ lib, fetchurl, fetchgit, rocknix }:
# `path` is relative to projects/ROCKNIX/packages; sources fetched with git
# (a `.git` PKG_URL) have no PKG_SHA256 and take `hash`.
{ path, hash ? null }:
let
  dir = "${rocknix}/projects/ROCKNIX/packages/${path}";
  # A value continued with a trailing backslash is one line, as in shell.
  lines = lib.splitString "\n" (lib.replaceStrings [ "\\\n" ] [ " " ] (builtins.readFile "${dir}/package.mk"));
  field = name:
    let line = lib.findFirst (lib.hasPrefix "${name}=\"") null lines;
    in if line == null then null else lib.removeSuffix "\"" (lib.removePrefix "${name}=\"" line);
  rev = field "PKG_VERSION";
  site = field "PKG_SITE";
  url = lib.replaceStrings [ "\${PKG_SITE}" "\${PKG_VERSION}" ] [ site rev ] (field "PKG_URL");
  # ROCKNIX applies patches/, then patches/<arch>/ and patches/<version>/.
  patchesIn = sub:
    let patches = "${dir}/patches${sub}"; in
    lib.optionals (builtins.pathExists patches) (map (name: "${patches}/${name}")
      (lib.sort lib.lessThan (lib.attrNames (lib.filterAttrs
        (name: type: type == "regular" && lib.hasSuffix ".patch" name) (builtins.readDir patches)))));
in
{
  inherit dir field rev;
  src = if lib.hasSuffix ".git" url
    then fetchgit { url = site; inherit rev hash; fetchSubmodules = true; }
    else fetchurl { inherit url; sha256 = field "PKG_SHA256"; };
  patches = lib.concatMap patchesIn [ "" "/aarch64" "/${rev}" "/${rev}/aarch64" ];
}
