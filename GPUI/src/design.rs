//! The app's look, ported from Flutter's `Flutter/lib/ui/design.dart` (MS-182–192, MS-200): the
//! color roles and elevations of both appearances ([`Palette`], Flutter's `MailColors`), the scales
//! ([`radius`], [`space`], [`stroke`], [`icon`]), the type, the motion and the metrics of the window's
//! islands, rows, marks, avatars, tags and notices. Every literal in the UI comes from here; use the
//! nearest step of a scale, and add a token rather than a literal.

use std::sync::LazyLock;
use std::time::Duration;

use gpui::{App, BoxShadow, Hsla, WindowAppearance, WindowBackgroundAppearance, point, px, rgba};

pub struct Palette {
    pub dark: bool,
    /// The window behind the islands, where the sidebar sits, where nothing shows through it; over
    /// a blur (an older Mac, a Linux desktop that blurs) only a wash of it ([`window_fill`]).
    pub window: Hsla,
    /// The tint of the Mac window's Liquid Glass (`glass.rs`, MS-242): warm, so the sidebar and
    /// the gaps read cosy rather than gray whatever the desktop behind them.
    pub glass_tint: Hsla,
    /// Islands (the conversation), sheets, fields, menus.
    pub background: Hsla,
    /// The list's island, a shade under `background`, so rows can lift off it.
    pub tray: Hsla,
    /// The label color.
    pub text: Hsla,
    /// The secondary label: small print, counts, dates.
    pub muted: Hsla,
    /// Glyphs on toolbar capsules and icon buttons, a pill's label and a segmented control's
    /// choices: a step between the label and the secondary label, so controls don't read gray on
    /// gray (MS-241).
    pub glyph: Hsla,
    /// Placeholders and disabled glyphs.
    pub faint: Hsla,
    /// Field and control outlines.
    pub border: Hsla,
    /// Separators and hairline rims.
    pub line: Hsla,
    /// Quiet fills (message cards, the composer) and a pill's face.
    pub fill: Hsla,
    pub chip: Hsla,
    /// The unemphasized selection gray (a selected sidebar row).
    pub selection: Hsla,
    /// Hover and press washes over the sidebar, buttons and menu items; rows lift instead.
    pub hover: Hsla,
    pub pressed: Hsla,
    pub accent: Hsla,
    pub on_accent: Hsla,
    /// "Waiting on you": the system indigo.
    pub waiting: Hsla,
    pub waiting_fill: Hsla,
    pub on_waiting_fill: Hsla,
    pub danger: Hsla,
    pub on_red_fill: Hsla,
    /// Notices floating over the window.
    pub toast: Hsla,
    pub on_toast: Hsla,
    pub star: Hsla,
    /// A card resting on the tray (the calendar's events, Up Next): white in light mode, a step
    /// above the tray and the islands in dark (Flutter's `MailColors.card`).
    pub card: Hsla,
    /// A hairline around cards resting on the tray (a half-lifted row).
    pub card_border: Hsla,
    /// The paper HTML mail is drawn on: Blitz paints its pages white, as mail is written for
    /// white; dark mode shows it as a sheet with room around the mail (MS-256).
    pub page: Hsla,
    /// The opaque face of a toolbar capsule (and the sidebar's count badge), inside its rim.
    pub capsule: Hsla,
    pub capsule_rim: Hsla,
    /// A toolbar capsule's glass face floating over content (macOS 26's toolbar groups, MS-242):
    /// lit from above, top to bottom, all but opaque, so what scrolls under it shows faintly
    /// without blurring (a backdrop blur cost Flutter 2–3 ms a frame, MS-187).
    pub capsule_glass: [Hsla; 2],
    /// The capsule's specular top edge (an inset highlight) over the resting card's shadow.
    pub capsule_shadow: Vec<BoxShadow>,
    /// The capsule floating over a row (its hover actions): lit from above, top to bottom.
    pub raised_capsule: [Hsla; 2],
    pub raised_capsule_rim: Hsla,
    /// The rim just outside an island.
    pub island_rim: Hsla,
    /// What the user opened or selected on a tray (MS-189): a face lit from above, top to bottom,
    /// in a `stroke::RIM` of `lifted_rim`, on the `lifted_shadow`.
    pub lifted_card: [Hsla; 2],
    pub lifted_rim: Hsla,
    /// A row under the pointer lifts half as far: this face, a `card_border` hairline and the
    /// `raised_shadow`. Opaque, so the shadow doesn't show through.
    pub half_lifted: Hsla,
    /// A card raised a step past a resting one (today's date in the calendar).
    pub raised_card: Hsla,
    /// The light over today's column on the calendar's timeline.
    pub spotlight: Hsla,
    // Elevation, lowest first.
    /// A resting card.
    pub card_shadow: Vec<BoxShadow>,
    /// A control raised off its surface, a row half lifted under the pointer.
    pub raised_shadow: Vec<BoxShadow>,
    /// A lifted card.
    pub lifted_shadow: Vec<BoxShadow>,
    /// An island on the window.
    pub island_shadow: Vec<BoxShadow>,
    /// Popovers, menus, tooltips and toasts.
    pub popover_shadow: Vec<BoxShadow>,
    /// A page sliding over the list within its island (an open bundle): its leading edge.
    pub page_shadow: Vec<BoxShadow>,
}

/// `0xAARRGGBB`, as the Flutter tokens are written.
fn argb(value: u32) -> Hsla {
    rgba(value.rotate_left(8)).into()
}

fn shadow(color: u32, y: f32, blur: f32) -> BoxShadow {
    BoxShadow { color: argb(color), offset: point(px(0.), px(y)), blur_radius: px(blur), spread_radius: px(0.), inset: false }
}

/// A highlight along an edge's inside: `y` points down from the top edge.
fn inset(color: u32, y: f32) -> BoxShadow {
    BoxShadow { inset: true, ..shadow(color, y, 0.) }
}

// Both palettes are warm (MS-241: "too gray, I'd like it cosier"): the neutrals lean toward linen
// and cocoa instead of Apple's cool grays, the hairlines and shadows toward brown; the accent and
// the status colors stay the system's.
static LIGHT: LazyLock<Palette> = LazyLock::new(|| Palette {
    dark: false,
    window: argb(0xFFF1EBE3),
    glass_tint: argb(0x73FBEFDF),
    background: argb(0xFFFFFDF9),
    tray: argb(0xFFF8F3EC),
    text: argb(0xFF2B2420),
    muted: argb(0xFF7A6F67),
    glyph: argb(0xFF564A42),
    faint: argb(0x402B2420),
    border: argb(0xFFD8CEC3),
    line: argb(0xFFEEE6DC),
    fill: argb(0xFFF7F1E9),
    chip: argb(0x0F6B4524),
    selection: argb(0x1C6B4524),
    hover: argb(0x0F6B4524),
    pressed: argb(0x1F6B4524),
    accent: argb(0xFF007AFF),
    on_accent: argb(0xFFFFFFFF),
    waiting: argb(0xFF5856D6),
    waiting_fill: argb(0xFFECEBFB),
    on_waiting_fill: argb(0xFF3634A3),
    danger: argb(0xFFFF3B30),
    on_red_fill: argb(0xFFB3261E),
    toast: argb(0xFF332B26),
    on_toast: argb(0xFFF8F2EA),
    star: argb(0xFFFFB800),
    card: argb(0xFFFFFDF9),
    page: argb(0xFFFFFFFF),
    card_border: argb(0x173C2A1C),
    capsule: argb(0xFFFFFDF9),
    capsule_rim: argb(0x173C2A1C),
    capsule_glass: [argb(0xF5FFFDF9), argb(0xE6FAF4EC)],
    capsule_shadow: vec![inset(0xE6FFFFFF, 1.), shadow(0x123C2A1C, 1., 3.), shadow(0x0C3C2A1C, 4., 12.)],
    raised_capsule: [argb(0xFFFFFDF9), argb(0xFFF6F0E8)],
    raised_capsule_rim: argb(0x1C3C2A1C),
    island_rim: argb(0x143C2A1C),
    lifted_card: [argb(0xFFFFFDF9), argb(0xFFFBF9F7)],
    lifted_rim: argb(0x52007AFF),
    half_lifted: argb(0xFFFFFAF4),
    raised_card: argb(0xFFFFFDF9),
    spotlight: argb(0xA6FFFDF8),
    card_shadow: vec![shadow(0x123C2A1C, 1., 3.)],
    raised_shadow: vec![shadow(0x163C2A1C, 1., 2.), shadow(0x163C2A1C, 4., 10.)],
    lifted_shadow: vec![shadow(0x123C2A1C, 1., 2.), shadow(0x170055FF, 4., 14.)],
    island_shadow: vec![shadow(0x0F3C2A1C, 0.5, 1.), shadow(0x163C2A1C, 4., 18.)],
    popover_shadow: vec![shadow(0x363C2A1C, 6., 18.)],
    page_shadow: vec![BoxShadow { offset: point(px(-2.), px(0.)), ..shadow(0x1C3C2A1C, 0., 12.) }],
});

static DARK: LazyLock<Palette> = LazyLock::new(|| Palette {
    dark: true,
    window: argb(0xFF16120F),
    glass_tint: argb(0x802A1F17),
    background: argb(0xFF231E1B),
    tray: argb(0xFF1C1815),
    text: argb(0xFFF6EFE7),
    muted: argb(0xFFA59B92),
    glyph: argb(0xFFDDD4CA),
    faint: argb(0x40F6EFE7),
    border: argb(0xFF5C524A),
    line: argb(0xFF3B342E),
    fill: argb(0xFF2D2723),
    chip: argb(0x1AFFE8D0),
    selection: argb(0x24FFE8D0),
    hover: argb(0x14FFE8D0),
    pressed: argb(0x24FFE8D0),
    accent: argb(0xFF0A84FF),
    on_accent: argb(0xFFFFFFFF),
    waiting: argb(0xFF7D7AFF),
    waiting_fill: argb(0xFF26254D),
    on_waiting_fill: argb(0xFFD0CFFF),
    danger: argb(0xFFFF453A),
    on_red_fill: argb(0xFFFFB4AB),
    toast: argb(0xFFEFE7DD),
    on_toast: argb(0xFF2B2420),
    star: argb(0xFFFFB800),
    card: argb(0xFF2A2420),
    page: argb(0xFFFFFFFF),
    card_border: argb(0x1AFFEEDD),
    capsule: argb(0xFF312A26),
    capsule_rim: argb(0x1FFFEEDD),
    capsule_glass: [argb(0xF03E3631), argb(0xE6302924)],
    capsule_shadow: vec![inset(0x2EFFF4E8, 1.), shadow(0x33000000, 1., 3.), shadow(0x26000000, 4., 12.)],
    raised_capsule: [argb(0xFF473E38), argb(0xFF39302B)],
    raised_capsule_rim: argb(0x1FFFEEDD),
    island_rim: argb(0x17FFEEDD),
    lifted_card: [argb(0xFF332B26), argb(0xFF2B2520)],
    lifted_rim: argb(0x4D0A84FF),
    half_lifted: argb(0xFF2D2723),
    raised_card: argb(0xFF342C27),
    spotlight: argb(0x06FFEEDD),
    card_shadow: vec![shadow(0x33000000, 1., 3.)],
    raised_shadow: vec![shadow(0x66000000, 1., 2.), shadow(0x4D000000, 4., 10.)],
    lifted_shadow: vec![shadow(0x0F000000, 1., 2.), shadow(0x45000000, 4., 14.)],
    island_shadow: vec![shadow(0x4D000000, 0.5, 1.), shadow(0x59000000, 4., 18.)],
    popover_shadow: vec![shadow(0x33000000, 6., 18.)],
    page_shadow: vec![BoxShadow { offset: point(px(-2.), px(0.)), ..shadow(0x1A000000, 0., 12.) }],
});

pub fn palette(appearance: WindowAppearance) -> &'static Palette {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => &DARK,
        WindowAppearance::Light | WindowAppearance::VibrantLight => &LIGHT,
    }
}

/// What the window behind the islands is made of, the one place that chooses it (MS-241, MS-242,
/// MS-249).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowMaterial {
    /// [`Palette::window`], where the system offers neither: a Linux desktop that doesn't blur
    /// behind windows, so the desktop never shows through sharp.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Opaque,
    /// macOS 26's Liquid Glass under GPUI's view (`glass.rs`), as Flutter's window: the desktop
    /// shows through the sidebar and between the islands, in the system's glass, tinted for the
    /// appearance by the system.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Glass,
    /// The desktop blurred behind the window, under a wash ([`WINDOW_WASH`]): GPUI's blur on an
    /// older Mac; on Linux the compositor's, where it blurs translucent windows (Hyprland's
    /// `decoration:blur`, KWin; `blur.rs`).
    Blur,
}

pub fn window_material() -> WindowMaterial {
    #[cfg(target_os = "macos")]
    {
        if crate::glass::available() { WindowMaterial::Glass } else { WindowMaterial::Blur }
    }
    #[cfg(target_os = "linux")]
    {
        if crate::blur::blurs() { WindowMaterial::Blur } else { WindowMaterial::Opaque }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        WindowMaterial::Opaque
    }
}

/// The window's background for GPUI: transparent over the glass, which the app puts under it.
pub fn window_background() -> WindowBackgroundAppearance {
    match window_material() {
        WindowMaterial::Opaque => WindowBackgroundAppearance::Opaque,
        WindowMaterial::Glass => WindowBackgroundAppearance::Transparent,
        WindowMaterial::Blur => WindowBackgroundAppearance::Blurred,
    }
}

/// How much of [`Palette::window`] lies over the blur: the blur is the desktop alone, untinted, so
/// without a wash of the appearance's own color a light desktop would sit under the dark mode's
/// white sidebar text. The glass tints itself and takes none.
pub const WINDOW_WASH: f32 = 0.78;

/// What the app paints behind the islands: [`Palette::window`] where the window is opaque,
/// nothing over the glass, a [`WINDOW_WASH`] of it over the blur.
pub fn window_fill(p: &Palette) -> Hsla {
    match window_material() {
        WindowMaterial::Opaque => p.window,
        WindowMaterial::Glass => gpui::transparent_black(),
        WindowMaterial::Blur => tint(p.window, WINDOW_WASH),
    }
}

/// gpui-kit's theme in this palette (MS-226): the kit's components (buttons, switches, text fields,
/// tooltips) take their colors, corners and type from it. Run at launch and whenever the appearance
/// changes; it redraws every window. The rem is `text::BODY`, so the kit's type scale sits on the
/// app's: its base size is the body text and its small print the caption.
pub fn sync_kit_theme(appearance: WindowAppearance, cx: &mut App) {
    use gpui::component::{Theme, ThemeMode};
    let p = palette(appearance);
    let mode = if p.dark { ThemeMode::Dark } else { ThemeMode::Light };
    if Theme::global(cx).mode != mode {
        Theme::change(mode, None, cx);
    }
    // A filled face darkens toward black (white in dark mode) under the pointer and pressed.
    let shade = if p.dark { gpui::white() } else { gpui::black() };
    let hovered = |face: Hsla| face.blend(tint(shade, 0.08));
    let pressed = |face: Hsla| face.blend(tint(shade, 0.16));
    Theme::update(cx, |theme| {
        theme.font_family = ui_font().into();
        theme.font_size = px(text::BODY);
        theme.radius = px(radius::CONTROL);
        theme.radius_lg = px(radius::CARD);
        theme.shadow = false;
        // Toasts rise from the bottom of the window (`controls::toast`).
        theme.notification.margins.bottom = px(space::S24);
        let c = &mut theme.colors;
        // The kit's own surfaces under the app's are the islands' background, but the kit also
        // raises a segmented control's chosen choice on it: the raised card, the same white in
        // light mode and a step above the tray (not under it) in dark (MS-241). Windows paint
        // their own surface (`main.rs`, `settings.rs`), so nothing else of the app shows it.
        c.background = p.raised_card;
        c.foreground = p.text;
        c.muted = p.fill;
        c.muted_foreground = p.muted;
        c.border = p.line;
        c.input = p.border;
        c.ring = p.accent;
        c.caret = p.accent;
        c.selection = tint(p.accent, editor::SELECTION_ALPHA);
        c.popover = p.background;
        c.popover_foreground = p.text;
        // The wash under the pointer; the kit draws a quiet button's at half of it in dark mode.
        c.accent = if p.dark { tint(p.hover, 2.) } else { p.hover };
        c.accent_foreground = p.text;
        c.link = p.accent;
        c.link_hover = p.accent;
        c.link_active = p.accent;
        c.switch = p.border;
        c.switch_thumb = gpui::white();
        // A segmented control's track is the quiet fill; its choices the glyph tone, the chosen
        // one (and the one under the pointer) the text.
        c.tab_bar_segmented = p.chip;
        c.tab_foreground = p.glyph;
        c.tab_active_foreground = p.text;
        (c.primary, c.primary_hover, c.primary_active, c.primary_foreground) =
            (p.accent, hovered(p.accent), pressed(p.accent), p.on_accent);
        (c.danger, c.danger_hover, c.danger_active, c.danger_foreground) =
            (p.danger, hovered(p.danger), pressed(p.danger), p.on_accent);
        (c.secondary, c.secondary_hover, c.secondary_active, c.secondary_foreground) =
            (p.fill, p.fill.blend(p.hover), p.fill.blend(p.pressed), p.text);
        (c.button_primary, c.button_primary_hover, c.button_primary_active, c.button_primary_foreground) =
            (c.primary, c.primary_hover, c.primary_active, c.primary_foreground);
        (c.button_danger, c.button_danger_hover, c.button_danger_active, c.button_danger_foreground) =
            (c.danger, c.danger_hover, c.danger_active, c.danger_foreground);
        (c.button_secondary, c.button_secondary_hover, c.button_secondary_active, c.button_secondary_foreground) =
            (c.secondary, c.secondary_hover, c.secondary_active, c.secondary_foreground);
        // A quiet (ghost) button: the wash under the pointer is `accent`, pressed this.
        c.button_active = p.pressed;
    });
}

/// `color` at `alpha` of its own opacity: a tag's or a notice's wash of its hue.
pub fn tint(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: color.a * alpha, ..color }
}

/// The system's UI font: the Mac's own, and on Linux the desktop's sans-serif family as fontconfig
/// names it (Flutter's `MailFonts`); GPUI's stand-in there is IBM Plex Sans, rarely installed.
pub fn ui_font() -> &'static str {
    #[cfg(target_os = "macos")]
    return ".SystemUIFont";
    #[cfg(not(target_os = "macos"))]
    {
        static FAMILY: LazyLock<String> = LazyLock::new(|| {
            let mut fonts =
                fontique::Collection::new(fontique::CollectionOptions { shared: false, system_fonts: true });
            let sans = fonts.generic_families(fontique::GenericFamily::SansSerif).next();
            sans.and_then(|id| fonts.family_name(id).map(str::to_owned)).unwrap_or_else(|| ".SystemUIFont".into())
        });
        &FAMILY
    }
}

/// The type, in points (Flutter's `TextRoles`): `DISPLAY` titles a detail (a subject), `PANE_TITLE`
/// a list or panel, `BODY` (semibold for a headline) the UI's text, `READING` message text,
/// `FOOTNOTE` and `CAPTION` small print, `TAG` a tag's label.
pub mod text {
    pub const TAG: f32 = 10.0;
    pub const CAPTION: f32 = 11.0;
    pub const FOOTNOTE: f32 = 12.0;
    pub const BODY: f32 = 13.0;
    /// Message text in the reader, at `READING_LEADING`.
    pub const READING: f32 = 14.0;
    pub const READING_LEADING: f32 = 1.5;
    pub const PANE_TITLE: f32 = 15.0;
    /// The thread title.
    pub const DISPLAY: f32 = 22.0;
}

/// The reader's mail as a chat log (Flutter's `_ChatLog`) and its folded quotes (`_Fold`).
pub mod reader {
    /// The column the sender's avatar sits in.
    pub const LOG_GUTTER: f32 = 56.0;
    /// The "more" toggle that opens quoted history.
    pub const FOLD_WIDTH: f32 = 32.0;
    pub const FOLD_HEIGHT: f32 = 18.0;
    /// The rule left of opened quoted history, and the gap after it.
    pub const QUOTE_RULE: f32 = 2.0;
    pub const QUOTE_INDENT: f32 = 12.0;
    /// The scrim behind the toolbar once the conversation scrolls under it: the page's surface
    /// down to `SCRIM_SOLID` of its height, fading out below (MS-255).
    pub const SCRIM_HEIGHT: f32 = super::layout::HEADER_HEIGHT + 16.0;
    pub const SCRIM_SOLID: f32 = 0.6;
    /// The gap between the toolbar and the head of the message a thread opens at.
    pub const OPEN_GAP: f32 = 8.0;
    /// The room around an HTML body on its sheet, in dark mode.
    pub const SHEET_PADDING: f32 = 16.0;
}

/// Corner radii: `SMALL` for inline marks, `CONTROL` for buttons and controls, `CARD` for cards,
/// banners and lifted rows, `PANE` for the islands and message cards; capsules are fully round.
pub mod radius {
    pub const SMALL: f32 = 4.0;
    pub const CONTROL: f32 = 8.0;
    pub const CARD: f32 = 10.0;
    pub const PANE: f32 = 12.0;
}

/// The spacing scale for padding and gaps.
pub mod space {
    pub const S2: f32 = 2.0;
    pub const S4: f32 = 4.0;
    pub const S6: f32 = 6.0;
    pub const S8: f32 = 8.0;
    pub const S10: f32 = 10.0;
    pub const S12: f32 = 12.0;
    pub const S16: f32 = 16.0;
    pub const S20: f32 = 20.0;
    pub const S24: f32 = 24.0;
    pub const S32: f32 = 32.0;
}

/// Line widths: a `HAIRLINE` for separators, card and capsule rims; a `RIM` for the accent edge
/// of a lifted or focused thing.
pub mod stroke {
    pub const HAIRLINE: f32 = 0.5;
    pub const RIM: f32 = 1.0;
}

/// Animation durations and the curve.
pub mod motion {
    use super::Duration;
    /// A page sliding in over the list or beside it, and back out.
    pub const ENTER: Duration = Duration::from_millis(400);
    /// The calendar's days paging one page on (← and →).
    pub const PAGE: Duration = Duration::from_millis(180);
}

/// The conversation list's rows: Mail's two lines.
pub mod list {
    pub const ROW_HEIGHT: f32 = 60.0;
    pub const SECTION_HEIGHT: f32 = 30.0;
    pub const AVATAR: f32 = 36.0;
    /// The rows' inset from the island's edges.
    pub const INSET: f32 = 6.0;
    pub const TRAILING_INSET: f32 = 14.0;
    /// Lifted rows (selected, under the pointer) stay this far apart, their cards inset by half.
    pub const LIFT_GAP: f32 = 4.0;
    /// A selected row's accent bar at its card's start: `LIFT_BAR` wide, `LIFT_BAR_INSET` from the
    /// card's top and bottom.
    pub const LIFT_BAR: f32 = 3.0;
    pub const LIFT_BAR_INSET: f32 = 14.0;
    pub const AVATAR_GAP: f32 = 12.0;
    /// Where the hairline under a row starts: past the unread column and the avatar.
    pub const SEPARATOR_X: f32 = super::marks::UNREAD_SLOT + AVATAR + AVATAR_GAP;
    /// A date group's title, from the row's start.
    pub const SECTION_X: f32 = 22.0;
}

/// Marks (MS-190): an account or a topic is a `DOT` of its color; unread is an `UNREAD` accent dot
/// in an `UNREAD_SLOT` wide column before the avatar.
pub mod marks {
    pub const DOT: f32 = 6.0;
    pub const UNREAD: f32 = 7.0;
    pub const UNREAD_SLOT: f32 = 16.0;
}

/// Avatars (MS-192): a person's picture, else their initial in their hue at `LETTER` of the
/// avatar's size, on a `TINT` wash of that hue; a bundle's glyph at `GLYPH`.
pub mod avatar {
    pub const TINT: f32 = 0.15;
    pub const LETTER: f32 = 0.4;
    pub const GLYPH: f32 = 0.5;
}

/// Tags (MS-190): small print in its color on a `TINT` wash of it, fully round.
pub mod tag {
    pub const HEIGHT: f32 = 18.0;
    pub const TINT: f32 = 0.12;
    pub const PADDING: f32 = super::space::S6;
}

/// Notices (MS-191): a wash of the tone's color, `TINT` of it in light mode and `DARK_TINT` in
/// dark, inside a hairline of it at `RIM`; `COMPACT_HEIGHT` tall as one line over a view.
pub mod notice {
    pub const TINT: f32 = 0.10;
    pub const DARK_TINT: f32 = 0.14;
    pub const RIM: f32 = 0.22;
    pub const COMPACT_HEIGHT: f32 = 28.0;
}

/// Icon sizes: `INLINE` beside small print, `ROW` in rows and chips, `TOOLBAR` on toolbar buttons;
/// `BUTTON` is an icon button's square.
pub mod icon {
    pub const INLINE: f32 = 12.0;
    pub const ROW: f32 = 15.0;
    pub const TOOLBAR: f32 = 17.0;
    pub const BUTTON: f32 = 30.0;
}

/// The window as islands, after Dia: the sidebar sits on the window, and the list and the
/// conversation are rounded islands floating on it, `GAP` apart and from the window's edges.
pub mod layout {
    pub const GAP: f32 = super::space::S8;
    pub const SIDEBAR_WIDTH: f32 = 232.0;
    pub const SIDEBAR_ROW: f32 = 30.0;
    /// How far each level of the sidebar tree steps in.
    pub const SIDEBAR_INDENT: f32 = 18.0;
    /// The sidebar's top band, which moves the window (and on the Mac holds the traffic lights).
    pub const SIDEBAR_TOP: f32 = 44.0;
    /// The rail that stands in for the sidebar when it is hidden.
    pub const RAIL_WIDTH: f32 = 72.0;
    /// Room above the rail's first button for the Mac's traffic lights.
    #[cfg(target_os = "macos")]
    pub const RAIL_TOP: f32 = 36.0;
    #[cfg(not(target_os = "macos"))]
    pub const RAIL_TOP: f32 = 8.0;
    /// The list's title row, and the band at the top of a detail pane its toolbar floats in.
    pub const HEADER_HEIGHT: f32 = 56.0;
    /// A capsule of toolbar buttons: a `icon::BUTTON` control and its padding.
    pub const CAPSULE_HEIGHT: f32 = 36.0;
    pub const CAPSULE_PADDING: f32 = 3.0;
    /// From this window width the conversation opens beside the list instead of over it.
    pub const TWO_PANE_WIDTH: f32 = 1000.0;
    /// The narrowest list and page (the list's share is the engine's `gpui.listWidth`).
    pub const MIN_LIST: f32 = 300.0;
    pub const MIN_DETAIL: f32 = 420.0;
    /// The reader's text column.
    pub const READING_WIDTH: f32 = 720.0;
    pub const MENU_WIDTH: f32 = 236.0;
    pub const MENU_ROW: f32 = 26.0;
}

/// The chat log (`ReaderPage.chatLog`): a flat list, one row per message, runs under one head.
pub mod chat {
    /// The column the head's avatar sits in.
    pub const GUTTER: f32 = 34.0;
    pub const AVATAR: f32 = 30.0;
    /// Gap between the gutter and the text.
    pub const GAP: f32 = 12.0;
    /// Text line height, relative to its size.
    pub const LINE: f32 = 1.55;
    /// Messages from one person within this many seconds of the last one continue its run.
    pub const RUN_SECONDS: i64 = 3600;
    /// The composer's field.
    pub const COMPOSER_HEIGHT: f32 = 40.0;
    /// The space header above the log.
    pub const HEADER_HEIGHT: f32 = 56.0;
    /// Width of the new-chat form.
    pub const FORM_WIDTH: f32 = 520.0;
    /// A message still being sent.
    pub const PENDING_OPACITY: f32 = 0.55;
    /// The largest a picture takes in the log; one still downloading holds a `MEDIA_LOADING` box.
    pub const MEDIA_WIDTH: f32 = 360.0;
    pub const MEDIA_HEIGHT: f32 = 300.0;
    pub const MEDIA_LOADING: (f32, f32) = (240.0, 160.0);
    /// An attachment's tile: its width range and its kind's glyph.
    pub const TILE_MIN_WIDTH: f32 = 220.0;
    pub const TILE_MAX_WIDTH: f32 = 340.0;
    pub const TILE_GLYPH: f32 = 24.0;
    /// Rows from the space list's end at which the next page of spaces loads.
    pub const MORE_AHEAD: usize = 8;
    /// An attachment that opens nowhere.
    pub const DIMMED: f32 = 0.5;
}

/// The calendar (Flutter's `CalendarMetrics` and the agenda's sizes).
pub mod calendar {
    /// An agenda row.
    pub const ROW_HEIGHT: f32 = 52.0;
    /// The empty agenda's note.
    pub const NOTE_HEIGHT: f32 = 120.0;
    /// The agenda's time column.
    pub const TIME_WIDTH: f32 = 64.0;
    /// The week strip over the agenda, and the Up Next card.
    pub const WEEK_STRIP: f32 = 44.0;
    pub const UP_NEXT: f32 = 36.0;
    pub const CARD_RADIUS: f32 = super::radius::CONTROL;
    /// Guests' avatars overlapping on a row, each `AVATAR_STEP` of the last one's size on and
    /// ringed `AVATAR_RING` wide in the surface under them; a guest card's avatar.
    pub const AVATAR: f32 = 16.0;
    pub const AVATAR_STEP: f32 = 0.7;
    pub const AVATAR_RING: f32 = 1.5;
    pub const GUEST_AVATAR: f32 = 26.0;
    /// A day's load bar: `LOAD_BAR` thick, `LOAD_WIDTH` long when `FULL_DAY_MINUTES` are booked.
    pub const LOAD_BAR: f32 = 2.5;
    pub const LOAD_WIDTH: f32 = 24.0;
    pub const LOAD_OPACITY: f32 = 0.55;
    pub const FULL_DAY_MINUTES: f32 = 8.0 * 60.0;
    /// Past events fade, declined ones further; a past card on the timeline keeps its title and
    /// its face at `PAST_EMPHASIS` of an upcoming one's, so it reads rather than grays out (MS-241).
    pub const PAST_OPACITY: f32 = 0.48;
    pub const PAST_EMPHASIS: f32 = 0.72;
    pub const DECLINED_OPACITY: f32 = 0.40;
    /// A calendar's dot, this far toward the card under it.
    pub const DOT_SOFTEN: f32 = 0.2;
    /// The day and week views' timeline: an hour's height, the hours' gutter, the days' dates
    /// over it, an all-day bar (`ALL_DAY_ROWS` show, more scroll), the room over the first hour
    /// line and under the last.
    pub const HOUR_HEIGHT: f32 = 48.0;
    pub const GUTTER: f32 = 52.0;
    pub const HEADER_HEIGHT: f32 = 44.0;
    pub const ALL_DAY_HEIGHT: f32 = 22.0;
    pub const ALL_DAY_ROWS: usize = 3;
    pub const INSET: f32 = 8.0;
    /// Cards side by side stay this far apart.
    pub const CONFLICT_GAP: f32 = 2.0;
    /// An all-day bar's face: its calendar's color at this over the card.
    pub const ALL_DAY_TINT: f32 = 0.18;
    /// An unanswered invitation's card: the clean card, hatched at 45° in its calendar's color at
    /// `HATCH_OPACITY` every `HATCH_SPACING`, in a dashed border (Flutter's `_InvitedPainter`).
    pub const HATCH_OPACITY: f32 = 0.07;
    pub const HATCH_SPACING: f32 = 6.0;
    /// The time line: across today's column `NOW_LINE` thick, across the other days dashed
    /// (`DASH` long, as far apart) at `NOW_TRAIL_OPACITY`; its time on a `NOW_PILL` in the gutter.
    pub const NOW_TRAIL_OPACITY: f32 = 0.5;
    pub const NOW_PILL: f32 = 16.0;
    pub const NOW_LINE: f32 = 1.5;
    pub const DASH: f32 = 3.0;
    /// The week view's open event, floating beside its card (MS-184): `POPOVER_WIDTH` wide, at
    /// most `POPOVER_HEIGHT` tall (less in a short window, its notes and guests scrolling), and
    /// `POPOVER_GAP` off the card.
    pub const POPOVER_WIDTH: f32 = 360.0;
    pub const POPOVER_HEIGHT: f32 = 560.0;
    pub const POPOVER_GAP: f32 = 8.0;
}

/// The house easing curve (Flutter's `Motion.curve`), `cubic-bezier(0.05, 0.7, 0.1, 1)`: a strong
/// deceleration, quick off the mark, a long settle.
pub fn house_curve(progress: f32) -> f32 {
    const X1: f32 = 0.05;
    const Y1: f32 = 0.7;
    const X2: f32 = 0.1;
    const Y2: f32 = 1.0;
    fn bezier(t: f32, p1: f32, p2: f32) -> f32 {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    }
    // x(t) is monotonic on [0, 1]: bisect for the t whose x is `progress`.
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (low + high) / 2.0;
        if bezier(mid, X1, X2) < progress { low = mid } else { high = mid }
    }
    bezier((low + high) / 2.0, Y1, Y2)
}

/// A slot in `0..count` for `key` that is the same on every launch and matches the native app
/// (FNV-1a 64 over UTF-8).
pub fn stable_index(key: &str, count: usize) -> usize {
    let mut hash: u64 = 1_469_598_103_934_665_603;
    for byte in key.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211);
    }
    (hash % count as u64) as usize
}

const AVATAR_LIGHT: [u32; 11] = [
    0xFF007AFF, 0xFFAF52DE, 0xFFFF2D55, 0xFFFF3B30, 0xFFFF9500, 0xFF34C759, 0xFF30B0C7, 0xFF5856D6, 0xFFA2845E,
    0xFF00C7BE, 0xFF32ADE6,
];
const AVATAR_DARK: [u32; 11] = [
    0xFF0A84FF, 0xFFBF5AF2, 0xFFFF375F, 0xFFFF453A, 0xFFFF9F0A, 0xFF30D158, 0xFF40C8E0, 0xFF5E5CE6, 0xFFAC8E68,
    0xFF63E6E2, 0xFF64D2FF,
];

/// A person's hue (`MailColors.avatarHue`): system colors by a stable hash of the lowercased
/// address (else the name).
pub fn avatar_color(email: &str, dark: bool) -> Hsla {
    let palette = if dark { &AVATAR_DARK } else { &AVATAR_LIGHT };
    argb(palette[stable_index(&email.to_lowercase(), palette.len())])
}

/// Stable per-account colors, in sidebar order: the system blue, orange, purple, green, red, teal.
pub fn account_color(index: usize) -> Hsla {
    const PALETTE: [u32; 6] = [0xFF007AFF, 0xFFFF9500, 0xFFAF52DE, 0xFF34C759, 0xFFFF3B30, 0xFF30B0C7];
    argb(PALETTE[index % PALETTE.len()])
}

/// Topic colors: known topics keep fixed hues, the rest take a stable one.
pub fn topic_color(label: &str, dark: bool) -> Hsla {
    const KNOWN: [(&str, u32, u32); 9] = [
        ("work", 0xFF3B82F6, 0xFF60A5FA),
        ("education", 0xFFA855F7, 0xFFC084FC),
        ("finance", 0xFF10B981, 0xFF34D399),
        ("shopping", 0xFFF59E0B, 0xFFFBBF24),
        ("tech", 0xFF06B6D4, 0xFF22D3EE),
        ("travel", 0xFFEC4899, 0xFFF472B6),
        ("food", 0xFFF97316, 0xFFFB923C),
        ("health", 0xFF14B8A6, 0xFF2DD4BF),
        ("social", 0xFF8B5CF6, 0xFFA78BFA),
    ];
    const FALLBACK: [(u32, u32); 4] =
        [(0xFF64748B, 0xFF94A3B8), (0xFF6366F1, 0xFF818CF8), (0xFF84CC16, 0xFFA3E635), (0xFFE11D48, 0xFFFB7185)];
    let key = label.to_lowercase();
    let (light, dark_color) = KNOWN
        .iter()
        .find(|(name, _, _)| *name == key)
        .map(|(_, l, d)| (*l, *d))
        .unwrap_or_else(|| FALLBACK[stable_index(&key, FALLBACK.len())]);
    argb(if dark { dark_color } else { light })
}

/// A Smart Inbox bundle's color: its glyph sits white on it, as Mail marks its categories.
pub fn category_color(category: crate::model::Category, p: &Palette) -> Hsla {
    use crate::model::Category;
    match category {
        Category::People => p.accent,
        Category::Notification => argb(0xFF5AC8FA),
        Category::Newsletter => argb(0xFFFF2D55),
        Category::Invitation => argb(0xFFFF9500),
        Category::Other => argb(0xFF8E8E93),
    }
}

/// What a file is, for the color of its glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    Pdf,
    Document,
    File,
}

/// Attachments' colors by kind (MS-191), light and dark, after the system hues the rest of the
/// app uses: pictures purple, videos pink, audio orange, PDFs red, documents (a Drive file) blue,
/// any other file gray.
pub fn attachment_color(kind: AttachmentKind, dark: bool) -> Hsla {
    let (light, night) = match kind {
        AttachmentKind::Image => (0xFFAF52DE, 0xFFBF5AF2),
        AttachmentKind::Video => (0xFFFF2D55, 0xFFFF375F),
        AttachmentKind::Audio => (0xFFFF9500, 0xFFFF9F0A),
        AttachmentKind::Pdf => (0xFFFF3B30, 0xFFFF453A),
        AttachmentKind::Document => (0xFF007AFF, 0xFF0A84FF),
        AttachmentKind::File => (0xFF8E8E93, 0xFF98989D),
    };
    argb(if dark { night } else { light })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn house_curve_runs_from_rest_to_rest_and_lands_softly() {
        assert!(house_curve(0.0).abs() < 1e-4);
        assert!((house_curve(1.0) - 1.0).abs() < 1e-4);
        // Quick start: most of the move is done in the first third.
        assert!(house_curve(0.33) > 0.8);
    }
}

/// The Settings window.
pub mod settings {
    pub const WIDTH: f32 = 560.0;
    pub const HEIGHT: f32 = 620.0;
    pub const PADDING: f32 = 24.0;
    /// The account color dot.
    pub const DOT: f32 = 10.0;
    pub const ROW_HEIGHT: f32 = 56.0;
}

/// Written text (the kit's text fields, `input.rs`).
pub mod editor {
    /// The accent's opacity behind selected text.
    pub const SELECTION_ALPHA: f32 = 0.3;
    /// Line height of written text, as a multiple of its size (the composer's `1.55`).
    pub const LEADING: f32 = 1.55;
}

/// The compose column (Flutter's `ComposePane`), its drafts and the reader's inline reply.
pub mod compose {
    use std::time::Duration;

    /// The docked column: as it opens, the narrowest and the widest it is dragged to.
    pub const COLUMN_WIDTH: f32 = 440.0;
    pub const MIN_COLUMN: f32 = 340.0;
    pub const MAX_COLUMN: f32 = 760.0;
    /// The room the column always leaves the list and the conversation, and the room they need to
    /// stay side by side beside it (below, the conversation slides over the list).
    pub const MIN_MAIL: f32 = 360.0;
    pub const TWO_PANE_MAIL: f32 = 700.0;
    /// While the column is docked the sidebar shows only from this window width, a rail below.
    pub const SIDEBAR_WIDTH: f32 = 1440.0;
    /// The grip in the gap beside the docked column, under the pointer.
    pub const GRIP_WIDTH: f32 = 3.0;
    pub const GRIP_HEIGHT: f32 = 36.0;
    /// The floating draft: its size as it first floats, the smallest and largest it is sized to,
    /// and its distance from the window's edges.
    pub const FLOAT_WIDTH: f32 = 500.0;
    pub const FLOAT_HEIGHT: f32 = 560.0;
    pub const MIN_FLOAT: f32 = 380.0;
    pub const MAX_FLOAT: f32 = 760.0;
    pub const FLOAT_MARGIN: f32 = 16.0;
    /// The floating draft's top-left corner, which sizes it.
    pub const FLOAT_HANDLE: f32 = 14.0;
    /// The tab row over several drafts: its height, a tab's widest title, its glyphs.
    pub const TAB_ROW: f32 = 36.0;
    pub const TAB_TITLE: f32 = 140.0;
    pub const TAB_GLYPH: f32 = 12.0;
    pub const TAB_CLOSE: f32 = 16.0;
    /// The minimized draft's capsule in the window's bottom-right corner: its widest title.
    pub const CAPSULE_TITLE: f32 = 220.0;
    /// Leading inset of the header's labels and the body's text.
    pub const MARGIN: f32 = 20.0;
    /// The header's right-aligned `To:` labels.
    pub const LABEL_WIDTH: f32 = 52.0;
    pub const ROW_HEIGHT: f32 = 34.0;
    pub const SUBJECT_HEIGHT: f32 = 44.0;
    pub const BODY_PADDING: f32 = 14.0;
    /// Recipient completion: how many people, the menu's width, a row's height and avatar.
    pub const SUGGESTIONS: usize = 6;
    pub const MENU_WIDTH: f32 = 340.0;
    pub const MENU_ROW: f32 = 44.0;
    pub const MENU_AVATAR: f32 = 24.0;
    /// Typing pauses this long before the directory is asked.
    pub const COMPLETION_DELAY: Duration = Duration::from_millis(120);
    /// The inline reply: rows the text starts with and grows to (about 360 points of written
    /// text), then it scrolls.
    pub const REPLY_MIN_ROWS: usize = 3;
    pub const REPLY_MAX_ROWS: usize = 15;
    pub const REPLY_PADDING: f32 = 14.0;
    /// The reply card's border while it has the keyboard: the accent at this opacity.
    pub const FOCUS_RING_ALPHA: f32 = 0.45;
    /// Text being sent.
    pub const SENT_TEXT_OPACITY: f32 = 0.6;
    /// The Send button's ⌘↩ hint.
    pub const KEY_HINT_OPACITY: f32 = 0.72;
    /// Files dragged over a draft: the target's inset, its accent face's opacity and its glyph.
    pub const DROP_INSET: f32 = 8.0;
    pub const DROP_FILL_ALPHA: f32 = 0.92;
    pub const DROP_GLYPH: f32 = 36.0;
}
