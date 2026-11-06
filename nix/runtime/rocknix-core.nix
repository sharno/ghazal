# A libretro core built from ROCKNIX's H700 recipe
# (projects/ROCKNIX/packages/emulators/libretro/<package>/package.mk): its
# pinned revision and source hash, its patches and make or CMake options, and
# ROCKNIX's H700 compiler flags. Bumping the flake's `rocknix` input therefore
# brings ROCKNIX's tested emulator builds. package.mk is shell: its unconditional
# `NAME="value"` lines are read here; the caller restates conditional options
# and its unpack, patch and build hooks, which see $PKG_VERSION as there.
{ lib, stdenv, fetchurl, fetchgit, cmake, rocknix }:
{
  package,
  # The built core, relative to the source root (CMake: its build directory).
  library,
  # Sources fetched with git (a `.git` PKG_URL) have no PKG_SHA256.
  hash ? null,
  builder ? "make",
  # Replaces PKG_MAKE_OPTS_TARGET when it is more than plain options.
  makeFlags ? null,
  extraMakeFlags ? [ ],
  extraCmakeFlags ? [ ],
  cflags ? [ ],
  cxxflags ? [ ],
  ldflags ? [ ],
  # Runs before the patches, as package.mk's post_unpack.
  prePatch ? "",
  preBuild ? "",
  buildInputs ? [ ],
}:
assert lib.elem builder [ "make" "cmake" ];
let
  dir = "${rocknix}/projects/ROCKNIX/packages/emulators/libretro/${package}";
  # A value continued with a trailing backslash is one line, as in shell.
  lines = lib.splitString "\n" (lib.replaceStrings [ "\\\n" ] [ " " ] (builtins.readFile "${dir}/package.mk"));
  field = name:
    let line = lib.findFirst (lib.hasPrefix "${name}=\"") null lines;
    in if line == null then null else lib.removeSuffix "\"" (lib.removePrefix "${name}=\"" line);
  rev = field "PKG_VERSION";
  site = field "PKG_SITE";
  url = lib.replaceStrings [ "\${PKG_SITE}" "\${PKG_VERSION}" ] [ site rev ] (field "PKG_URL");
  options = name:
    let value = lib.defaultTo "" (field name); in
    assert lib.assertMsg (!lib.hasInfix "$" value) "${package}: restate ${name} (${value})";
    lib.filter (option: option != "") (lib.splitString " " value);
  # ROCKNIX applies patches/, then patches/<arch>/ and patches/<version>/.
  patchesIn = sub:
    let path = "${dir}/patches${sub}"; in
    lib.optionals (builtins.pathExists path) (map (name: "${path}/${name}")
      (lib.sort lib.lessThan (lib.attrNames (lib.filterAttrs
        (name: type: type == "regular" && lib.hasSuffix ".patch" name) (builtins.readDir path)))));
  # config/arch.aarch64 and config/optimize for TARGET_CPU=cortex-a53.
  targetFlags = [ "-march=armv8-a+crc+fp+simd" "-mabi=lp64" "-Wno-psabi" "-mtune=cortex-a53" "-O2" "-fomit-frame-pointer" ];
in
stdenv.mkDerivation {
  pname = "libretro-${package}";
  version = builtins.substring 0 7 rev;
  src = if lib.hasSuffix ".git" url
    then fetchgit { url = site; inherit rev hash; fetchSubmodules = true; }
    else fetchurl { inherit url; sha256 = field "PKG_SHA256"; };
  inherit buildInputs prePatch;
  patches = lib.concatMap patchesIn [ "" "/aarch64" "/${rev}" "/${rev}/aarch64" ];
  # Patches name the device and CPU through placeholders.
  patchPhase = ''
    runHook prePatch
    for patch in $patches; do
      sed -e 's#@TARGET_CPU@#cortex-a53#g' -e 's#@DEVICE@#H700#g' "$patch" | patch -p1
    done
    runHook postPatch
  '';
  nativeBuildInputs = lib.optional (builder == "cmake") cmake;
  cmakeFlags = lib.optionals (builder == "cmake") (options "PKG_CMAKE_OPTS_TARGET" ++ extraCmakeFlags);
  dontConfigure = builder == "make";
  makeFlags = lib.optionals (builder == "make")
    ((if makeFlags == null then options "PKG_MAKE_OPTS_TARGET" else makeFlags) ++ extraMakeFlags);
  enableParallelBuilding = true;
  env = {
    PKG_VERSION = rev;
    ARCH = "aarch64";
    CFLAGS = toString (targetFlags ++ cflags);
    CXXFLAGS = toString (targetFlags ++ cxxflags);
    LDFLAGS = toString ([ "-Wl,--as-needed" ] ++ ldflags);
  };
  # ROCKNIX's flags replace nixpkgs' hardening, as in its builds.
  hardeningDisable = [ "all" ];
  # ROCKNIX runs make in a subdirectory when the source has its own configure
  # step; make options such as `-C ..` are relative to it.
  preBuild = ''
    root=$PWD
    ${preBuild}
  '' + lib.optionalString (builder == "make") ''
    if [ -e configure ] || [ -e CMakeLists.txt ] || [ -e meson.build ]; then
      mkdir -p .build && cd .build
    fi
  '';
  installPhase = ''
    runHook preInstall
    install -Dm644 "$root/${library}" $out/lib/libretro/${baseNameOf library}
    runHook postInstall
  '';
}
