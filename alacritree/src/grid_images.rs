//! Kitty graphics placements, drawn by the grid's paint callback.
//!
//! This module holds the renderer's side of an [`ImageFrame`] as plain data,
//! testable without a GL context: one texture per set of decoded pixels, and
//! where the image bands run among the grid's own passes. `grid_gl` issues
//! the calls. Uploads, frees and the quad upload happen only when the frame's
//! generation moves, so a steady screen only binds and draws its runs.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use alacritree_graphics::frame::{Band, ImageFrame, Pixels, PixelsKey};
use eframe::glow::{self, HasContext};

/// One draw the paint callback issues, in the order it issues them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Images(Band),
    Backgrounds,
    Glyphs,
    Decorations,
}

/// The callback's draw order, the one kitty and Ghostty draw their bands in.
///
/// The background pass skips cells still carrying the default background,
/// which eframe's clear already painted, so the lowest band shows through
/// those and under every coloured one. The cursor and egui's overlays are
/// painted after the callback, above every band.
pub(crate) const PASSES: [Pass; 6] = [
    Pass::Images(Band::UnderBackgrounds),
    Pass::Backgrounds,
    Pass::Images(Band::UnderText),
    Pass::Glyphs,
    Pass::Decorations,
    Pass::Images(Band::OverText),
];

/// The textures behind one [`ImageFrame`], generic over the texture handle so
/// the upload and free decisions run without a GPU.
pub(crate) struct ImageTextures<T> {
    textures: HashMap<PixelsKey, Texture<T>>,
    /// The texture each run of the last synced frame samples, by run index.
    /// `None` for pixels the driver could not take.
    run_textures: Vec<Option<T>>,
    synced: Option<u64>,
}

struct Texture<T> {
    /// Held weakly, so the texture outlives neither the image nor its
    /// eviction: the graphics layer owns the pixels, the renderer only mirrors
    /// them.
    pixels: Weak<Pixels>,
    /// `None` once an upload was refused, so the refusal is not retried on
    /// every frame that still shows the image.
    texture: Option<T>,
}

impl<T> Default for ImageTextures<T> {
    fn default() -> Self {
        Self { textures: HashMap::new(), run_textures: Vec::new(), synced: None }
    }
}

impl<T: Copy> ImageTextures<T> {
    /// Bring the textures in line with `frame`, and say whether it is a frame
    /// not seen before, whose quads the GPU does not hold yet.
    ///
    /// On a new frame, each run's pixels get a texture the first time their
    /// key appears, and every texture whose pixels are gone is handed to
    /// `free`. A frame already synced does nothing at all.
    pub(crate) fn sync(
        &mut self,
        frame: &ImageFrame,
        mut upload: impl FnMut(&Pixels) -> Option<T>,
        mut free: impl FnMut(T),
    ) -> bool {
        if self.synced == Some(frame.generation()) {
            return false;
        }
        self.synced = Some(frame.generation());
        self.run_textures.clear();
        for run in frame.runs() {
            let texture = self.textures.entry(run.pixels.key()).or_insert_with(|| Texture {
                pixels: Arc::downgrade(&run.pixels),
                texture: upload(&run.pixels),
            });
            self.run_textures.push(texture.texture);
        }
        self.textures.retain(|_, texture| {
            let alive = texture.pixels.strong_count() > 0;
            if !alive && let Some(spent) = texture.texture {
                free(spent);
            }
            alive
        });
        true
    }

    /// The texture run `index` of the last synced frame samples.
    pub(crate) fn run_texture(&self, index: usize) -> Option<T> {
        self.run_textures.get(index).copied().flatten()
    }
}

/// The texture size for `pixels`, or `None` when the driver cannot hold it.
///
/// An image larger than `GL_MAX_TEXTURE_SIZE` on either side makes
/// `glTexImage2D` fail with `GL_INVALID_VALUE`, which leaves the texture
/// empty and says nothing unless someone polls `glGetError`.
pub(crate) fn texture_size(pixels: &Pixels, max_side: u32) -> Option<[i32; 2]> {
    let (width, height) = (pixels.width(), pixels.height());
    let fits = (1..=max_side).contains(&width) && (1..=max_side).contains(&height);
    fits.then_some([width as i32, height as i32])
}

/// Upload `pixels` into a new texture, or `None` when it is too large or the
/// context cannot make one.
///
/// # Safety
///
/// `gl` must be current, as it is inside a paint callback.
pub(crate) unsafe fn upload_texture(
    gl: &glow::Context,
    pixels: &Pixels,
    max_side: u32,
) -> Option<glow::Texture> {
    let Some([width, height]) = texture_size(pixels, max_side) else {
        log::warn!(
            "image of {}x{} is not drawn: this GPU takes textures up to {max_side} a side",
            pixels.width(),
            pixels.height(),
        );
        return None;
    };
    unsafe {
        let texture = match gl.create_texture() {
            Ok(texture) => texture,
            Err(err) => {
                log::warn!("image is not drawn: {err}");
                return None;
            },
        };
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (name, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
        }
        // sRGB storage, as kitty and Ghostty use, so a scaled image is
        // filtered in linear light. GL 3.1 and GLES 3.0 both have the format
        // in core, and the grid needs one of them anyway.
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::SRGB8_ALPHA8 as i32,
            width,
            height,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(pixels.rgba())),
        );
        Some(texture)
    }
}

pub(crate) const IMAGE_VERT: &str = r#"
uniform vec2 u_origin;
uniform vec2 u_cell;
uniform vec2 u_viewport;
uniform sampler2D u_image;

in vec4 a_dest;
in vec4 a_src;

out vec2 v_uv;
flat out vec4 v_bounds;

void main() {
    // 0 = top-left, 1 = top-right, 2 = bottom-left, 3 = bottom-right.
    vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    // Placements are laid out in cells, so they scale with the text.
    vec2 pos = u_origin + mix(a_dest.xy, a_dest.zw, corner) * u_cell;

    gl_Position = vec4(
        2.0 * pos.x / u_viewport.x - 1.0,
        1.0 - 2.0 * pos.y / u_viewport.y,
        0.0,
        1.0);

    // With no CLAMP_TO_BORDER in GLSL 140 or ES 3.0, a sample half a texel
    // inside the source rect keeps filtering from reaching past its edge.
    vec2 size = vec2(textureSize(u_image, 0));
    vec2 low = a_src.xy + 0.5;
    vec2 high = max(a_src.zw - 0.5, low);
    v_bounds = vec4(low, high) / size.xyxy;
    v_uv = mix(a_src.xy, a_src.zw, corner) / size;
}
"#;

pub(crate) const IMAGE_FRAG: &str = r#"
uniform sampler2D u_image;
in vec2 v_uv;
flat in vec4 v_bounds;
out vec4 f_color;

// The inverse of the sRGB decode the texture format applies, as egui's own
// image shader does it.
vec3 srgb_gamma_from_linear(vec3 rgb) {
    bvec3 cutoff = lessThan(rgb, vec3(0.0031308));
    vec3 lower = rgb * vec3(12.92);
    vec3 higher = vec3(1.055) * pow(rgb, vec3(1.0 / 2.4)) - vec3(0.055);
    return mix(higher, lower, vec3(cutoff));
}

void main() {
    vec4 texel = texture(u_image, clamp(v_uv, v_bounds.xy, v_bounds.zw));
    // Premultiplied in gamma space, which is what the framebuffer holds and
    // what egui's `ONE, ONE_MINUS_SRC_ALPHA` blend expects.
    f_color = vec4(srgb_gamma_from_linear(texel.rgb) * texel.a, texel.a);
}
"#;

#[cfg(test)]
mod tests {
    use alacritree_graphics::frame::ImageQuad;

    use super::*;

    fn pixels(width: u32, height: u32) -> Arc<Pixels> {
        let bytes = width as usize * height as usize * 4;
        Arc::new(Pixels::new(width, height, vec![0; bytes].into()))
    }

    fn quad() -> ImageQuad {
        ImageQuad { dest: [0.0, 0.0, 1.0, 1.0], src: [0.0, 0.0, 1.0, 1.0] }
    }

    /// Stands in for the GL side: textures are numbered in upload order, and
    /// both what was uploaded and what was freed are kept for the assertions.
    #[derive(Default)]
    struct Gpu {
        uploaded: Vec<PixelsKey>,
        freed: Vec<u32>,
        refuse: bool,
    }

    impl Gpu {
        fn sync(&mut self, textures: &mut ImageTextures<u32>, frame: &ImageFrame) -> bool {
            let (uploaded, refuse) = (&mut self.uploaded, self.refuse);
            textures.sync(
                frame,
                |pixels| {
                    uploaded.push(pixels.key());
                    (!refuse).then_some(uploaded.len() as u32)
                },
                |texture| self.freed.push(texture),
            )
        }
    }

    #[test]
    fn image_bands_run_around_backgrounds_glyphs_and_decorations() {
        assert_eq!(PASSES, [
            Pass::Images(Band::UnderBackgrounds),
            Pass::Backgrounds,
            Pass::Images(Band::UnderText),
            Pass::Glyphs,
            Pass::Decorations,
            Pass::Images(Band::OverText),
        ]);
        for band in Band::ALL {
            let runs = PASSES.iter().filter(|&&pass| pass == Pass::Images(band)).count();
            assert_eq!(runs, 1, "{band:?} has no single place in the draw order");
        }
    }

    #[test]
    fn a_frame_already_synced_uploads_nothing_and_sends_no_quads() {
        let (mut gpu, mut textures, mut frame) =
            (Gpu::default(), ImageTextures::default(), ImageFrame::default());
        let a = pixels(2, 2);
        frame.push(Band::OverText, &a, quad());

        assert!(gpu.sync(&mut textures, &frame), "a first frame sends its quads");
        assert!(!gpu.sync(&mut textures, &frame), "the same generation sent them again");
        assert_eq!(gpu.uploaded, [a.key()]);
    }

    #[test]
    fn an_empty_frame_seen_before_costs_nothing() {
        let (mut gpu, mut textures, frame) =
            (Gpu::default(), ImageTextures::default(), ImageFrame::default());
        gpu.sync(&mut textures, &frame);

        assert!(!gpu.sync(&mut textures, &frame));
        assert!(gpu.uploaded.is_empty() && gpu.freed.is_empty());
    }

    #[test]
    fn an_image_uploads_once_however_many_frames_show_it() {
        let (mut gpu, mut textures, mut frame) =
            (Gpu::default(), ImageTextures::default(), ImageFrame::default());
        let a = pixels(2, 2);
        frame.push(Band::UnderText, &a, quad());
        gpu.sync(&mut textures, &frame);

        frame.clear();
        frame.push(Band::UnderText, &a, quad());
        frame.push(Band::OverText, &a, quad());

        assert!(gpu.sync(&mut textures, &frame), "a rebuilt frame sends its quads");
        assert_eq!(gpu.uploaded, [a.key()]);
        assert_eq!([textures.run_texture(0), textures.run_texture(1)], [Some(1), Some(1)]);
    }

    #[test]
    fn each_run_samples_the_texture_of_its_own_pixels() {
        let (mut gpu, mut textures, mut frame) =
            (Gpu::default(), ImageTextures::default(), ImageFrame::default());
        let (a, b) = (pixels(1, 1), pixels(1, 1));
        frame.push(Band::UnderBackgrounds, &a, quad());
        frame.push(Band::UnderText, &b, quad());
        frame.push(Band::OverText, &a, quad());

        gpu.sync(&mut textures, &frame);

        assert_eq!(gpu.uploaded, [a.key(), b.key()]);
        let runs: Vec<_> = (0..3).map(|run| textures.run_texture(run)).collect();
        assert_eq!(runs, [Some(1), Some(2), Some(1)]);
    }

    #[test]
    fn a_texture_is_freed_on_the_first_new_frame_after_its_pixels_go() {
        let (mut gpu, mut textures, mut frame) =
            (Gpu::default(), ImageTextures::default(), ImageFrame::default());
        let (a, b) = (pixels(1, 1), pixels(1, 1));
        frame.push(Band::OverText, &a, quad());
        frame.push(Band::OverText, &b, quad());
        gpu.sync(&mut textures, &frame);

        frame.clear();
        frame.push(Band::OverText, &b, quad());
        drop(a);
        gpu.sync(&mut textures, &frame);

        assert_eq!(gpu.freed, [1]);
    }

    #[test]
    fn an_image_scrolled_out_of_view_keeps_its_texture() {
        let (mut gpu, mut textures, mut frame) =
            (Gpu::default(), ImageTextures::default(), ImageFrame::default());
        let a = pixels(1, 1);
        frame.push(Band::OverText, &a, quad());
        gpu.sync(&mut textures, &frame);

        frame.clear();
        gpu.sync(&mut textures, &frame);
        frame.clear();
        frame.push(Band::OverText, &a, quad());
        gpu.sync(&mut textures, &frame);

        assert!(gpu.freed.is_empty());
        assert_eq!(gpu.uploaded, [a.key()], "coming back into view uploaded it again");
    }

    #[test]
    fn a_refused_upload_is_not_retried() {
        let mut gpu = Gpu { refuse: true, ..Gpu::default() };
        let (mut textures, mut frame) = (ImageTextures::default(), ImageFrame::default());
        let a = pixels(1, 1);
        frame.push(Band::OverText, &a, quad());
        gpu.sync(&mut textures, &frame);

        frame.clear();
        frame.push(Band::OverText, &a, quad());
        gpu.sync(&mut textures, &frame);

        assert_eq!(gpu.uploaded, [a.key()]);
        assert_eq!(textures.run_texture(0), None);
        drop(a);
        frame.clear();
        gpu.sync(&mut textures, &frame);
        assert!(gpu.freed.is_empty(), "freed a texture that was never made");
    }

    #[test]
    fn a_texture_larger_than_the_driver_allows_is_refused() {
        assert_eq!(texture_size(&pixels(16, 8), 16), Some([16, 8]));
        assert_eq!(texture_size(&pixels(17, 8), 16), None);
        assert_eq!(texture_size(&pixels(8, 17), 16), None);
        assert_eq!(texture_size(&pixels(0, 0), 16), None);
    }
}
