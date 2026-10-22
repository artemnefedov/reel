# reel

A Wayland wallpaper daemon based on [swaybg](https://github.com/swaywm/swaybg) that adds
switching images from a directory on a timer.

Requires a compositor with `wlr-layer-shell` (Sway, Hyprland, river, niri, …).
Formats: JPEG, PNG, WebP.

## Usage

```
reel -i ~/wall.jpg                         # one image, fill mode
reel -d ~/Images/wallpapers -t 5m          # next image every 5 minutes
reel -d ~/Images/wallpapers -t 5m -s       # same, in random order
reel -c 1e1e2e                             # solid color
reel -d ~/walls -t 10m -o eDP-1 -i ~/laptop.png -m fit -c 000000
reel -i ~/wall.jpg -o DP-2 -r 90           # rotate the image on DP-2
```

| Option | |
|---|---|
| `-i, --image <file>` | Image to display |
| `-d, --dir <dir>` | Cycle through `jpg`/`jpeg`/`png`/`webp` files, sorted by name |
| `-t, --time <time>` | Interval for `--dir`: `30s`, `5m`, `1h` or seconds |
| `-s, --shuffle` | Random order for `--dir`, never the same image twice in a row |
| `-m, --mode <mode>` | `stretch`, `fit`, `fill` (default), `center`, `tile`, `solid_color` |
| `-c, --color RRGGBB` | Background color (`#` optional) |
| `-r, --rotate <degrees>` | Rotate the image clockwise: `0`, `90`, `180` or `270` |
| `-o, --output <name>` | Apply the following options to one output (`*` for all) |
| `-h, --help` / `-v, --version` | Show help / version and quit |

Options before the first `-o` apply to every output. An output is matched by its name (`DP-1`)
or its description (`make model serial`); an exact match wins over `*`.

The directory is re-read on every switch. Files that fail to decode are skipped.

### Autostart

Hyprland (Lua config):

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("reel -d " .. os.getenv("HOME") .. "/Images/wallpapers -t 5m -s")
end)
```

Sway (`config`):

```
exec reel -d ~/Images/wallpapers -t 5m -s
```

## Comparison

`fill` mode, median of 5 runs on Hyprland with two outputs (1920x1080 at scale 1.2 and
2560x1440 at scale 1). "Photo" is a 26 MP camera JPEG (6240x4160, ~10 MB).

Everything is measured over the whole process tree (a systemd scope): swaybg 1.2.2 loads images
through gdk-pixbuf/glycin, which starts `bwrap` and `glycin-image-rs` processes that stay
running afterwards. RSS is summed over the tree, so pages shared between its processes are
counted several times; PSS splits shared pages between the processes using them.

| | swaybg 1.2.2 | wbg 1.3.0 | reel |
|---|---|---|---|
| Processes at idle | 4 | 1 | 1 |
| Idle RSS | 26.8 MB | 6.6 MB | 3.9 MB |
| Idle PSS | 12.2 MB | 1.8 MB | 1.8 MB |
| Peak RSS, JPEG 2560x1440 | 54.5 MB | 52.1 MB | 28.6 MB |
| Peak RSS, PNG 2560x1440 | 54.7 MB | 52.8 MB | 28.5 MB |
| Peak RSS, WebP 2560x1440 | 57.9 MB | 56.9 MB | 28.6 MB |
| Peak RSS, photo | 203.3 MB | 116.7 MB | 114.6 MB |
| CPU, JPEG 2560x1440 | 315 ms | 450 ms | 131 ms |
| CPU, PNG 2560x1440 | 294 ms | 535 ms | 138 ms |
| CPU, WebP 2560x1440 | 478 ms | 592 ms | 309 ms |
| CPU, photo | 1179 ms | 929 ms | 521 ms |
| Slideshow | no | no | yes |

CPU is the total CPU time to decode and draw one wallpaper on both outputs.

Startup, with the image file already in the page cache (the last two rows: page cache dropped):

| | swaybg 1.2.2 | wbg 1.3.0 | reel |
|---|---|---|---|
| Start until the Wayland connection | 25 ms | 21 ms | 6 ms |
| Start until the first frame, JPEG 2560x1440 | 154 ms | 328 ms | 114 ms |
| Start until the first frame, PNG 2560x1440 | 168 ms | 385 ms | 131 ms |
| Start until the first frame, WebP 2560x1440 | 304 ms | 395 ms | 258 ms |
| Cold start until the Wayland connection | 97 ms | 128 ms | 25 ms |
| Cold start until the first frame, JPEG 2560x1440 | 260 ms | 412 ms | 119 ms |

The first frame is the first `wl_surface.attach` of a buffer, that is, the first output.

## How it works

- The image is decoded, drawn into a shm buffer and committed; the decoded image and the
  buffer are then freed, so only the compositor holds the pixels.
- Buffers follow the fractional scale (`wp_fractional_scale_v1` + `wp_viewporter`).
- A solid color is a single-pixel buffer (`wp_single_pixel_buffer_v1`).
- The shm buffer is advised for transparent huge pages (`MADV_HUGEPAGE`), which takes the
  page-fault cost out of filling it where shmem THP is set to `advise` or `always`.
- Resizing uses SIMD convolution (`fast_image_resize`); `fill` crops before scaling;
  `center`, `tile` and images already at the screen size are drawn without a resized copy.
- `--rotate` sets the buffer transform, the compositor does the rotation.
- A timed switch waits for a frame callback, so nothing is decoded while the output is off
  or hidden.
- Allocations of 128 KiB and more get their own mapping that is returned to the system on free
  (`M_MMAP_THRESHOLD`), and `malloc_trim` runs after each switch. Without this, peak memory in
  a long slideshow grows from 29 MB to 44 MB.

## Build and install

Rust 1.88 or newer is required. There are no C dependencies: the Wayland client and the image
decoders are written in Rust.

```
cargo install --locked --git https://github.com/artemnefedov/reel
```

From a checkout:

```
git clone https://github.com/artemnefedov/reel.git && cd reel
cargo build --release --locked
cargo test --locked
install -Dm755 target/release/reel ~/.local/bin/reel
```

At runtime reel needs only `glibc` and `libgcc`.

## License

[Apache-2.0](LICENSE)
