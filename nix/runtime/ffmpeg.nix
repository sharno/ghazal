# FFmpeg's H.264 decoder and nothing else. Moonlight links libavcodec and
# libavutil only; nixpkgs' FFmpeg builds every codec, demuxer and filter and
# pulls in dozens of libraries, which would not fit the 1 GiB OS slot.
#
# This is software decoding: mainline's Cedrus driver and a video-engine node
# for the H616 are not in this kernel (MEDIA_SUPPORT is disabled), so the
# four Cortex-A53 cores decode. The decoder's NEON assembly is selected for
# aarch64 automatically; slice threading needs pthreads.
{ lib, stdenv, ffmpeg, removeReferencesTo }:
stdenv.mkDerivation {
  pname = "ffmpeg-h264-h700";
  inherit (ffmpeg) version src;
  outputs = [ "out" "dev" ];
  strictDeps = true;
  enableParallelBuilding = true;
  # Built like the emulators: without nixpkgs' hardening flags.
  hardeningDisable = [ "all" ];
  nativeBuildInputs = [ removeReferencesTo ];
  # configure is FFmpeg's own script, not autoconf's: it rejects --includedir
  # and the other output flags stdenv would add.
  setOutputFlags = false;
  configurePlatforms = [ ];
  # configure applies options in order: what is enabled must follow "everything".
  configureFlags = [
    "--prefix=${placeholder "out"}"
    "--libdir=${placeholder "out"}/lib"
    "--incdir=${placeholder "dev"}/include"
    "--enable-shared" "--disable-static" "--enable-pic"
  ] ++ map (option: "--disable-${option}") [
    "autodetect" "everything" "programs" "doc" "debug" "network"
    "avdevice" "avformat" "avfilter" "swscale" "swresample"
  ] ++ [ "--enable-pthreads" "--enable-decoder=h264" ];
  # configure's flags are embedded in config.h; drop the references to the
  # other outputs they would create.
  postConfigure = "remove-references-to -t ${placeholder "dev"} config.h";
  # Only the example sources are installed besides the libraries.
  postInstall = "rm -r $out/share";
  meta = {
    description = "FFmpeg's H.264 decoder for software decoding on the H700";
    homepage = "https://ffmpeg.org";
    license = lib.licenses.lgpl21Plus;
    platforms = lib.platforms.linux;
  };
}
