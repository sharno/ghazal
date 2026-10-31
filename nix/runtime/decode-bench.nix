# Decode benchmark for the handheld's software H.264 path (tools/decode-bench).
# Not part of the image: copy the binary to the device, which already has the
# libraries it links from the runtime's closure.
{ stdenv, pkg-config, ffmpeg }:
stdenv.mkDerivation {
  pname = "h264-decode-bench";
  version = "0";
  src = ../../tools/decode-bench;
  strictDeps = true;
  hardeningDisable = [ "all" ];
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ ffmpeg ];
  buildPhase = ''
    $CC -O2 -Wall -o decode-bench decode-bench.c $(pkg-config --cflags --libs libavcodec libavutil)
  '';
  installPhase = "install -Dm755 decode-bench $out/bin/decode-bench";
}
