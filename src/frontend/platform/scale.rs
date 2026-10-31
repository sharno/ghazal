//! Bilinear upscaling of the menu frame onto an HDMI scanout buffer. The menu is
//! drawn once at the panel's size; an HDMI display gets that picture scaled to
//! fit, with bars where its aspect ratio differs. The display engine on the
//! H616 has no usable plane scaler in mainline, so this runs on the CPU, and
//! only over the part of the output that a damaged source region can reach.

use slint::Rgb8Pixel;

use super::Damage;
use crate::hdmi::Rect;

/// Fixed-point bits of the interpolation weights.
const WEIGHT_BITS: u32 = 8;
const ONE: u32 = 1 << WEIGHT_BITS;

/// Where a destination sample lands in the source: two neighbours and the
/// weight of the second, sampling pixel centres.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tap {
    first: usize,
    second: usize,
    weight: u32,
}

fn tap(destination: usize, destination_len: usize, source_len: usize) -> Tap {
    let position = ((2 * destination as i64 + 1) * source_len as i64 * i64::from(ONE)) / (2 * destination_len as i64) - i64::from(ONE / 2);
    let last = source_len - 1;
    let position = position.max(0) as u64;
    let first = (position >> WEIGHT_BITS) as usize;
    if first >= last {
        return Tap { first: last, second: last, weight: 0 };
    }
    Tap { first, second: first + 1, weight: (position & u64::from(ONE - 1)) as u32 }
}

/// The destination rectangle (x0, y0, x1, y1, exclusive end) that source
/// `damage` can change, one pixel wider on every side for the filter's reach.
fn destination_damage(damage: Damage, source: (usize, usize), rect: Rect) -> (usize, usize, usize, usize) {
    let (x, y, w, h) = damage;
    let scale = |start: usize, len: usize, source_len: usize, destination_len: u32| {
        let begin = start * destination_len as usize / source_len;
        let end = ((start + len) * destination_len as usize).div_ceil(source_len);
        (begin.saturating_sub(1), (end + 1).min(destination_len as usize))
    };
    let (x0, x1) = scale(x, w, source.0, rect.width);
    let (y0, y1) = scale(y, h, source.1, rect.height);
    (x0, y0, x1, y1)
}

/// Scales the damaged part of `frame` (`source` pixels, rows `stride` apart)
/// into `rect` of an XRGB8888 `target` whose rows are `pitch` bytes apart.
pub fn blit(frame: &[Rgb8Pixel], stride: usize, source: (usize, usize), damage: Damage, target: &mut [u8], pitch: usize, rect: Rect) {
    let (width, height) = (rect.width as usize, rect.height as usize);
    if source.0 == 0 || source.1 == 0 || width == 0 || height == 0 || frame.len() < stride * (source.1 - 1) + source.0 { return; }
    let (x0, y0, x1, y1) = destination_damage(damage, source, rect);
    let columns: Vec<Tap> = (x0..x1).map(|column| tap(column, width, source.0)).collect();
    for row in y0..y1 {
        let vertical = tap(row, height, source.1);
        let (upper, lower) = (&frame[vertical.first * stride..][..source.0], &frame[vertical.second * stride..][..source.0]);
        let out = &mut target[(rect.y as usize + row) * pitch + (rect.x as usize + x0) * 4..][..(x1 - x0) * 4];
        for (horizontal, out) in columns.iter().zip(out.as_chunks_mut::<4>().0) {
            let (top_left, top_right) = (upper[horizontal.first], upper[horizontal.second]);
            let (bottom_left, bottom_right) = (lower[horizontal.first], lower[horizontal.second]);
            let channel = |a: u8, b: u8, c: u8, d: u8| {
                let along = |left: u8, right: u8| u32::from(left) * (ONE - horizontal.weight) + u32::from(right) * horizontal.weight;
                ((along(a, b) * (ONE - vertical.weight) + along(c, d) * vertical.weight) >> (2 * WEIGHT_BITS)) as u8
            };
            // XRGB8888, little-endian in memory: B G R X.
            *out = [
                channel(top_left.b, top_right.b, bottom_left.b, bottom_right.b),
                channel(top_left.g, top_right.g, bottom_left.g, bottom_right.g),
                channel(top_left.r, top_right.r, bottom_left.r, bottom_right.r),
                0xff,
            ];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(value: u8) -> Rgb8Pixel {
        Rgb8Pixel { r: value, g: value, b: value }
    }

    fn scaled(frame: &[Rgb8Pixel], source: (usize, usize), rect: Rect, target_size: (usize, usize)) -> Vec<u8> {
        let mut target = vec![7u8; target_size.0 * target_size.1 * 4];
        blit(frame, source.0, source, (0, 0, source.0, source.1), &mut target, target_size.0 * 4, rect);
        target
    }

    #[test]
    fn samples_blend_neighbours_around_pixel_centres() {
        assert_eq!(tap(0, 4, 2), Tap { first: 0, second: 1, weight: 0 }, "before the first centre");
        assert_eq!(tap(1, 4, 2), Tap { first: 0, second: 1, weight: 64 });
        assert_eq!(tap(2, 4, 2), Tap { first: 0, second: 1, weight: 192 });
        assert_eq!(tap(3, 4, 2), Tap { first: 1, second: 1, weight: 0 }, "past the last centre");
        assert_eq!(tap(5, 10, 10), Tap { first: 5, second: 6, weight: 0 }, "unscaled");
    }

    #[test]
    fn a_black_to_white_edge_ramps_across_the_new_pixels() {
        let output = scaled(&[pixel(0), pixel(255)], (2, 1), Rect { x: 0, y: 0, width: 4, height: 1 }, (4, 1));
        let blues: Vec<u8> = output.chunks(4).map(|p| p[0]).collect();
        assert_eq!(blues, [0, 63, 191, 255]);
        assert!(output.chunks(4).all(|p| p[3] == 0xff && p[0] == p[1] && p[1] == p[2]));
    }

    #[test]
    fn a_flat_colour_stays_flat_inside_the_rect_and_the_bars_are_untouched() {
        let frame = vec![Rgb8Pixel { r: 10, g: 20, b: 30 }; 4 * 3];
        let output = scaled(&frame, (4, 3), Rect { x: 2, y: 0, width: 8, height: 6 }, (12, 6));
        for row in 0..6 {
            for column in 0..12 {
                let at = (row * 12 + column) * 4;
                let expected: [u8; 4] = if (2..10).contains(&column) { [30, 20, 10, 0xff] } else { [7; 4] };
                assert_eq!(output[at..at + 4], expected, "({column}, {row})");
            }
        }
    }

    #[test]
    fn an_unscaled_rect_is_a_straight_copy() {
        let frame: Vec<Rgb8Pixel> = (0..12).map(|value| pixel(value * 20)).collect();
        let output = scaled(&frame, (4, 3), Rect { x: 0, y: 0, width: 4, height: 3 }, (4, 3));
        for (index, chunk) in output.chunks(4).enumerate() {
            assert_eq!(chunk, [index as u8 * 20, index as u8 * 20, index as u8 * 20, 0xff]);
        }
    }

    #[test]
    fn damage_limits_the_output_that_is_rewritten() {
        let frame = vec![pixel(200); 8 * 4];
        let mut target = vec![7u8; 16 * 8 * 4];
        let rect = Rect { x: 0, y: 0, width: 16, height: 8 };
        blit(&frame, 8, (8, 4), (4, 2, 1, 1), &mut target, 16 * 4, rect);
        let written: Vec<usize> = target.chunks(4).enumerate().filter(|(_, p)| p[3] == 0xff).map(|(index, _)| index).collect();
        // Source pixel (4, 2) covers output (8..10, 4..6); the filter reaches one more pixel each way.
        let (columns, rows): (Vec<_>, Vec<_>) = written.iter().map(|index| (index % 16, index / 16)).unzip();
        assert_eq!((columns.iter().min(), columns.iter().max()), (Some(&7), Some(&10)));
        assert_eq!((rows.iter().min(), rows.iter().max()), (Some(&3), Some(&6)));
        assert_eq!(written.len(), 4 * 4);
    }

    #[test]
    fn degenerate_inputs_are_ignored() {
        let mut target = vec![7u8; 16];
        blit(&[], 0, (0, 0), (0, 0, 0, 0), &mut target, 16, Rect { x: 0, y: 0, width: 4, height: 1 });
        blit(&[pixel(1)], 1, (1, 1), (0, 0, 1, 1), &mut target, 16, Rect { x: 0, y: 0, width: 0, height: 0 });
        assert!(target.iter().all(|byte| *byte == 7));
    }
}
