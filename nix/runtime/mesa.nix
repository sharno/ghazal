# Mesa for the H700's Mali-G31: panfrost only, EGL/GBM/GLES on DRM.
#
# nixpkgs' Mesa builds every driver with LLVM, X11 and Wayland (~1.1 GB).
# Here there is no LLVM at runtime, no window system and no glvnd: programs
# link this libEGL/libGLESv2 directly and the GBM backend is found in $out.
# Panfrost's OpenCL-C kernels are compiled at build time with the mesa_clc and
# panfrost_compile tools nixpkgs ships in the same Mesa version's cross_tools.
{ lib, stdenv, mesa, meson, ninja, pkg-config, python3Packages, bison, flex,
  glslang, libdrm, expat, zlib, zstd }:
stdenv.mkDerivation {
  pname = "mesa-h700";
  inherit (mesa) version src;
  outputs = [ "out" "dev" ];
  strictDeps = true;
  depsBuildBuild = [ pkg-config ];
  nativeBuildInputs = [
    meson ninja pkg-config bison flex (lib.getBin glslang) mesa.cross_tools
  ] ++ (with python3Packages; [ python packaging mako pyyaml ply pycparser ]);
  buildInputs = [ libdrm expat zlib zstd ];
  postPatch = "patchShebangs .";
  mesonFlags = [
    "--sysconfdir=/etc"
    # Every optional feature is off unless enabled below.
    (lib.mesonOption "auto_features" "disabled")
    (lib.mesonOption "platforms" "")
    (lib.mesonOption "gallium-drivers" "panfrost")
    (lib.mesonOption "vulkan-drivers" "")
    (lib.mesonOption "vulkan-layers" "")
    (lib.mesonOption "video-codecs" "")
    (lib.mesonOption "tools" "")
    (lib.mesonOption "glx" "disabled")
    (lib.mesonBool "libgbm-external" false)
    (lib.mesonBool "gallium-rusticl" false)
    (lib.mesonBool "teflon" false)
    (lib.mesonBool "build-tests" false)
    (lib.mesonOption "mesa-clc" "system")
    (lib.mesonOption "precomp-compiler" "system")
  ] ++ map (feature: lib.mesonEnable feature true) [
    "egl" "gbm" "gles2" "shader-cache" "zstd" "zlib" "expat" "xmlconfig"
  ];
  meta = mesa.meta // { description = "Mesa with only the panfrost driver, for EGL/GLES on DRM"; };
}
