//! Mail on the reader's own surface (MS-267): Blitz pages are transparent, so the reader's
//! surface shows where the mail paints nothing, and in dark mode the mail's colours move to it, as
//! Apple Mail, Gmail and Outlook do, never inverting the page as a whole. These are the rules;
//! `html_page.rs` walks a page's computed styles with them.
//!
//! - Light: a near-white background with nothing painted under it (the mail's page white on
//!   `body`, its outer tables) is cleared; every other colour stays the mail's.
//! - Dark: page white is cleared too. Other light, muted backgrounds and hairlines (page greys,
//!   pastel panels, `#ddd` rules) step down from the surface as far as they stood below white,
//!   and muted borders of any lightness move the same way (a near-black rule turns light);
//!   translucent grey shades turn into the same shade of white. Saturated and dark colours (brand
//!   bands, buttons, highlights) stay. Text that no longer stands out against what is now behind
//!   it turns light: near-black as the reader's own text, other colours with their hue kept, as
//!   far as the contrast it had in the mail (between 3 and 4.5, WCAG's large and body text). The
//!   browser's default link blue becomes the reader's link colour; text hidden in its background
//!   (a preview line, white on white) stays hidden. Images keep their pixels; a
//!   transparent one (a logo drawn for white) gets the light background it was drawn on behind it.
//!   An element with a background image (a hero, a textured panel) keeps its own colours and its
//!   text's, as that text was chosen for the image.
//! - Dark, mail that declares its own dark look (`color-scheme: dark`, a `prefers-color-scheme:
//!   dark` rule): laid out in dark, its backgrounds kept; only text that doesn't stand out moves.
//! - Dark, paper: mail that asks to stay light (`color-scheme: only light`), paints its page with
//!   an image or sets most of its text over images is drawn as written, on white, which the
//!   reader shows as a sheet (MS-256).

/// A colour: sRGB channels and straight alpha, 0–1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const WHITE: Self = Self::opaque(1., 1., 1.);
    pub const BLACK: Self = Self::opaque(0., 0., 0.);

    pub const fn opaque(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1. }
    }

    /// `0xRRGGBB`, opaque.
    pub fn hex(rgb: u32) -> Self {
        let channel = |shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.;
        Self::opaque(channel(16), channel(8), channel(0))
    }

    /// The colour as it shows over the opaque `under`.
    pub fn over(self, under: Self) -> Self {
        let mix = |top: f32, bottom: f32| top * self.a + bottom * (1. - self.a);
        Self::opaque(mix(self.r, under.r), mix(self.g, under.g), mix(self.b, under.b))
    }

    /// WCAG's relative luminance.
    pub fn luminance(self) -> f32 {
        let [r, g, b] = self.linear();
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn linear(self) -> [f32; 3] {
        let decode = |c: f32| {
            let c = c.clamp(0., 1.);
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        [decode(self.r), decode(self.g), decode(self.b)]
    }

    /// The colour as a CSS value.
    pub fn css(self) -> String {
        let byte = |c: f32| (c.clamp(0., 1.) * 255.).round() as u8;
        format!("rgba({}, {}, {}, {})", byte(self.r), byte(self.g), byte(self.b), (self.a.clamp(0., 1.) * 1000.).round() / 1000.)
    }

    /// Whether the channels match as 8-bit values.
    pub fn same(self, other: Self) -> bool {
        let byte = |c: f32| (c.clamp(0., 1.) * 255.).round() as u8;
        [self.r, self.g, self.b].map(byte) == [other.r, other.g, other.b].map(byte)
            && (self.a - other.a).abs() < 0.5 / 255.
    }
}

/// The browser's link colour (Blitz's `a[href]`), which a mail that styles no links shows.
pub(crate) const DEFAULT_LINK: Rgba = Rgba::opaque(0., 0., 238. / 255.);

/// WCAG's contrast ratio between two opaque colours.
pub(crate) fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (a, b) = (a.luminance() + 0.05, b.luminance() + 0.05);
    a.max(b) / a.min(b)
}

/// Lightness, chroma and hue in OKLCH.
#[derive(Clone, Copy, Debug)]
struct Lch {
    l: f32,
    c: f32,
    h: f32,
}

fn lch(colour: Rgba) -> Lch {
    let [r, g, b] = colour.linear();
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let lightness = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    Lch { l: lightness, c: a.hypot(b), h: b.atan2(a) }
}

/// `lch` in sRGB, its chroma reduced until it fits.
fn from_lch(lch: Lch, alpha: f32) -> Rgba {
    let encode = |c: f32| if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1. / 2.4) - 0.055 };
    let mut chroma = lch.c;
    loop {
        let (a, b) = (chroma * lch.h.cos(), chroma * lch.h.sin());
        let l = (lch.l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
        let m = (lch.l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
        let s = (lch.l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
        let rgb = [
            4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
            -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
            -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        ];
        let fits = rgb.iter().all(|c| (-0.001..=1.001).contains(c));
        if fits || chroma < 0.001 {
            let [r, g, b] = rgb.map(|c| encode(c.clamp(0., 1.)));
            return Rgba { r, g, b, a: alpha };
        }
        chroma *= 0.9;
    }
}

/// The surface a page sits on and the reader's own colours there.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Theme {
    pub dark: bool,
    pub surface: Rgba,
    pub text: Rgba,
    pub link: Rgba,
}

/// Page white: at least this light and at most this chroma (`#fafafa` and lighter greys).
const PAGE_WHITE: f32 = 0.98;
const NEUTRAL: f32 = 0.02;
/// Light, muted colours, which dark mode moves: lighter than this, less chroma than that (pastel
/// panels, page greys; not brand colours or highlights).
const LIGHT: f32 = 0.6;
const MUTED: f32 = 0.1;
/// Near-black text (less light and chroma than these) becomes the reader's own text in dark mode.
const NEAR_BLACK: f32 = 0.35;
const NEAR_BLACK_CHROMA: f32 = 0.04;
/// The contrast text keeps against what is behind it: what it had in the mail, within these.
const MIN_CONTRAST: f32 = 3.0;
const MAX_CONTRAST: f32 = 4.5;
/// Text that stood out less than this from its background was hidden on purpose.
const HIDDEN: f32 = 1.2;

/// What becomes of an element's background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Paint {
    Keep,
    /// Painted no more: what is behind shows.
    Clear,
    Set(Rgba),
}

/// The mail's background `colour` on an element; `on_surface` when nothing the page paints lies
/// behind it.
pub(crate) fn background(colour: Rgba, on_surface: bool, theme: &Theme) -> Paint {
    if colour.a < 0.02 {
        return Paint::Keep;
    }
    let lch = lch(colour);
    if on_surface && colour.a > 0.9 && lch.l >= PAGE_WHITE && lch.c <= NEUTRAL {
        return Paint::Clear;
    }
    match theme.dark.then(|| step_down(colour, lch, theme, LIGHT)).flatten() {
        Some(colour) => Paint::Set(colour),
        None => Paint::Keep,
    }
}

/// A border's `colour` in dark mode, where it changes: a muted one of any lightness, so a faint
/// hairline stays faint and a near-black rule stands out as it did.
pub(crate) fn border(colour: Rgba, theme: &Theme) -> Option<Rgba> {
    theme.dark.then(|| step_down(colour, lch(colour), theme, 0.)).flatten()
}

/// A muted colour lighter than `lighter_than` as far above the surface as it stood below white; a
/// translucent grey shade as the same shade of the other end. Other colours stay.
fn step_down(colour: Rgba, lch: Lch, theme: &Theme, lighter_than: f32) -> Option<Rgba> {
    if lch.c >= MUTED {
        return None;
    }
    if colour.a < 0.9 {
        return Some(Rgba { r: 1. - colour.r, g: 1. - colour.g, b: 1. - colour.b, a: colour.a });
    }
    if lch.l <= lighter_than {
        return None;
    }
    let surface = self::lch(theme.surface).l;
    Some(from_lch(Lch { l: surface + (1. - lch.l) * (1. - surface), ..lch }, colour.a))
}

/// Text in `colour` that the mail set on `was` and now shows on `behind` (both opaque): the
/// colour it shows in, which stands out from `behind` as much as it stood out from `was` (within
/// [`MIN_CONTRAST`]–[`MAX_CONTRAST`]). Light mode keeps every text colour.
pub(crate) fn text(colour: Rgba, was: Rgba, behind: Rgba, theme: &Theme) -> Rgba {
    if !theme.dark {
        return colour;
    }
    let target = contrast(colour.over(was), was);
    if target < HIDDEN {
        // Text the mail hid in its background (a preview line) stays hidden.
        return behind;
    }
    let target = target.clamp(MIN_CONTRAST, MAX_CONTRAST);
    if contrast(colour.over(behind), behind) >= target {
        return colour;
    }
    let lighter = contrast(Rgba::WHITE, behind) >= contrast(Rgba::BLACK, behind);
    let lch = lch(colour);
    if lighter && lch.l < NEAR_BLACK && lch.c < NEAR_BLACK_CHROMA && contrast(theme.text, behind) >= target {
        return theme.text;
    }
    // The other side of the lightness scale, then further until it stands out.
    let (mut l, step) = if lighter { (lch.l.max(1. - lch.l), 0.02) } else { (lch.l.min(1. - lch.l), -0.02) };
    loop {
        let shown = from_lch(Lch { l, ..lch }, 1.);
        if contrast(shown, behind) >= target || !(0. ..1.).contains(&(l + step)) {
            return shown;
        }
        l += step;
    }
}

/// The colour schemes a mail says it supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Schemes {
    /// It has a dark look of its own.
    pub dark: bool,
    /// It asks to be shown light only.
    pub only_light: bool,
}

/// The schemes the `content` of a mail's `color-scheme` (or `supported-color-schemes`) meta tags
/// and the text of its style sheets declare.
pub(crate) fn schemes<'a>(metas: impl IntoIterator<Item = &'a str>, styles: impl IntoIterator<Item = &'a str>) -> Schemes {
    let mut found = Schemes::default();
    let take = |found: &mut Schemes, value: &str| {
        let tokens: Vec<_> = value.split_ascii_whitespace().collect();
        if tokens.contains(&"dark") {
            found.dark = true;
        } else if tokens.contains(&"only") && tokens.contains(&"light") {
            found.only_light = true;
        }
    };
    for meta in metas {
        take(&mut found, &meta.to_ascii_lowercase());
    }
    for css in styles {
        let css = css.to_ascii_lowercase();
        let mut rest = css.as_str();
        while let Some(at) = rest.find("color-scheme") {
            let before = &rest[..at];
            let after = rest[at + "color-scheme".len()..].trim_start();
            rest = after;
            let Some(value) = after.strip_prefix(':') else { continue };
            let value = value.trim_start();
            if before.ends_with("prefers-") {
                if value.starts_with("dark") {
                    found.dark = true;
                }
            } else if !before.ends_with('-') {
                take(&mut found, value.split(|c| matches!(c, ';' | '}' | '!' | '"' | '\'')).next().unwrap_or_default());
            }
        }
    }
    if found.dark {
        found.only_light = false;
    }
    found
}

/// Whether dark mode draws the mail as written, on white paper: it asks to stay light, its page
/// is an image, or most of its text (`on_images` of `text` characters) lies over images.
pub(crate) fn needs_paper(schemes: Schemes, image_page: bool, text: usize, on_images: usize) -> bool {
    schemes.only_light || image_page || on_images * 2 > text
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Theme = Theme {
        dark: true,
        surface: Rgba::opaque(0x23 as f32 / 255., 0x1E as f32 / 255., 0x1B as f32 / 255.),
        text: Rgba::opaque(0xF6 as f32 / 255., 0xEF as f32 / 255., 0xE7 as f32 / 255.),
        link: Rgba::opaque(0x0A as f32 / 255., 0x84 as f32 / 255., 1.),
    };
    const LIGHT_THEME: Theme = Theme {
        dark: false,
        surface: Rgba::opaque(1., 0xFD as f32 / 255., 0xF9 as f32 / 255.),
        text: Rgba::opaque(0x2B as f32 / 255., 0x24 as f32 / 255., 0x20 as f32 / 255.),
        link: Rgba::opaque(0., 0x7A as f32 / 255., 1.),
    };

    fn round_trip(hex: u32) -> Rgba {
        let colour = Rgba::hex(hex);
        from_lch(lch(colour), 1.)
    }

    #[test]
    fn oklch_round_trips() {
        for hex in [0xFFFFFF, 0x000000, 0x0366D6, 0xFFD33D, 0x2DA44E, 0x6E7781, 0xFFF3CD] {
            assert!(round_trip(hex).same(Rgba::hex(hex)), "{hex:06x}: {:?}", round_trip(hex));
        }
        assert!((lch(Rgba::WHITE).l - 1.).abs() < 1e-3 && lch(Rgba::WHITE).c < 1e-3);
    }

    #[test]
    fn contrast_is_wcags() {
        assert!((contrast(Rgba::WHITE, Rgba::BLACK) - 21.).abs() < 0.01);
        assert!((contrast(Rgba::hex(0x777777), Rgba::WHITE) - 4.48).abs() < 0.02);
    }

    #[test]
    fn page_white_clears_only_on_the_surface() {
        for theme in [DARK, LIGHT_THEME] {
            assert_eq!(background(Rgba::WHITE, true, &theme), Paint::Clear);
            assert_eq!(background(Rgba::hex(0xFAFAFA), true, &theme), Paint::Clear);
            // A white card on the mail's own grey stays a card.
            assert_ne!(background(Rgba::WHITE, false, &theme), Paint::Clear);
            assert_eq!(background(Rgba { a: 0., ..Rgba::WHITE }, true, &theme), Paint::Keep);
        }
        // Light mode keeps every other colour.
        assert_eq!(background(Rgba::hex(0xF4F4F4), true, &LIGHT_THEME), Paint::Keep);
        assert_eq!(background(Rgba::WHITE, false, &LIGHT_THEME), Paint::Keep);
    }

    #[test]
    fn dark_mode_steps_light_muted_backgrounds_down_and_keeps_brand_colours() {
        let surface = lch(DARK.surface).l;
        let Paint::Set(grey) = background(Rgba::hex(0xF6F8FA), true, &DARK) else { panic!("GitHub's grey moves") };
        let grey = lch(grey).l;
        assert!(grey > surface && grey < surface + 0.05, "{grey}");
        let Paint::Set(white_card) = background(Rgba::WHITE, false, &DARK) else { panic!("a nested white card moves") };
        assert!((lch(white_card).l - surface).abs() < 0.01);
        let Paint::Set(panel) = background(Rgba::hex(0xFFF3CD), false, &DARK) else { panic!("a pastel panel moves") };
        assert!(panel.luminance() < 0.1 && panel.r > panel.b, "keeps its warm hue: {panel:?}");
        for brand in [0xFFD33D, 0x2DA44E, 0x0366D6, 0x24292F, 0x000000] {
            assert_eq!(background(Rgba::hex(brand), false, &DARK), Paint::Keep, "{brand:06x}");
        }
        let shade = Rgba { a: 0.05, ..Rgba::BLACK };
        assert_eq!(background(shade, false, &DARK), Paint::Set(Rgba { a: 0.05, ..Rgba::WHITE }));
    }

    #[test]
    fn hairlines_stay_faint_in_dark_mode() {
        let rule = border(Rgba::hex(0xDDDDDD), &DARK).expect("a light rule moves");
        assert!(contrast(rule, DARK.surface) < 2.0 && rule.luminance() > DARK.surface.luminance());
        assert_eq!(border(Rgba::hex(0xD73A49), &DARK), None);
        // A near-black rule stands out as it did.
        let rule = border(Rgba::hex(0x222222), &DARK).expect("a dark rule moves");
        assert!(contrast(rule, DARK.surface) > 7.0, "{rule:?}");
        assert_eq!(border(Rgba::hex(0xDDDDDD), &LIGHT_THEME), None);
    }

    #[test]
    fn text_turns_light_only_where_it_would_vanish() {
        let s = DARK.surface;
        // Near-black text becomes the reader's own.
        assert_eq!(text(Rgba::hex(0x1F1F1F), Rgba::WHITE, s, &DARK), DARK.text);
        assert_eq!(text(Rgba::hex(0x24292F), Rgba::WHITE, s, &DARK), DARK.text);
        // Grey stays grey, standing out as much as it did.
        let grey = text(Rgba::hex(0x6E7781), Rgba::WHITE, s, &DARK);
        let was = contrast(Rgba::hex(0x6E7781), Rgba::WHITE);
        assert!(contrast(grey, s) >= was && grey.luminance() < DARK.text.luminance(), "{grey:?}");
        // Blue keeps its hue.
        let blue = text(Rgba::hex(0x0366D6), Rgba::WHITE, s, &DARK);
        assert!(contrast(blue, s) >= 4.5 && blue.b > blue.r, "{blue:?}");
        // Light text, and dark text on a kept highlight, stay.
        assert_eq!(text(Rgba::WHITE, Rgba::hex(0x24292F), Rgba::hex(0x24292F), &DARK), Rgba::WHITE);
        let yellow = Rgba::hex(0xFFD33D);
        assert_eq!(text(Rgba::hex(0x1F1F1F), yellow, yellow, &DARK), Rgba::hex(0x1F1F1F));
        // A light footer grey already reads on the dark surface.
        assert_eq!(text(Rgba::hex(0x999999), Rgba::WHITE, s, &DARK), Rgba::hex(0x999999));
        // Text hidden in its background (white on white) stays hidden.
        assert_eq!(text(Rgba::WHITE, Rgba::WHITE, s, &DARK), s);
        // Light mode keeps text as written.
        assert_eq!(text(Rgba::hex(0xDDDDDD), Rgba::WHITE, LIGHT_THEME.surface, &LIGHT_THEME), Rgba::hex(0xDDDDDD));
    }

    #[test]
    fn text_on_a_light_panel_turns_dark() {
        // Light text on a light panel that dark mode keeps light (a highlight): darker.
        let yellow = Rgba::hex(0xFFD33D);
        let shown = text(Rgba::hex(0xEEEEEE), Rgba::hex(0x333333), yellow, &DARK);
        assert!(contrast(shown, yellow) >= 4.5, "{shown:?}");
    }

    #[test]
    fn declared_schemes() {
        assert_eq!(schemes(["light dark"], []), Schemes { dark: true, only_light: false });
        assert_eq!(schemes(["only light"], []), Schemes { dark: false, only_light: true });
        assert_eq!(schemes(["light"], []), Schemes::default());
        assert!(schemes([], ["@media (prefers-color-scheme: dark) { body { color: #eee } }"]).dark);
        assert!(schemes([], [":root { color-scheme: light dark; }"]).dark);
        assert!(schemes([], [":root { Color-Scheme: Only Light }"]).only_light);
        assert!(!schemes([], ["@media (prefers-color-scheme: light) { }"]).dark);
        assert!(!schemes([], ["--my-color-scheme: dark;"]).dark);
        assert_eq!(schemes(["only light"], ["@media (prefers-color-scheme:dark){}"]), Schemes { dark: true, only_light: false });
    }

    #[test]
    fn paper_only_for_mail_that_cannot_move() {
        let none = Schemes::default();
        assert!(!needs_paper(none, false, 100, 10));
        assert!(needs_paper(none, false, 100, 60));
        assert!(needs_paper(none, true, 100, 0));
        assert!(needs_paper(Schemes { only_light: true, ..none }, false, 100, 0));
        assert!(!needs_paper(none, false, 0, 0));
    }

    #[test]
    fn css_values() {
        assert_eq!(Rgba::hex(0x0366D6).css(), "rgba(3, 102, 214, 1)");
        assert_eq!(Rgba { a: 0.05, ..Rgba::WHITE }.css(), "rgba(255, 255, 255, 0.05)");
    }
}
