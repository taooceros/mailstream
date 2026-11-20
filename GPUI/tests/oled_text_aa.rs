use gpui::{
    AtlasKey, AtlasTextureKind, FontId, GlyphId, Point, RenderGlyphParams,
    get_gamma_correction_ratios, px,
};

#[test]
fn gamma_correction_ratios_linear_interpolation() {
    let r14 = get_gamma_correction_ratios(1.4);
    let r145 = get_gamma_correction_ratios(1.45);
    let r15 = get_gamma_correction_ratios(1.5);
    for i in 0..4 {
        let expected = (r14[i] + r15[i]) * 0.5;
        assert!(
            (r145[i] - expected).abs() < 1e-5,
            "channel {i}: got {}, expected {expected}",
            r145[i]
        );
    }

    // Monotonicity across [1.0, 2.2]: channels 0 and 2 increase, channels 1 and 3 decrease.
    let mut prev = get_gamma_correction_ratios(1.0);
    for step in 1..=24 {
        let g = 1.0 + step as f32 * 0.05;
        let cur = get_gamma_correction_ratios(g);
        assert!(cur[0] >= prev[0] - 1e-6, "ch0 non-monotonic at {g}");
        assert!(cur[1] <= prev[1] + 1e-6, "ch1 non-monotonic at {g}");
        assert!(cur[2] >= prev[2] - 1e-6, "ch2 non-monotonic at {g}");
        assert!(cur[3] <= prev[3] + 1e-6, "ch3 non-monotonic at {g}");
        prev = cur;
    }

    // Clamping outside [1.0, 2.2]
    assert_eq!(
        get_gamma_correction_ratios(0.5),
        get_gamma_correction_ratios(1.0)
    );
    assert_eq!(
        get_gamma_correction_ratios(3.0),
        get_gamma_correction_ratios(2.2)
    );
}

#[test]
fn grayscale_glyph_uses_monochrome_atlas() {
    let key = AtlasKey::Glyph(RenderGlyphParams {
        font_id: FontId(0),
        glyph_id: GlyphId(65),
        font_size: px(16.0),
        subpixel_variant: Point::default(),
        scale_factor: 3.0,
        is_emoji: false,
        subpixel_rendering: false,
        dilation: 0,
    });
    assert_eq!(key.texture_kind(), AtlasTextureKind::Monochrome);
}
