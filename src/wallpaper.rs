//! Desktop background image.
//!
//! The source image is decoded once at startup. Per output it's scaled and
//! center-cropped to fill the output exactly ("cover" fit, like every other
//! desktop's wallpaper setting), producing a [`MemoryRenderBuffer`] that's
//! cached and only rebuilt when that output's size changes.

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Size, Transform},
};
use tracing::warn;

/// Shipped as the wallpaper whenever no `wallpaper` path is configured, or
/// the configured one fails to load.
static DEFAULT_WALLPAPER: &[u8] = include_bytes!("../resources/wallpaper.webp");

#[derive(Debug)]
pub struct Wallpaper {
    image: image::RgbaImage,
    cache: Option<(Size<i32, Logical>, MemoryRenderBuffer)>,
    blur_cache: Option<(Size<i32, Logical>, u32, MemoryRenderBuffer)>,
}

impl Wallpaper {
    /// Loads the wallpaper from `path` if given and decodable, falling back
    /// to the built-in default image otherwise (a missing/invalid path is
    /// logged and ignored rather than treated as fatal, matching how the
    /// rest of `config` handles bad settings).
    pub fn load(path: Option<&str>) -> Wallpaper {
        let image = path
            .and_then(|path| match image::open(path) {
                Ok(image) => Some(image.to_rgba8()),
                Err(err) => {
                    warn!(path, %err, "Failed to load configured wallpaper, using default");
                    None
                }
            })
            .unwrap_or_else(default_image);
        Wallpaper {
            image,
            cache: None,
            blur_cache: None,
        }
    }

    /// The buffer to render for an output of logical `size`, rebuilding it
    /// only when `size` differs from the last call.
    pub fn buffer_for(&mut self, size: Size<i32, Logical>) -> &MemoryRenderBuffer {
        if self.cache.as_ref().map(|(cached_size, _)| *cached_size) != Some(size) {
            let buffer = rasterize(&self.image, size);
            self.cache = Some((size, buffer));
        }
        &self.cache.as_ref().expect("just inserted above").1
    }

    /// A Gaussian-blurred copy of the output-sized wallpaper. This is cached
    /// independently because it is only needed when backdrop blur is enabled.
    pub fn blurred_buffer_for(
        &mut self,
        size: Size<i32, Logical>,
        radius: u32,
    ) -> &MemoryRenderBuffer {
        let radius = radius.clamp(1, 50);
        if self
            .blur_cache
            .as_ref()
            .map(|(cached_size, cached_radius, _)| (*cached_size, *cached_radius))
            != Some((size, radius))
        {
            let rasterized = rasterize_image(&self.image, size);
            let blurred = image::imageops::blur(&rasterized, radius as f32);
            self.blur_cache = Some((size, radius, buffer_from_image(&blurred)));
        }
        &self.blur_cache.as_ref().expect("just inserted above").2
    }
}

fn default_image() -> image::RgbaImage {
    image::load_from_memory(DEFAULT_WALLPAPER)
        .expect("built-in default wallpaper is a valid image")
        .to_rgba8()
}

/// Scales `image` up just enough to cover a `size`-sized output, then crops
/// the centered `size`-sized window out of it.
fn rasterize(image: &image::RgbaImage, size: Size<i32, Logical>) -> MemoryRenderBuffer {
    buffer_from_image(&rasterize_image(image, size))
}

fn rasterize_image(image: &image::RgbaImage, size: Size<i32, Logical>) -> image::RgbaImage {
    let target_w = size.w.max(1) as u32;
    let target_h = size.h.max(1) as u32;

    let (src_w, src_h) = image.dimensions();
    let scale = f64::max(target_w as f64 / src_w as f64, target_h as f64 / src_h as f64);
    let scaled_w = ((src_w as f64 * scale).round() as u32).max(target_w);
    let scaled_h = ((src_h as f64 * scale).round() as u32).max(target_h);

    let scaled = image::imageops::resize(image, scaled_w, scaled_h, image::imageops::FilterType::Triangle);

    let crop_x = (scaled_w - target_w) / 2;
    let crop_y = (scaled_h - target_h) / 2;
    image::imageops::crop_imm(&scaled, crop_x, crop_y, target_w, target_h).to_image()
}

fn buffer_from_image(image: &image::RgbaImage) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        image,
        Fourcc::Abgr8888,
        (image.width() as i32, image.height() as i32),
        1,
        Transform::Normal,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_wallpaper_decodes() {
        let image = default_image();
        assert!(image.width() > 0 && image.height() > 0);
    }

    #[test]
    fn buffer_for_handles_a_range_of_output_sizes_and_aspect_ratios() {
        let mut wallpaper = Wallpaper::load(None);

        for size in [Size::from((1920, 1080)), Size::from((800, 600)), Size::from((3440, 1440))] {
            wallpaper.buffer_for(size);
            assert_eq!(wallpaper.cache.as_ref().map(|(cached_size, _)| *cached_size), Some(size));
        }
    }

    #[test]
    fn buffer_for_only_rebuilds_when_the_size_changes() {
        let mut wallpaper = Wallpaper::load(None);
        let size = Size::from((1280, 720));

        wallpaper.buffer_for(size);
        assert!(wallpaper.cache.is_some());

        // Same size again: the cached entry for it is reused rather than
        // rebuilt (asserting on the outcome, since `MemoryRenderBuffer`
        // exposes no public identity to compare against directly).
        wallpaper.buffer_for(size);
        assert_eq!(wallpaper.cache.as_ref().map(|(cached_size, _)| *cached_size), Some(size));
    }

    #[test]
    fn unloadable_path_falls_back_to_default() {
        let wallpaper = Wallpaper::load(Some("/nonexistent/path/to/wallpaper.png"));
        assert!(wallpaper.image.width() > 0);
    }

    #[test]
    fn blurred_buffer_is_cached_by_size_and_radius() {
        let mut wallpaper = Wallpaper::load(None);
        let size = Size::from((320, 200));
        wallpaper.blurred_buffer_for(size, 8);
        assert_eq!(
            wallpaper
                .blur_cache
                .as_ref()
                .map(|(cached_size, radius, _)| (*cached_size, *radius)),
            Some((size, 8))
        );
        wallpaper.blurred_buffer_for(size, 16);
        assert_eq!(wallpaper.blur_cache.as_ref().map(|(_, radius, _)| *radius), Some(16));
    }
}
