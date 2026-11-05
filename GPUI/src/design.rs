//! The native app's `Design` tokens (Sources/Mailstream/UI/Design.swift), ported: semantic colors
//! for both appearances, the type scale, radii, durations, the house curve and list metrics. Every
//! literal in the UI comes from here.

use std::sync::LazyLock;
use std::time::Duration;

use gpui::{Hsla, WindowAppearance, rgba};

pub struct Palette {
    pub dark: bool,
    /// The reading surface: the reader page, and what a sliding list uncovers.
    pub page: Hsla,
    pub sidebar: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    /// Hairlines between messages, card borders, separators.
    pub line: Hsla,
    /// Row and button hover fill.
    pub hover: Hsla,
    /// Quiet fill for chips and thumbnails.
    pub chip: Hsla,
    /// The selected sidebar row and the keyboard highlight.
    pub selection: Hsla,
    pub accent: Hsla,
    pub danger: Hsla,
    pub star: Hsla,
    pub waiting: Hsla,
    pub on_accent: Hsla,
    /// The shadow under a page sliding over another.
    pub shadow: Hsla,
    /// Menus and tooltips floating over the window.
    pub popover: Hsla,
}

/// `0xAARRGGBB`, as the native tokens are written.
fn argb(value: u32) -> Hsla {
    rgba(value.rotate_left(8)).into()
}

static LIGHT: LazyLock<Palette> = LazyLock::new(|| Palette {
    dark: false,
    page: argb(0xFFFFFFFF),
    sidebar: argb(0xFFF1F1F3),
    text: argb(0xD9000000),
    muted: argb(0x80000000),
    faint: argb(0x40000000),
    line: argb(0x1A000000),
    hover: argb(0x0A000000),
    chip: argb(0x0F000000),
    selection: argb(0x14000000),
    accent: argb(0xFF007AFF),
    danger: argb(0xFFFF3B30),
    star: argb(0xFFFFCC00),
    waiting: argb(0xFFFF9500),
    on_accent: argb(0xFFFFFFFF),
    shadow: argb(0x1F000000),
    popover: argb(0xFFFCFCFD),
});

static DARK: LazyLock<Palette> = LazyLock::new(|| Palette {
    dark: true,
    page: argb(0xFF1E1E1E),
    sidebar: argb(0xFF282829),
    text: argb(0xD9FFFFFF),
    muted: argb(0x8CFFFFFF),
    faint: argb(0x40FFFFFF),
    line: argb(0x1AFFFFFF),
    hover: argb(0x0DFFFFFF),
    chip: argb(0x14FFFFFF),
    selection: argb(0x1FFFFFFF),
    accent: argb(0xFF0A84FF),
    danger: argb(0xFFFF453A),
    star: argb(0xFFFFD60A),
    waiting: argb(0xFFFF9F0A),
    on_accent: argb(0xFFFFFFFF),
    shadow: argb(0x59000000),
    popover: argb(0xFF2C2C2E),
});

pub fn palette(appearance: WindowAppearance) -> &'static Palette {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => &DARK,
        WindowAppearance::Light | WindowAppearance::VibrantLight => &LIGHT,
    }
}

/// The system font.
pub const UI_FONT: &str = ".SystemUIFont";

/// The type scale, in points.
pub mod text {
    pub const MICRO: f32 = 10.5;
    pub const CAPTION: f32 = 11.0;
    pub const FOOTNOTE: f32 = 12.0;
    pub const BODY: f32 = 13.0;
    /// Message text in the reader.
    pub const READING: f32 = 14.0;
    pub const HEADLINE: f32 = 15.0;
    /// The thread title.
    pub const DISPLAY: f32 = 27.0;
}

/// Corner radii.
pub mod radius {
    pub const SMALL: f32 = 4.0;
    pub const CONTROL: f32 = 8.0;
    pub const CARD: f32 = 10.0;
}

/// Animation durations.
pub mod motion {
    use super::Duration;
    /// Folds and slides.
    pub const STANDARD: Duration = Duration::from_millis(300);
}

/// The conversation list's fixed columns (`ListMetrics`).
pub mod list {
    pub const ROW_HEIGHT: f32 = 38.0;
    pub const DOT_X: f32 = 14.0;
    pub const SENDER_X: f32 = 28.0;
    pub const AVATAR: f32 = 28.0;
    pub const SENDER_WIDTH: f32 = 196.0;
    pub const NAME_X: f32 = SENDER_X + AVATAR + 9.0;
    pub const SUBJECT_X: f32 = SENDER_X + SENDER_WIDTH + 16.0;
    pub const DATE_WIDTH: f32 = 62.0;
    pub const INSET: f32 = 10.0;
    pub const TRAILING_INSET: f32 = 14.0;
}

/// Icon sizes.
pub mod icon {
    pub const SMALL: f32 = 16.0;
    pub const TOOLBAR: f32 = 18.0;
    pub const BUTTON: f32 = 30.0;
}

/// The window's fixed regions.
pub mod layout {
    pub const SIDEBAR_WIDTH: f32 = 220.0;
    pub const SIDEBAR_ROW: f32 = 28.0;
    /// How far each level of the sidebar tree steps in.
    pub const SIDEBAR_INDENT: f32 = 14.0;
    /// The toolbar, which is also the title bar the traffic lights sit in.
    pub const TOOLBAR_HEIGHT: f32 = 52.0;
    /// Room for the traffic lights when the sidebar is hidden.
    pub const TRAFFIC_LIGHTS: f32 = 78.0;
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
    /// The per-account mark in rows when several accounts are listed.
    pub const ACCOUNT_DOT: f32 = 6.0;
    /// A message still being sent.
    pub const PENDING_OPACITY: f32 = 0.55;
}

/// The house easing curve, `cubic-bezier(0.2, 0.9, 0.25, 1)`: quick start, long soft landing.
pub fn house_curve(progress: f32) -> f32 {
    const X1: f32 = 0.2;
    const Y1: f32 = 0.9;
    const X2: f32 = 0.25;
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

/// Sender avatar colors (`Avatar.color`): system colors by a stable hash of the address.
pub fn avatar_color(email: &str, dark: bool) -> Hsla {
    let palette = if dark { &AVATAR_DARK } else { &AVATAR_LIGHT };
    argb(palette[stable_index(&email.to_lowercase(), palette.len())])
}

/// Stable per-account colors, in sidebar order (`AccountStyle`).
pub fn account_color(index: usize) -> Hsla {
    const PALETTE: [u32; 6] = [0xFF007AFF, 0xFFFF9500, 0xFFAF52DE, 0xFF34C759, 0xFFFF2D55, 0xFF30B0C7];
    argb(PALETTE[index % PALETTE.len()])
}

/// Muted topic colors (`TagPalette`): known topics keep fixed hues.
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
    /// `accounts.add` waits this long on the browser.
    pub const SIGN_IN_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60 + 10);
}

/// The multi-line text editor (`textarea.rs`).
pub mod editor {
    pub const CARET_WIDTH: f32 = 1.5;
    /// The accent's opacity behind selected text.
    pub const SELECTION_ALPHA: f32 = 0.3;
    /// How wide a selected line break shows.
    pub const NEWLINE_SELECTION: f32 = 5.0;
    /// Line height of written text, as a multiple of its size (the composer's `1.55`).
    pub const LEADING: f32 = 1.55;
}

/// The compose window and the reader's inline reply.
pub mod compose {
    use std::time::Duration;

    pub const WINDOW_WIDTH: f32 = 640.0;
    pub const WINDOW_HEIGHT: f32 = 560.0;
    pub const MIN_WIDTH: f32 = 480.0;
    pub const MIN_HEIGHT: f32 = 360.0;
    /// Where the traffic lights sit in the window's title bar row.
    pub const TRAFFIC_LIGHT_X: f32 = 16.0;
    pub const TRAFFIC_LIGHT_Y: f32 = 19.0;
    /// The title's start, clear of the traffic lights.
    pub const TITLE_INSET: f32 = 84.0;
    /// Leading inset of the header's labels and the body's text.
    pub const MARGIN: f32 = 20.0;
    /// The header's right-aligned `To:` labels.
    pub const LABEL_WIDTH: f32 = 52.0;
    pub const ROW_HEIGHT: f32 = 34.0;
    pub const SUBJECT_HEIGHT: f32 = 44.0;
    pub const BODY_MIN_ROWS: usize = 10;
    pub const BODY_PADDING: f32 = 14.0;
    /// The Send button and the confirm bar's buttons.
    pub const BUTTON_HEIGHT: f32 = 28.0;
    /// Recipient completion: how many people, the menu's width, a row's height and avatar.
    pub const SUGGESTIONS: usize = 6;
    pub const MENU_WIDTH: f32 = 340.0;
    pub const MENU_ROW: f32 = 44.0;
    pub const MENU_AVATAR: f32 = 24.0;
    /// Typing pauses this long before the directory is asked.
    pub const COMPLETION_DELAY: Duration = Duration::from_millis(120);
    /// The inline reply: rows the text starts with and the most the card grows to.
    pub const REPLY_MIN_ROWS: usize = 3;
    pub const REPLY_MAX_HEIGHT: f32 = 360.0;
    pub const REPLY_PADDING: f32 = 14.0;
    /// The reply card's border while it has the keyboard: the accent at this opacity.
    pub const FOCUS_RING_ALPHA: f32 = 0.45;
    /// The reply card's floating shadow.
    pub const CARD_SHADOW_Y: f32 = 2.0;
    pub const CARD_SHADOW_BLUR: f32 = 14.0;
    /// The Send button while a send is in flight.
    pub const SENDING_OPACITY: f32 = 0.55;
    /// Text being sent.
    pub const SENT_TEXT_OPACITY: f32 = 0.6;
    /// The Send button under the pointer, and its ⌘↩ hint.
    pub const SEND_HOVER_OPACITY: f32 = 0.9;
    pub const KEY_HINT_OPACITY: f32 = 0.72;
}
