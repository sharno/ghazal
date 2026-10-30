# libretro cores by the file name frontend.toml uses. nixpkgs' core packages
# wrap RetroArch, so each is rebuilt with only the core; GL cores target GLES 3
# like ROCKNIX's H700 builds.
{ lib, stdenv, libretro, fetchFromGitHub, mesa, zlib, libpng, libzip, rocknix, ppsspp-libretro }:
let
  core = drv: attrs: let package = drv.overrideAttrs ({ includeRetroArch = false; } // attrs); in
    "${package}${package.libretroCore}/${lib.replaceStrings [ "-" ] [ "_" ] package.core}_libretro.so";
  # Yabause's Saturn fork with its aarch64 dynarec, which nixpkgs lacks.
  yabasanshiro = stdenv.mkDerivation {
    pname = "libretro-yabasanshiro";
    version = "0-unstable-2024-05-23";
    src = fetchFromGitHub {
      owner = "libretro";
      repo = "yabause";
      rev = "39535a6abcad5abf9f71c8b2a7975f005ee12ed6";
      hash = "sha256-V7PNrh9Neys4xVPLs5rzh6rS5icc7mdWluKsnqODx0c=";
    };
    patches = [ "${rocknix}/projects/ROCKNIX/packages/emulators/libretro/yabasanshiro-lr/patches/aarch64/01-optimization.patch" ];
    buildInputs = [ mesa zlib ];
    enableParallelBuilding = true;
    makeFlags = [ "-C" "yabause/src/libretro" "platform=rockpro64" "HAVE_NEON=0" "FORCE_GLES=1" ];
    env.NIX_CFLAGS_COMPILE = "-Wno-error=implicit-function-declaration -Wno-error=incompatible-pointer-types -Wno-error=int-conversion";
    installPhase = "install -Dm644 yabause/src/libretro/yabasanshiro_libretro.so -t $out/lib/libretro";
    meta.license = lib.licenses.gpl2Only;
  };
in
{
  "gambatte_libretro.so" = core libretro.gambatte { };
  "mgba_libretro.so" = core libretro.mgba { };
  "fceumm_libretro.so" = core libretro.fceumm { };
  "snes9x_libretro.so" = core libretro.snes9x { };
  "genesis_plus_gx_libretro.so" = core libretro.genesis-plus-gx { };
  "picodrive_libretro.so" = core libretro.picodrive { };
  "beetle_pce_fast_libretro.so" = core libretro.beetle-pce-fast { };
  "beetle_ngp_libretro.so" = core libretro.beetle-ngp { };
  "beetle_wswan_libretro.so" = core libretro.beetle-wswan { };
  "beetle_vb_libretro.so" = core libretro.beetle-vb { };
  "stella_libretro.so" = core libretro.stella { };
  "handy_libretro.so" = core libretro.handy { };
  "gw_libretro.so" = core libretro.gw { };
  "bluemsx_libretro.so" = core libretro.bluemsx { };
  "fbneo_libretro.so" = core libretro.fbneo { };
  "mame2003_plus_libretro.so" = core libretro.mame2003-plus { };
  # ari64 dynarec and the NEON GPU are the aarch64 defaults.
  "pcsx_rearmed_libretro.so" = core libretro.pcsx-rearmed { };
  # Software renderer with the aarch64 JIT; its OpenGL renderer is x86-only.
  "melonds_libretro.so" = core libretro.melonds { buildInputs = [ zlib ]; };
  "mupen64plus_next_libretro.so" = core libretro.mupen64plus {
    makeFlags = [ "platform=arm64_cortex_a53_gles3" ];
    buildInputs = [ mesa zlib libpng ];
  };
  "flycast_libretro.so" = core libretro.flycast {
    cmakeFlags = map (option: lib.cmakeBool option true) [ "LIBRETRO" "USE_GLES" "USE_OPENMP" "WITH_SYSTEM_ZLIB" ]
      ++ [ (lib.cmakeBool "USE_VULKAN" false) ];
    buildInputs = [ mesa zlib libzip ];
  };
  "yabasanshiro_libretro.so" = "${yabasanshiro}/lib/libretro/yabasanshiro_libretro.so";
  "ppsspp_libretro.so" = "${ppsspp-libretro}/lib/libretro/ppsspp_libretro.so";
}
