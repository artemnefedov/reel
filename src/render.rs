//! Drawing a wallpaper frame into a one-shot shm buffer.

use std::{borrow::Cow, str::FromStr};

use fast_image_resize as fr;
use image::RgbImage;
use smithay_client_toolkit::{
    reexports::client::{
        Dispatch, QueueHandle,
        protocol::{wl_buffer::WlBuffer, wl_shm},
    },
    shm::{Shm, raw::RawPool},
};

use crate::app::Noop;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Stretch,
    Fill,
    Fit,
    Center,
    Tile,
    SolidColor,
}

impl FromStr for Mode {
    type Err = ();

    fn from_str(s: &str) -> Result<Mode, ()> {
        Ok(match s {
            "stretch" => Mode::Stretch,
            "fill" => Mode::Fill,
            "fit" => Mode::Fit,
            "center" => Mode::Center,
            "tile" => Mode::Tile,
            "solid_color" => Mode::SolidColor,
            _ => return Err(()),
        })
    }
}

/// Bilinear resize of `src` to `w`x`h`; `fill` crops the overflow (centered) instead of distorting.
fn scale(src: &RgbImage, w: u32, h: u32, fill: bool) -> RgbImage {
    let view =
        fr::images::ImageRef::new(src.width(), src.height(), src.as_raw(), fr::PixelType::U8x3)
            .expect("RGB8 buffer");
    let mut dst = fr::images::Image::new(w, h, fr::PixelType::U8x3);
    let mut opts =
        fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Bilinear));
    if fill {
        opts = opts.fit_into_destination(None);
    }
    fr::Resizer::new()
        .resize(&view, &mut dst, &opts)
        .expect("same pixel types");
    RgbImage::from_raw(w, h, dst.into_vec()).expect("w*h*3 buffer")
}

/// The size an `iw`x`ih` image takes when fitted into `w`x`h` keeping its aspect ratio.
fn fit_size((iw, ih): (u32, u32), (w, h): (u32, u32)) -> (u32, u32) {
    let r = f64::min(w as f64 / iw as f64, h as f64 / ih as f64);
    let fit = |v: u32| ((v as f64 * r).round() as u32).max(1);
    (fit(iw), fit(ih))
}

/// Fills a `w`x`h` XRGB8888 buffer: `color` background with `img` placed according to `mode`.
fn compose(dst: &mut [u8], w: u32, h: u32, img: Option<&RgbImage>, mode: Mode, color: [u8; 3]) {
    let (scaled, x0, y0) = match (img, mode) {
        (None, _) | (_, Mode::SolidColor) => (None, 0, 0),
        (Some(img), mode) => {
            let target = match mode {
                Mode::Stretch | Mode::Fill => Some((w, h)),
                Mode::Fit => Some(fit_size(img.dimensions(), (w, h))),
                _ => None,
            };
            // Center, tile and images already at the target size draw straight from the
            // decoded image, without an intermediate copy.
            let rgb = match target {
                Some((tw, th)) if (tw, th) != img.dimensions() => {
                    Cow::Owned(scale(img, tw, th, mode == Mode::Fill))
                }
                _ => Cow::Borrowed(img),
            };
            // Center offset; negative crops. Tile starts at the top-left corner, as in swaybg.
            let (x0, y0) = match mode {
                Mode::Tile => (0, 0),
                _ => (
                    (w as i64 - rgb.width() as i64) / 2,
                    (h as i64 - rgb.height() as i64) / 2,
                ),
            };
            (Some(rgb), x0, y0)
        }
    };
    let [r, g, b] = color;
    // XRGB8888 is little-endian: bytes in memory are B, G, R, X.
    let bg = [b, g, r, 0xff];
    let bgrx = |p: &[u8; 3]| [p[2], p[1], p[0], 0xff];
    for (y, row) in dst
        .as_chunks_mut::<4>()
        .0
        .chunks_exact_mut(w as usize)
        .enumerate()
    {
        let Some(s) = &scaled else {
            row.fill(bg);
            continue;
        };
        let (sw, sh) = (s.width() as i64, s.height() as i64);
        let sy = if mode == Mode::Tile {
            y as i64 % sh
        } else {
            y as i64 - y0
        };
        if !(0..sh).contains(&sy) {
            row.fill(bg);
            continue;
        }
        let src = &s.as_raw().as_chunks::<3>().0[(sy * sw) as usize..][..sw as usize];
        if mode == Mode::Tile {
            row.iter_mut()
                .zip(src.iter().cycle())
                .for_each(|(d, p)| *d = bgrx(p));
            continue;
        }
        // Columns [start, end) of the row are covered by the image; the rest is background.
        let (start, end) = (
            x0.clamp(0, w as i64) as usize,
            (x0 + sw).clamp(0, w as i64) as usize,
        );
        row[..start].fill(bg);
        row[end.max(start)..].fill(bg);
        if start < end {
            row[start..end]
                .iter_mut()
                .zip(&src[(start as i64 - x0) as usize..])
                .for_each(|(d, p)| *d = bgrx(p));
        }
    }
}

/// Renders a `w`x`h` frame into a one-shot shm buffer. The pool and its mapping are freed on
/// return; the caller destroys the buffer after commit, so the compositor keeps the only copy.
#[allow(clippy::too_many_arguments)]
pub fn shm_buffer<D: Dispatch<WlBuffer, Noop> + 'static>(
    img: Option<&RgbImage>,
    mode: Mode,
    color: [u8; 3],
    w: u32,
    h: u32,
    shm: &Shm,
    qh: &QueueHandle<D>,
) -> std::io::Result<WlBuffer> {
    let stride = w as i32 * 4;
    let mut pool =
        RawPool::new((stride * h as i32) as usize, shm).map_err(std::io::Error::other)?;
    // Transparent huge pages, as tofi does: filling a 2560x1440 buffer then takes a handful of
    // page faults instead of thousands. Ignored where shmem THP is disabled.
    let _ = pool.mmap().advise(memmap2::Advice::HugePage);
    compose(pool.mmap(), w, h, img, mode, color);
    Ok(pool.create_buffer(
        0,
        w as i32,
        h as i32,
        stride,
        wl_shm::Format::Xrgb8888,
        Noop,
        qh,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i + 2], buf[i + 1], buf[i]]
    }
    #[test]
    fn center_and_tile() {
        let red = [255, 0, 0];
        let bg = [0, 0, 255];
        let img = RgbImage::from_pixel(2, 2, Rgb(red));
        let mut buf = vec![0; 4 * 4 * 4];

        compose(&mut buf, 4, 4, Some(&img), Mode::Center, bg);
        assert_eq!(px(&buf, 4, 0, 0), bg);
        assert_eq!(px(&buf, 4, 1, 1), red);
        assert_eq!(px(&buf, 4, 2, 2), red);
        assert_eq!(px(&buf, 4, 3, 3), bg);

        compose(&mut buf, 4, 4, Some(&img), Mode::Tile, bg);
        assert!((0..16).all(|i| px(&buf, 4, i % 4, i / 4) == red));

        // A wide image fits as a horizontal band; fill covers everything.
        let wide = RgbImage::from_pixel(4, 2, Rgb(red));
        compose(&mut buf, 4, 4, Some(&wide), Mode::Fit, bg);
        assert_eq!(
            (0..4).map(|y| px(&buf, 4, 0, y)).collect::<Vec<_>>(),
            [bg, red, red, bg]
        );
        compose(&mut buf, 4, 4, Some(&wide), Mode::Fill, bg);
        assert!((0..16).all(|i| px(&buf, 4, i % 4, i / 4) == red));

        compose(&mut buf, 4, 4, Some(&img), Mode::SolidColor, bg);
        assert!((0..16).all(|i| px(&buf, 4, i % 4, i / 4) == bg));
    }
}
