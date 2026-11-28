use crate::{textures::TextureOptions, Color32};
use std::sync::Arc;

/// An image stored in RAM.
///
/// To load an image file, see [`ColorImage::from_rgba_unmultiplied`].
///
/// In order to paint the image on screen, you first need to convert it to
///
/// See also: [`ColorImage`], [`FontImage`].
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum ImageData {
    /// RGBA image.
    Color(Arc<ColorImage>),

    /// Used for the font texture.
    Font(FontImage),
}

impl ImageData {
    pub fn size(&self) -> [usize; 2] {
        match self {
            Self::Color(image) => image.size,
            Self::Font(image) => image.size,
        }
    }

    pub fn width(&self) -> usize {
        self.size()[0]
    }

    pub fn height(&self) -> usize {
        self.size()[1]
    }

    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            Self::Color(_) | Self::Font(_) => 4,
        }
    }
}

// ----------------------------------------------------------------------------

/// A 2D RGBA color image in RAM.
#[derive(Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ColorImage {
    /// width, height.
    pub size: [usize; 2],

    /// The pixels, row by row, from top to bottom.
    pub pixels: Vec<Color32>,
}

impl ColorImage {
    /// Create an image filled with the given color.
    pub fn new(size: [usize; 2], color: Color32) -> Self {
        Self {
            size,
            pixels: vec![color; size[0] * size[1]],
        }
    }

    /// Create a [`ColorImage`] from flat un-multiplied RGBA data.
    ///
    /// This is usually what you want to use after having loaded an image file.
    ///
    /// Panics if `size[0] * size[1] * 4 != rgba.len()`.
    ///
    /// ## Example using the [`image`](crates.io/crates/image) crate:
    /// ``` ignore
    /// fn load_image_from_path(path: &std::path::Path) -> Result<egui::ColorImage, image::ImageError> {
    ///     let image = image::io::Reader::open(path)?.decode()?;
    ///     let size = [image.width() as _, image.height() as _];
    ///     let image_buffer = image.to_rgba8();
    ///     let pixels = image_buffer.as_flat_samples();
    ///     Ok(egui::ColorImage::from_rgba_unmultiplied(
    ///         size,
    ///         pixels.as_slice(),
    ///     ))
    /// }
    ///
    /// fn load_image_from_memory(image_data: &[u8]) -> Result<ColorImage, image::ImageError> {
    ///     let image = image::load_from_memory(image_data)?;
    ///     let size = [image.width() as _, image.height() as _];
    ///     let image_buffer = image.to_rgba8();
    ///     let pixels = image_buffer.as_flat_samples();
    ///     Ok(ColorImage::from_rgba_unmultiplied(
    ///         size,
    ///         pixels.as_slice(),
    ///     ))
    /// }
    /// ```
    pub fn from_rgba_unmultiplied(size: [usize; 2], rgba: &[u8]) -> Self {
        assert_eq!(size[0] * size[1] * 4, rgba.len());
        let pixels = rgba
            .chunks_exact(4)
            .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        Self { size, pixels }
    }

    pub fn from_rgba_premultiplied(size: [usize; 2], rgba: &[u8]) -> Self {
        assert_eq!(size[0] * size[1] * 4, rgba.len());
        let pixels = rgba
            .chunks_exact(4)
            .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        Self { size, pixels }
    }

    /// Create a [`ColorImage`] from flat opaque gray data.
    ///
    /// Panics if `size[0] * size[1] != gray.len()`.
    pub fn from_gray(size: [usize; 2], gray: &[u8]) -> Self {
        assert_eq!(size[0] * size[1], gray.len());
        let pixels = gray.iter().map(|p| Color32::from_gray(*p)).collect();
        Self { size, pixels }
    }

    /// Alternative method to `from_gray`.
    /// Create a [`ColorImage`] from iterator over flat opaque gray data.
    ///
    /// Panics if `size[0] * size[1] != gray_iter.len()`.
    #[doc(alias = "from_grey_iter")]
    pub fn from_gray_iter(size: [usize; 2], gray_iter: impl Iterator<Item = u8>) -> Self {
        let pixels: Vec<_> = gray_iter.map(Color32::from_gray).collect();
        assert_eq!(size[0] * size[1], pixels.len());
        Self { size, pixels }
    }

    /// A view of the underlying data as `&[u8]`
    #[cfg(feature = "bytemuck")]
    pub fn as_raw(&self) -> &[u8] {
        bytemuck::cast_slice(&self.pixels)
    }

    /// A view of the underlying data as `&mut [u8]`
    #[cfg(feature = "bytemuck")]
    pub fn as_raw_mut(&mut self) -> &mut [u8] {
        bytemuck::cast_slice_mut(&mut self.pixels)
    }

    /// Create a [`ColorImage`] from flat RGB data.
    ///
    /// This is what you want to use after having loaded an image file (and if
    /// you are ignoring the alpha channel - considering it to always be 0xff)
    ///
    /// Panics if `size[0] * size[1] * 3 != rgb.len()`.
    pub fn from_rgb(size: [usize; 2], rgb: &[u8]) -> Self {
        assert_eq!(size[0] * size[1] * 3, rgb.len());
        let pixels = rgb
            .chunks_exact(3)
            .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
            .collect();
        Self { size, pixels }
    }

    /// An example color image, useful for tests.
    pub fn example() -> Self {
        let width = 128;
        let height = 64;
        let mut img = Self::new([width, height], Color32::TRANSPARENT);
        for y in 0..height {
            for x in 0..width {
                let h = x as f32 / width as f32;
                let s = 1.0;
                let v = 1.0;
                let a = y as f32 / height as f32;
                img[(x, y)] = crate::Hsva { h, s, v, a }.into();
            }
        }
        img
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.size[0]
    }

    #[inline]
    pub fn height(&self) -> usize {
        self.size[1]
    }

    /// Create a new image from a patch of the current image.
    ///
    /// This method is especially convenient for screenshotting a part of the app
    /// since `region` can be interpreted as screen coordinates of the entire screenshot if `pixels_per_point` is provided for the native application.
    /// The floats of [`emath::Rect`] are cast to usize, rounding them down in order to interpret them as indices to the image data.
    ///
    /// Panics if `region.min.x > region.max.x || region.min.y > region.max.y`, or if a region larger than the image is passed.
    pub fn region(&self, region: &emath::Rect, pixels_per_point: Option<f32>) -> Self {
        let pixels_per_point = pixels_per_point.unwrap_or(1.0);
        let min_x = (region.min.x * pixels_per_point) as usize;
        let max_x = (region.max.x * pixels_per_point) as usize;
        let min_y = (region.min.y * pixels_per_point) as usize;
        let max_y = (region.max.y * pixels_per_point) as usize;
        assert!(
            min_x <= max_x && min_y <= max_y,
            "Screenshot region is invalid: {region:?}"
        );
        let width = max_x - min_x;
        let height = max_y - min_y;
        let mut output = Vec::with_capacity(width * height);
        let row_stride = self.size[0];

        for row in min_y..max_y {
            output.extend_from_slice(
                &self.pixels[row * row_stride + min_x..row * row_stride + max_x],
            );
        }
        Self {
            size: [width, height],
            pixels: output,
        }
    }
}

impl std::ops::Index<(usize, usize)> for ColorImage {
    type Output = Color32;

    #[inline]
    fn index(&self, (x, y): (usize, usize)) -> &Color32 {
        let [w, h] = self.size;
        assert!(x < w && y < h);
        &self.pixels[y * w + x]
    }
}

impl std::ops::IndexMut<(usize, usize)> for ColorImage {
    #[inline]
    fn index_mut(&mut self, (x, y): (usize, usize)) -> &mut Color32 {
        let [w, h] = self.size;
        assert!(x < w && y < h);
        &mut self.pixels[y * w + x]
    }
}

impl From<ColorImage> for ImageData {
    #[inline(always)]
    fn from(image: ColorImage) -> Self {
        Self::Color(Arc::new(image))
    }
}

impl From<Arc<ColorImage>> for ImageData {
    #[inline]
    fn from(image: Arc<ColorImage>) -> Self {
        Self::Color(image)
    }
}

impl std::fmt::Debug for ColorImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorImage")
            .field("size", &self.size)
            .field("pixel-count", &self.pixels.len())
            .finish_non_exhaustive()
    }
}

// ----------------------------------------------------------------------------

/// A single-channel image designed for the font texture.
///
/// Each value represents "coverage", i.e. how much a texel is covered by a character.
///
/// This is roughly interpreted as the opacity of a white image.
#[derive(Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct FontImage {
    /// width, height
    pub size: [usize; 2],

    /// The coverage value.
    ///
    /// Often you want to use [`Self::srgba_pixels`] instead.
    pub pixels: Vec<f32>,

    /// Optional per-texel 3x1 RGB ClearType subpixel coverage (`[r, g, b]` in `0..=255`).
    ///
    /// When `Some`, `srgba_pixels` outputs linear RGB subpixel masks directly (with solid white
    /// `[255, 255, 255, 255]` at `(0, 0)` and neutral `[a, a, a, a]` for discs/fallback glyphs)
    /// for hardware dual-source ClearType blending.
    #[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
    pub(crate) subpixel_rgb: Option<Vec<[u8; 3]>>,
}

impl FontImage {
    pub fn new(size: [usize; 2]) -> Self {
        Self {
            size,
            pixels: vec![0.0; size[0] * size[1]],
            subpixel_rgb: None,
        }
    }

    pub(crate) fn new_subpixel(size: [usize; 2]) -> Self {
        let len = size[0] * size[1];
        Self {
            size,
            pixels: vec![0.0; len],
            subpixel_rgb: Some(vec![[0, 0, 0]; len]),
        }
    }

    #[inline]
    pub(crate) fn is_subpixel(&self) -> bool {
        self.subpixel_rgb.is_some()
    }

    /// Write a neutral grayscale coverage value `[0.0..=1.0]` into both `pixels` and
    /// (when active) `subpixel_rgb` as `[a, a, a]`.
    #[inline]
    pub(crate) fn set_coverage_linear(&mut self, (x, y): (usize, usize), coverage: f32) {
        let [w, h] = self.size;
        assert!(x < w && y < h);
        let idx = y * w + x;
        let clamped = coverage.clamp(0.0, 1.0);
        self.pixels[idx] = clamped;
        if let Some(rgb) = self.subpixel_rgb.as_mut() {
            let a = fast_round(clamped * 255.0);
            rgb[idx] = [a, a, a];
        }
    }

    /// Write a 3x1 ClearType RGB subpixel coverage triplet `[r, g, b]` in `0..=255`.
    #[inline]
    pub(crate) fn set_subpixel_rgb(&mut self, (x, y): (usize, usize), rgb: [u8; 3]) {
        let [w, h] = self.size;
        assert!(x < w && y < h);
        let idx = y * w + x;
        let luma = (0.299 * rgb[0] as f32 + 0.587 * rgb[1] as f32 + 0.114 * rgb[2] as f32) / 255.0;
        self.pixels[idx] = luma.clamp(0.0, 1.0);
        if let Some(buf) = self.subpixel_rgb.as_mut() {
            buf[idx] = rgb;
        }
    }

    pub(crate) fn resize_buffers(&mut self, new_len: usize) {
        self.pixels.resize(new_len, 0.0);
        if let Some(buf) = self.subpixel_rgb.as_mut() {
            buf.resize(new_len, [0, 0, 0]);
        }
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.size[0]
    }

    #[inline]
    pub fn height(&self) -> usize {
        self.size[1]
    }

    /// Returns the textures as `sRGBA` premultiplied pixels, row by row, top to bottom.
    ///
    /// `gamma` should normally be set to `None`.
    ///
    /// If you are having problems with text looking skinny and pixelated, try using a low gamma, e.g. `0.4`.
    #[inline]
    pub fn srgba_pixels(&self, gamma: Option<f32>) -> impl ExactSizeIterator<Item = Color32> + '_ {
        // TODO(emilk): this default coverage gamma is a magic constant, chosen by eye. I don't even know why we need it.
        // Maybe we need to implement the ideas in https://hikogui.org/2022/10/24/the-trouble-with-anti-aliasing.html
        let gamma = gamma.unwrap_or(0.55);
        let subpixel_rgb = self.subpixel_rgb.as_deref();
        (0..self.pixels.len()).map(move |idx| {
            if let Some(rgb) = subpixel_rgb {
                let [r, g, b] = rgb[idx];
                let a = r.max(g).max(b);
                Color32::from_rgba_premultiplied(r, g, b, a)
            } else {
                let alpha = self.pixels[idx].powf(gamma);
                // We want to multiply with `vec4(alpha)` in the fragment shader:
                let a = fast_round(alpha * 255.0);
                Color32::from_rgba_premultiplied(a, a, a, a)
            }
        })
    }

    /// Clone a sub-region as a new image.
    pub fn region(&self, [x, y]: [usize; 2], [w, h]: [usize; 2]) -> Self {
        assert!(x + w <= self.width());
        assert!(y + h <= self.height());

        let mut pixels = Vec::with_capacity(w * h);
        for row in y..y + h {
            let offset = row * self.width() + x;
            pixels.extend(&self.pixels[offset..(offset + w)]);
        }
        assert_eq!(pixels.len(), w * h);

        let subpixel_rgb = self.subpixel_rgb.as_ref().map(|src| {
            let mut dst = Vec::with_capacity(w * h);
            for row in y..y + h {
                let offset = row * self.width() + x;
                dst.extend_from_slice(&src[offset..(offset + w)]);
            }
            assert_eq!(dst.len(), w * h);
            dst
        });

        Self {
            size: [w, h],
            pixels,
            subpixel_rgb,
        }
    }
}

impl std::ops::Index<(usize, usize)> for FontImage {
    type Output = f32;

    #[inline]
    fn index(&self, (x, y): (usize, usize)) -> &f32 {
        let [w, h] = self.size;
        assert!(x < w && y < h);
        &self.pixels[y * w + x]
    }
}

impl std::ops::IndexMut<(usize, usize)> for FontImage {
    #[inline]
    fn index_mut(&mut self, (x, y): (usize, usize)) -> &mut f32 {
        let [w, h] = self.size;
        assert!(x < w && y < h);
        &mut self.pixels[y * w + x]
    }
}

impl From<FontImage> for ImageData {
    #[inline(always)]
    fn from(image: FontImage) -> Self {
        Self::Font(image)
    }
}

#[inline]
fn fast_round(r: f32) -> u8 {
    (r + 0.5) as _ // rust does a saturating cast since 1.45
}

// ----------------------------------------------------------------------------

/// A change to an image.
///
/// Either a whole new image, or an update to a rectangular region of it.
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[must_use = "The painter must take care of this"]
pub struct ImageDelta {
    /// What to set the texture to.
    ///
    /// If [`Self::pos`] is `None`, this describes the whole texture.
    ///
    /// If [`Self::pos`] is `Some`, this describes a patch of the whole image starting at [`Self::pos`].
    pub image: ImageData,

    pub options: TextureOptions,

    /// If `None`, set the whole texture to [`Self::image`].
    ///
    /// If `Some(pos)`, update a sub-region of an already allocated texture with the patch in [`Self::image`].
    pub pos: Option<[usize; 2]>,
}

impl ImageDelta {
    /// Update the whole texture.
    pub fn full(image: impl Into<ImageData>, options: TextureOptions) -> Self {
        Self {
            image: image.into(),
            options,
            pos: None,
        }
    }

    /// Update a sub-region of an existing texture.
    pub fn partial(pos: [usize; 2], image: impl Into<ImageData>, options: TextureOptions) -> Self {
        Self {
            image: image.into(),
            options,
            pos: Some(pos),
        }
    }

    /// Is this affecting the whole texture?
    /// If `false`, this is a partial (sub-region) update.
    pub fn is_whole(&self) -> bool {
        self.pos.is_none()
    }
}

#[cfg(test)]
mod cleartype_image_tests {
    use super::*;
    use crate::{
        text::{
            cleartype_subpixel_active, dual_source_blending_supported,
            set_dual_source_blending_supported, FontDefinitions, FontId, Fonts, LayoutJob,
        },
        textures::TextureOptions,
        TextureAtlas,
    };

    #[test]
    fn font_image_subpixel_srgba_preserves_solid_white_and_rgb_masks() {
        let mut atlas = TextureAtlas::new_with_subpixel([1024, 32], true);
        let (pos, img) = atlas.allocate((2, 2));
        img.set_subpixel_rgb((pos.0, pos.1), [240, 128, 32]);
        img.set_subpixel_rgb((pos.0 + 1, pos.1), [12, 200, 255]);
        img.set_coverage_linear((pos.0, pos.1 + 1), 0.5);

        let full: Vec<Color32> = atlas.image().srgba_pixels(None).collect();
        // Top-left (0, 0) WHITE_UV must remain exact Color32::WHITE [255, 255, 255, 255]
        assert_eq!(full[0], Color32::WHITE);
        assert_eq!(full[0].to_array(), [255, 255, 255, 255]);

        // Subpixel texels output exact [r, g, b, max(r, g, b)] without grayscale gamma distortion
        let w = atlas.image().width();
        assert_eq!(
            full[pos.1 * w + pos.0].to_array(),
            [240, 128, 32, 240]
        );
        assert_eq!(
            full[pos.1 * w + pos.0 + 1].to_array(),
            [12, 200, 255, 255]
        );
        // Linear neutral coverage (used by discs / fallback glyphs) maps uniformly to [a, a, a, a]
        let disc_px = full[(pos.1 + 1) * w + pos.0].to_array();
        assert_eq!(disc_px[0], disc_px[1]);
        assert_eq!(disc_px[1], disc_px[2]);
        assert_eq!(disc_px[2], disc_px[3]);
        assert!((126..=129).contains(&disc_px[0]), "got {disc_px:?}");

        // Partial region extraction preserves subpixel RGB data and emits NEAREST TextureOptions
        let region = atlas.image().region([pos.0, pos.1], [2, 2]);
        let rpx: Vec<Color32> = region.srgba_pixels(None).collect();
        assert_eq!(rpx[0].to_array(), [240, 128, 32, 240]);
        assert_eq!(rpx[1].to_array(), [12, 200, 255, 255]);

        let delta = atlas.take_delta().expect("initial full delta");
        assert_eq!(delta.options, TextureOptions::NEAREST);
    }

    #[cfg(windows)]
    #[test]
    fn fonts_begin_pass_rebuilds_atlas_when_dual_source_flips_and_snaps_quads() {
        let prev = dual_source_blending_supported();
        set_dual_source_blending_supported(false);

        let fonts = Fonts::new(1.0, 2048, FontDefinitions::default());
        assert!(!cleartype_subpixel_active());
        let d0 = fonts.font_image_delta().expect("initial grayscale delta");
        assert_eq!(d0.options, TextureOptions::LINEAR);

        // Simulate GPU backend enabling Dual-Source Blending before first frame
        set_dual_source_blending_supported(true);
        fonts.begin_pass(1.0, 2048);

        let job = LayoutJob::simple(
            "Hello 123".into(),
            FontId::proportional(13.0),
            Color32::BLACK,
            f32::INFINITY,
        );
        let galley = fonts.layout_job(job);
        let delta = fonts
            .font_image_delta()
            .expect("rebuilt full atlas delta after dual-source flip");
        assert!(delta.is_whole());
        if cleartype_subpixel_active() {
            assert_eq!(delta.options, TextureOptions::NEAREST);

            // Verify tessellate_text snaps fractional galley_pos to integer physical pixels
            let mut tess = crate::Tessellator::new(
                1.0,
                crate::TessellationOptions {
                    round_text_to_pixels: false,
                    ..Default::default()
                },
                fonts.font_image_size(),
                vec![],
            );
            let mut mesh = crate::Mesh::default();
            tess.tessellate_text(
                &crate::TextShape::new(
                    emath::pos2(10.37, 20.61),
                    galley,
                    Color32::BLACK,
                ),
                &mut mesh,
            );
            assert!(!mesh.vertices.is_empty());
            for v in &mesh.vertices {
                assert!(
                    (v.pos.x - v.pos.x.round()).abs() < 1e-4
                        && (v.pos.y - v.pos.y.round()).abs() < 1e-4,
                    "subpixel glyph vertex should snap to physical integer pixels: {:?}",
                    v.pos
                );
            }
        }

        // Restore previous state
        set_dual_source_blending_supported(prev);
    }
}
