//! Wayland state: one layer surface per output, drawn from the wallpaper configs.

use std::path::PathBuf;

use image::RgbImage;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_dispatch2, delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            Connection, Proxy, QueueHandle,
            globals::GlobalList,
            protocol::{
                wl_output::{Transform, WlOutput},
                wl_surface::WlSurface,
            },
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler},
};

use crate::{cli::Config, decode, playlist, render, render::Mode};

/// User data for objects whose events reel ignores (buffers, viewports, managers).
pub struct Noop;

impl<I: Proxy, D> Dispatch2<I, D> for Noop {
    fn event(&self, _: &mut D, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<D>) {}
}

/// One wallpaper surface per output.
struct Background {
    output: WlOutput,
    layer: LayerSurface,
    /// Index into `Reel::wallpapers`.
    wallpaper: usize,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    /// Logical size from the last configure.
    width: u32,
    height: u32,
    /// Integer buffer scale, used without a viewport.
    scale: i32,
    /// Preferred fractional scale in 1/120 units; 0 until the compositor sends one.
    fract: u32,
    /// The wallpaper changed since the last draw.
    dirty: bool,
    /// A new wallpaper waits for a frame callback, i.e. for the output to be visible.
    frame_pending: bool,
    /// (buffer width, buffer height, width, height) of the last draw.
    drawn: Option<(u32, u32, u32, u32)>,
}

impl Drop for Background {
    fn drop(&mut self) {
        self.viewport.take().inspect(WpViewport::destroy);
        self.fractional.take().inspect(WpFractionalScaleV1::destroy);
    }
}

/// Runtime state of one `-o` block, shared by all outputs it matches.
struct Wallpaper {
    output: String,
    dir: Option<PathBuf>,
    current: Option<PathBuf>,
    mode: Mode,
    color: [u8; 3],
    /// Applied by the compositor (`wl_surface.set_buffer_transform`), so rotating costs reel
    /// no memory or CPU: the image is drawn upright into a buffer of the rotated size.
    rotate: Transform,
    shuffle: bool,
}

impl From<Config> for Wallpaper {
    fn from(c: Config) -> Self {
        let has_image = c.image.is_some() || c.dir.is_some();
        Wallpaper {
            output: c.output,
            dir: c.dir,
            current: c.image,
            mode: if has_image {
                c.mode.unwrap_or(Mode::Fill)
            } else {
                Mode::SolidColor
            },
            color: c.color.unwrap_or([0, 0, 0]),
            // Checked on Hyprland with grim: buffer transform 270 shows the image turned 90°
            // clockwise, so the clockwise `--rotate` degrees map to the opposite transform.
            shuffle: c.shuffle,
            rotate: match c.rotate {
                Some(90) => Transform::_270,
                Some(180) => Transform::_180,
                Some(270) => Transform::_90,
                _ => Transform::Normal,
            },
        }
    }
}

pub struct Reel {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    viewporter: Option<WpViewporter>,
    fractional_manager: Option<WpFractionalScaleManagerV1>,
    single_pixel: Option<WpSinglePixelBufferManagerV1>,
    qh: QueueHandle<Reel>,
    backgrounds: Vec<Background>,
    wallpapers: Vec<Wallpaper>,
}

impl Reel {
    /// Binds the globals reel needs; exits if the compositor lacks a required one.
    pub fn new(globals: &GlobalList, qh: &QueueHandle<Reel>, configs: Vec<Config>) -> Reel {
        let missing = |what: &str| -> ! {
            eprintln!("reel: missing a required Wayland interface: {what}");
            std::process::exit(1)
        };
        let mut reel = Reel {
            registry_state: RegistryState::new(globals),
            output_state: OutputState::new(globals, qh),
            compositor: CompositorState::bind(globals, qh)
                .unwrap_or_else(|_| missing("wl_compositor")),
            layer_shell: LayerShell::bind(globals, qh)
                .unwrap_or_else(|_| missing("zwlr_layer_shell_v1")),
            shm: Shm::bind(globals, qh).unwrap_or_else(|_| missing("wl_shm")),
            viewporter: globals.bind(qh, 1..=1, Noop).ok(),
            fractional_manager: globals.bind(qh, 1..=1, Noop).ok(),
            single_pixel: globals.bind(qh, 1..=1, Noop).ok(),
            qh: qh.clone(),
            backgrounds: Vec::new(),
            wallpapers: configs.into_iter().map(Wallpaper::from).collect(),
        };
        (0..reel.wallpapers.len()).for_each(|i| reel.advance(i));
        reel
    }

    /// Moves wallpaper `i` to the next file of its directory.
    pub fn advance(&mut self, i: usize) {
        let wp = &mut self.wallpapers[i];
        let Some(dir) = &wp.dir else { return };
        let files = playlist::list_images(dir);
        let pick = if wp.shuffle {
            playlist::random_image
        } else {
            playlist::next_image
        };
        match pick(&files, wp.current.as_deref()) {
            Some(next) => wp.current = Some(next),
            None => eprintln!("reel: no images in {}", dir.display()),
        }
        // Draw only once the compositor asks for a frame: it does not while the output is
        // off or covered (DPMS, lock screen), so no image is decoded that nobody would see.
        for bg in self.backgrounds.iter_mut().filter(|bg| bg.wallpaper == i) {
            if bg.width == 0 {
                bg.dirty = true; // not configured yet: the first configure draws it
            } else if !bg.frame_pending {
                let surface = bg.layer.wl_surface();
                surface.frame(&self.qh, FrameCallbackData(surface.clone()));
                surface.commit();
                bg.frame_pending = true;
            }
        }
    }

    /// Loads the current image of wallpaper `i`; a broken file in a directory is skipped
    /// in favour of the next one, at most once per file.
    fn load(&mut self, i: usize) -> Option<RgbImage> {
        let wp = &mut self.wallpapers[i];
        let files = wp
            .dir
            .as_deref()
            .map(playlist::list_images)
            .unwrap_or_default();
        for _ in 0..files.len().max(1) {
            let path = wp.current.as_deref()?;
            match decode::load(path) {
                Ok(img) => return Some(img),
                Err(e) => eprintln!("reel: failed to load {}: {e}", path.display()),
            }
            wp.current = Some(playlist::next_image(&files, Some(path))?);
        }
        None
    }

    /// (buffer width, buffer height, width, height) `bg` should be drawn at, as in swaybg.
    fn frame_size(&self, bg: &Background) -> (u32, u32, u32, u32) {
        let (w, h) = (bg.width, bg.height);
        let (bw, bh) = match &bg.viewport {
            // A solid color is one pixel stretched by the compositor.
            Some(_) if self.wallpapers[bg.wallpaper].mode == Mode::SolidColor => (1, 1),
            // Round half up, like swaybg.
            Some(_) if bg.fract > 0 => ((w * bg.fract + 60) / 120, (h * bg.fract + 60) / 120),
            _ => (w * bg.scale as u32, h * bg.scale as u32),
        };
        match self.wallpapers[bg.wallpaper].rotate {
            Transform::_90 | Transform::_270 => (bh, bw, w, h),
            _ => (bw, bh, w, h),
        }
    }

    fn needs_draw(&self, bg: &Background) -> bool {
        bg.width > 0 && (bg.dirty || bg.drawn != Some(self.frame_size(bg)))
    }

    /// Decodes each wallpaper once, draws it on the outputs that need it and frees it.
    /// Runs once per batch of Wayland events, so a configure and a scale change cost one draw.
    pub fn flush(&mut self) {
        let mut decoded = false;
        for i in 0..self.wallpapers.len() {
            if !self
                .backgrounds
                .iter()
                .any(|bg| bg.wallpaper == i && self.needs_draw(bg))
            {
                continue;
            }
            let img = match self.wallpapers[i].mode {
                Mode::SolidColor => None,
                // Keep the previous picture if nothing loads.
                _ if self.wallpapers[i].current.is_some() => match self.load(i) {
                    Some(img) => Some(img),
                    None => continue,
                },
                _ => None,
            };
            decoded |= img.is_some();
            for j in 0..self.backgrounds.len() {
                let bg = &self.backgrounds[j];
                if bg.wallpaper != i || !self.needs_draw(bg) {
                    continue;
                }
                let size = self.frame_size(bg);
                match self.draw(bg, img.as_ref(), size) {
                    Ok(()) => {
                        (self.backgrounds[j].dirty, self.backgrounds[j].drawn) = (false, Some(size))
                    }
                    Err(e) => eprintln!("reel: failed to draw: {e}"),
                }
            }
        }
        #[cfg(target_env = "gnu")]
        if decoded {
            malloc_trim(0);
        }
    }

    fn draw(
        &self,
        bg: &Background,
        img: Option<&RgbImage>,
        (bw, bh, w, h): (u32, u32, u32, u32),
    ) -> std::io::Result<()> {
        let wp = &self.wallpapers[bg.wallpaper];
        let buffer = match &self.single_pixel {
            Some(spb) if (bw, bh) == (1, 1) => {
                let c = |v: u8| v as u32 * (u32::MAX / 255);
                let [r, g, b] = wp.color;
                spb.create_u32_rgba_buffer(c(r), c(g), c(b), u32::MAX, &self.qh, Noop)
            }
            _ => render::shm_buffer(img, wp.mode, wp.color, bw, bh, &self.shm, &self.qh)?,
        };
        let surface = bg.layer.wl_surface();
        surface.attach(Some(&buffer), 0, 0);
        surface.set_buffer_transform(wp.rotate);
        surface.damage_buffer(0, 0, bw as i32, bh as i32);
        match &bg.viewport {
            Some(viewport) => viewport.set_destination(w as i32, h as i32),
            None => surface.set_buffer_scale(bg.scale),
        }
        surface.commit();
        buffer.destroy();
        Ok(())
    }
}

/// The compositor's preferred fractional scale for one surface.
struct Fractional;

impl Dispatch2<WpFractionalScaleV1, Reel> for Fractional {
    fn event(
        &self,
        reel: &mut Reel,
        proxy: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<Reel>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
            return;
        };
        if let Some(bg) = reel
            .backgrounds
            .iter_mut()
            .find(|bg| bg.fractional.as_ref() == Some(proxy))
        {
            bg.fract = scale;
        }
    }
}

// SAFETY: these match glibc's `int malloc_trim(size_t)` and `int mallopt(int, int)`. They only
// tune the allocator and release free heap pages, have no preconditions and invalidate no
// pointers, so they are declared `safe`.
#[cfg(target_env = "gnu")]
unsafe extern "C" {
    safe fn malloc_trim(pad: usize) -> std::ffi::c_int;
    safe fn mallopt(param: std::ffi::c_int, value: std::ffi::c_int) -> std::ffi::c_int;
}

/// Serves every allocation of 128 KiB or more with its own mapping that is returned to the
/// system on free. glibc's default threshold grows after the first such free, after which
/// decoded images land in the heap, which fragments and keeps the freed memory.
pub fn tune_malloc() {
    #[cfg(target_env = "gnu")]
    {
        const M_MMAP_THRESHOLD: std::ffi::c_int = -3;
        mallopt(M_MMAP_THRESHOLD, 128 * 1024);
    }
}

/// Like swaybg: an exact name ("DP-1") or identifier ("make model serial") wins over "*".
fn match_output(patterns: &[&str], name: Option<&str>, description: Option<&str>) -> Option<usize> {
    // wlroots sets the description to "make model serial (name)"; Hyprland omits " (name)".
    let identifier = description.map(|d| d.rfind('(').map_or(d, |i| d[..i].trim_end()));
    patterns
        .iter()
        .position(|p| Some(*p) == name || Some(*p) == identifier)
        .or_else(|| patterns.iter().position(|p| *p == "*"))
}

impl OutputHandler for Reel {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: WlOutput) {
        let Some(info) = self.output_state.info(&output) else {
            return;
        };
        let patterns: Vec<&str> = self.wallpapers.iter().map(|w| w.output.as_str()).collect();
        let Some(wallpaper) =
            match_output(&patterns, info.name.as_deref(), info.description.as_deref())
        else {
            return;
        };
        let surface = self.compositor.create_surface(qh);
        // Empty input region: clicks pass through to the compositor.
        if let Ok(region) = Region::new(&self.compositor) {
            surface.set_input_region(Some(region.wl_region()));
        }
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Background,
            Some("wallpaper"),
            Some(&output),
        );
        layer.set_anchor(Anchor::all());
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        let surface = layer.wl_surface();
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(surface, qh, Noop));
        let fractional = self
            .fractional_manager
            .as_ref()
            .map(|m| m.get_fractional_scale(surface, qh, Fractional));
        layer.commit();
        let scale = info.scale_factor;
        self.backgrounds.push(Background {
            output,
            layer,
            wallpaper,
            viewport,
            fractional,
            width: 0,
            height: 0,
            scale,
            fract: 0,
            dirty: true,
            frame_pending: false,
            drawn: None,
        });
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.backgrounds.retain(|bg| bg.output != output);
    }
}

impl LayerShellHandler for Reel {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.backgrounds.retain(|bg| &bg.layer != layer);
    }

    // Redraws happen in `flush`, and only when the frame size actually changed.
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(bg) = self.backgrounds.iter_mut().find(|bg| &bg.layer == layer) {
            (bg.width, bg.height) = configure.new_size;
        }
    }
}

impl CompositorHandler for Reel {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        if let Some(bg) = self
            .backgrounds
            .iter_mut()
            .find(|bg| bg.layer.wl_surface() == surface)
        {
            bg.scale = factor;
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        if let Some(bg) = self
            .backgrounds
            .iter_mut()
            .find(|bg| bg.layer.wl_surface() == surface)
        {
            (bg.frame_pending, bg.dirty) = (false, true);
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl ShmHandler for Reel {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Reel {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_registry!(Reel);
delegate_dispatch2!(Reel);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs() {
        let p = ["*", "DP-1", "Dell Inc. U2720Q ABC123"];
        assert_eq!(match_output(&p, Some("DP-1"), None), Some(1));
        assert_eq!(
            match_output(
                &p,
                Some("HDMI-A-1"),
                Some("Dell Inc. U2720Q ABC123 (HDMI-A-1)")
            ),
            Some(2)
        );
        assert_eq!(
            match_output(&p, Some("HDMI-A-2"), Some("Other (HDMI-A-2)")),
            Some(0)
        );
        assert_eq!(
            match_output(&p, Some("HDMI-A-3"), Some("Dell Inc. U2720Q ABC123")),
            Some(2)
        );
        assert_eq!(match_output(&p[1..], Some("HDMI-A-2"), None), None);
    }
}
