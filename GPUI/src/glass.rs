//! The Mac window's Liquid Glass (MS-242), as Flutter's window has it (`BackdropGlass` in
//! `Flutter/macos/Runner/MainFlutterWindow.swift`): macOS 26's `NSGlassEffectView` fills the
//! window under GPUI's view, which is transparent wherever the app paints nothing (the sidebar,
//! the gaps around the islands), so the desktop shows through there in the system's glass. The
//! glass takes the window's appearance from AppKit; its tint is the palette's warm
//! [`Palette::glass_tint`](crate::design::Palette::glass_tint), set again when the appearance
//! changes. `design::window_material` picks it when the system has the class; an older Mac keeps
//! GPUI's blur.

use std::cell::RefCell;
use std::sync::LazyLock;

use gpui::{Rgba, Window};
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSColor, NSGlassEffectView, NSWindowOrderingMode};

use crate::design;

/// The glass's corners: the window's own, which clips it anyway; Flutter's glass has the same.
const CORNER: f64 = 16.;

thread_local! {
    /// The main window's glass, to tint when the appearance changes.
    static GLASS: RefCell<Option<Retained<NSGlassEffectView>>> = const { RefCell::new(None) };
}

/// Whether this Mac has Liquid Glass (macOS 26 and later); asked once, as every frame's
/// `design::window_fill` reads it.
pub fn available() -> bool {
    static AVAILABLE: LazyLock<bool> = LazyLock::new(|| AnyClass::get(c"NSGlassEffectView").is_some());
    *AVAILABLE
}

/// Puts the glass under the window's GPUI view, filling the window and following its size, in the
/// tint of the window's appearance. The window's background must be transparent
/// (`design::window_background`).
pub fn install(window: &Window) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if !available() {
        return;
    }
    let Some(host) = crate::webview::host_view(window) else { return };
    let Some(content) = (unsafe { host.superview() }) else { return };
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), content.bounds());
    glass.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    glass.setCornerRadius(CORNER);
    content.addSubview_positioned_relativeTo(&glass, NSWindowOrderingMode::Below, Some(&host));
    GLASS.set(Some(glass));
    follow_appearance(window);
}

/// Tints the glass for the window's appearance; nothing where there is none.
pub fn follow_appearance(window: &Window) {
    let tint = Rgba::from(design::palette(window.appearance()).glass_tint);
    GLASS.with_borrow(|glass| {
        let Some(glass) = glass else { return };
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(tint.r.into(), tint.g.into(), tint.b.into(), tint.a.into());
        glass.setTintColor(Some(&color));
    });
}
