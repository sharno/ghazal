// Decodes an H.264 Annex B file the way Moonlight does (low delay, slice
// threading, one access unit per packet) and reports the decode rate and the
// per-frame times, so the handheld's software decoding can be measured without
// a streaming host. Build: nix build .#decode-bench.
//
//   decode-bench FILE.h264 [THREADS] [PASSES]
#include <libavcodec/avcodec.h>
#include <libavutil/log.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct { size_t start, size; } Unit;

static double now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

// Offset of the next 00 00 01 at or after `from`, or `size`.
static size_t find_start(const uint8_t *d, size_t size, size_t from) {
  for (size_t i = from; i + 3 <= size; i++)
    if (d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1) return i;
  return size;
}

// Splits the stream into access units: a new one starts at an SEI/SPS/PPS/AUD
// or at a slice with first_mb_in_slice == 0 once the current one has a slice.
static Unit *split(const uint8_t *d, size_t size, size_t *count) {
  size_t cap = 1024, n = 0, au_start = find_start(d, size, 0), pos = au_start;
  int has_slice = 0;
  Unit *units = malloc(cap * sizeof *units);
  while (pos < size) {
    size_t header = pos + 3, next = find_start(d, size, header);
    int type = header < size ? d[header] & 0x1f : 0;
    int first_mb_zero = header + 1 < size && (d[header + 1] & 0x80);
    int slice = type == 1 || type == 5;
    if (has_slice && ((slice && first_mb_zero) || (type >= 6 && type <= 9))) {
      if (n == cap) units = realloc(units, (cap *= 2) * sizeof *units);
      units[n++] = (Unit){ au_start, pos - au_start };
      au_start = pos;
      has_slice = 0;
    }
    has_slice |= slice;
    pos = next;
  }
  if (n == cap) units = realloc(units, (cap + 1) * sizeof *units);
  units[n++] = (Unit){ au_start, size - au_start };
  *count = n;
  return units;
}

static int by_value(const void *a, const void *b) {
  double x = *(const double *)a, y = *(const double *)b;
  return (x > y) - (x < y);
}

int main(int argc, char **argv) {
  if (argc < 2) { fprintf(stderr, "usage: %s FILE.h264 [THREADS] [PASSES]\n", argv[0]); return 2; }
  int threads = argc > 2 ? atoi(argv[2]) : 4, passes = argc > 3 ? atoi(argv[3]) : 3;
  FILE *f = fopen(argv[1], "rb");
  if (!f) { perror(argv[1]); return 1; }
  fseek(f, 0, SEEK_END);
  size_t size = ftell(f);
  fseek(f, 0, SEEK_SET);
  uint8_t *data = malloc(size);
  if (fread(data, 1, size, f) != size) { perror("read"); return 1; }
  fclose(f);
  size_t units_count;
  Unit *units = split(data, size, &units_count);

  av_log_set_level(AV_LOG_ERROR);
  const AVCodec *codec = avcodec_find_decoder_by_name("h264");
  uint8_t *packet_data = malloc(size + AV_INPUT_BUFFER_PADDING_SIZE);
  double *times = malloc(units_count * passes * sizeof *times);
  size_t timed = 0, frames = 0;
  double total = 0;
  for (int pass = 0; pass < passes; pass++) {
    AVCodecContext *ctx = avcodec_alloc_context3(codec);
    ctx->flags |= AV_CODEC_FLAG_LOW_DELAY | AV_CODEC_FLAG_OUTPUT_CORRUPT;
    ctx->flags2 |= AV_CODEC_FLAG2_SHOW_ALL;
    // As Moonlight's SDL path: slice threads, one per slice requested of the host.
    ctx->thread_type = FF_THREAD_SLICE;
    ctx->thread_count = threads;
    if (avcodec_open2(ctx, codec, NULL) < 0) { fprintf(stderr, "cannot open decoder\n"); return 1; }
    AVPacket *packet = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    for (size_t i = 0; i < units_count; i++) {
      memcpy(packet_data, data + units[i].start, units[i].size);
      memset(packet_data + units[i].size, 0, AV_INPUT_BUFFER_PADDING_SIZE);
      packet->data = packet_data;
      packet->size = units[i].size;
      double begin = now_ms();
      int sent = avcodec_send_packet(ctx, packet);
      int got = 0;
      while (sent >= 0 && avcodec_receive_frame(ctx, frame) == 0) got++;
      double took = now_ms() - begin;
      if (got) {
        times[timed++] = took;
        total += took;
        frames += got;
      }
    }
    av_frame_free(&frame);
    av_packet_free(&packet);
    avcodec_free_context(&ctx);
  }
  if (!frames) { fprintf(stderr, "no frames decoded\n"); return 1; }
  qsort(times, timed, sizeof *times, by_value);
  size_t late = 0;
  for (size_t i = 0; i < timed; i++) late += times[i] > 1000.0 / 60.0;
  printf("%zu frames/pass, %d threads: %.1f fps, frame time avg %.2f ms, p50 %.2f, p99 %.2f, max %.2f, %.1f%% over 16.7 ms\n",
    frames / passes, threads, frames / (total / 1000.0), total / frames, times[timed / 2],
    times[(size_t)(timed * 0.99)], times[timed - 1], 100.0 * late / timed);
  return 0;
}
