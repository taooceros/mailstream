//! One square PNG per picture at the drawn size, as the Swift engine's `AvatarResolver.normalize`
//! draws it: photos and logos fill the square, cropped to its middle; site icons sit inside on
//! white, as their transparent corners and odd shapes don't survive a circular crop. Raster
//! formats decode with `image`, SVG (BIMI logos) with resvg.
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use resvg::tiny_skia::{self, Pixmap, PixmapPaint, PremultipliedColorU8, Transform};
use resvg::usvg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Photo,
    /// Fills like a photo unless its corners are see-through (a rounded square on nothing);
    /// then it sits inside on white like an icon.
    Logo,
    Icon,
}

/// What a picture decodes to.
enum Source {
    Raster(DynamicImage),
    Vector(Box<usvg::Tree>),
}

impl Source {
    fn decode(data: &[u8]) -> Option<Self> {
        if let Ok(image) = image::load_from_memory(data) {
            return Some(Self::Raster(image));
        }
        usvg::Tree::from_data(data, &usvg::Options::default()).ok().map(|tree| Self::Vector(Box::new(tree)))
    }

    fn size(&self) -> (f32, f32) {
        match self {
            Self::Raster(image) => {
                let (width, height) = image.dimensions();
                (width as f32, height as f32)
            }
            Self::Vector(tree) => (tree.size().width(), tree.size().height()),
        }
    }
}

/// `data` as a `side`-pixel square PNG in `style`; nil when it does not decode, or is a raster
/// site icon smaller than 32 px (too blurry to scale up).
pub fn normalize(data: &[u8], style: Style, side: u32) -> Option<Vec<u8>> {
    let source = Source::decode(data)?;
    let (width, height) = source.size();
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    // Vector pictures have no pixels to count.
    if style == Style::Icon && matches!(source, Source::Raster(_)) && width.max(height) < 32.0 {
        return None;
    }
    let canvas = draw(&source, style, side)?;
    if style == Style::Logo {
        let far = side - 2;
        let see_through = [(1, 1), (far, 1), (1, far), (far, far)]
            .into_iter()
            .any(|(x, y)| canvas.pixel(x, y).is_none_or(|pixel| pixel.alpha() < 128));
        if see_through {
            return normalize(data, Style::Icon, side);
        }
    }
    canvas.encode_png().ok()
}

fn draw(source: &Source, style: Style, side: u32) -> Option<Pixmap> {
    let mut canvas = Pixmap::new(side, side)?;
    let full = side as f32;
    let (width, height) = source.size();
    // The square drawn into, and whether the picture covers it (cropped) or fits inside it.
    let (inset, cover) = match style {
        Style::Photo | Style::Logo => (0.0, true),
        Style::Icon => {
            canvas.fill(tiny_skia::Color::WHITE);
            (full * 0.18, false)
        }
    };
    let space = full - 2.0 * inset;
    let scale = if cover { (space / width).max(space / height) } else { (space / width).min(space / height) };
    let (drawn_width, drawn_height) = (width * scale, height * scale);
    let (x, y) = ((full - drawn_width) / 2.0, (full - drawn_height) / 2.0);
    match source {
        Source::Vector(tree) => {
            let transform = Transform::from_row(scale, 0.0, 0.0, scale, x, y);
            resvg::render(tree, transform, &mut canvas.as_mut());
        }
        Source::Raster(image) => {
            let pixels_wide = (drawn_width.round() as u32).max(1);
            let pixels_high = (drawn_height.round() as u32).max(1);
            let resized = resized(image, pixels_wide, pixels_high)?;
            let left = ((full - pixels_wide as f32) / 2.0).round() as i32;
            let top = ((full - pixels_high as f32) / 2.0).round() as i32;
            canvas.draw_pixmap(left, top, resized.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    }
    Some(canvas)
}

/// `image` resampled to `width` × `height`, premultiplied so transparent edges don't darken.
fn resized(image: &DynamicImage, width: u32, height: u32) -> Option<Pixmap> {
    let mut rgba = image.to_rgba32f();
    for pixel in rgba.pixels_mut() {
        let alpha = pixel[3];
        for channel in &mut pixel.0[..3] {
            *channel *= alpha;
        }
    }
    let scaled = image::imageops::resize(&rgba, width, height, FilterType::Lanczos3);
    let mut pixmap = Pixmap::new(width, height)?;
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    for (target, pixel) in pixmap.pixels_mut().iter_mut().zip(scaled.pixels()) {
        let alpha = byte(pixel[3]);
        // Resampling rings a little past the alpha; premultiplied color never exceeds it.
        let [red, green, blue] = [0, 1, 2].map(|channel| byte(pixel[channel]).min(alpha));
        *target = PremultipliedColorU8::from_rgba(red, green, blue, alpha)?;
    }
    Some(pixmap)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG of `width` × `height` filled with `color` (RGBA).
    pub(crate) fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn decoded(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    #[test]
    fn photos_fill_and_crop_to_the_middle() {
        // Left half red, right half blue, wider than high: the middle of it fills the square.
        let mut wide = image::RgbaImage::from_pixel(300, 100, image::Rgba([255, 0, 0, 255]));
        for x in 150..300 {
            for y in 0..100 {
                wide.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let mut data = std::io::Cursor::new(Vec::new());
        wide.write_to(&mut data, image::ImageFormat::Png).unwrap();
        let out = decoded(&normalize(data.get_ref(), Style::Photo, 96).unwrap());
        assert_eq!(out.dimensions(), (96, 96));
        assert!(out.get_pixel(5, 48)[0] > 200 && out.get_pixel(90, 48)[2] > 200);
        assert!(out.pixels().all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn icons_sit_inside_on_white_and_tiny_ones_are_refused() {
        let out = decoded(&normalize(&png(64, 64, [0, 128, 0, 255]), Style::Icon, 96).unwrap());
        assert_eq!(*out.get_pixel(2, 2), image::Rgba([255, 255, 255, 255]));
        assert_eq!(*out.get_pixel(48, 48), image::Rgba([0, 128, 0, 255]));
        // The box is inset 18% (17.28 px) on each side.
        assert_eq!(*out.get_pixel(16, 48), image::Rgba([255, 255, 255, 255]));
        assert_eq!(*out.get_pixel(19, 48), image::Rgba([0, 128, 0, 255]));
        assert!(normalize(&png(16, 16, [0, 0, 0, 255]), Style::Icon, 96).is_none());
        assert!(normalize(b"not a picture", Style::Photo, 96).is_none());
    }

    #[test]
    fn logos_with_see_through_corners_become_icons() {
        let solid = decoded(&normalize(&png(200, 200, [10, 20, 30, 255]), Style::Logo, 96).unwrap());
        assert_eq!(*solid.get_pixel(1, 1), image::Rgba([10, 20, 30, 255]));
        let circle = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><circle cx="50" cy="50" r="50" fill="#ff0000"/></svg>"##;
        let out = decoded(&normalize(circle, Style::Logo, 96).unwrap());
        assert_eq!(*out.get_pixel(1, 1), image::Rgba([255, 255, 255, 255]));
        assert_eq!(*out.get_pixel(48, 48), image::Rgba([255, 0, 0, 255]));
        let square = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 20"><rect width="10" height="20" fill="#0000ff"/></svg>"##;
        let out = decoded(&normalize(square, Style::Logo, 96).unwrap());
        assert_eq!(*out.get_pixel(1, 1), image::Rgba([0, 0, 255, 255]));
    }
}
