// Modified by Longbridge for gpui-fast.
//! Paths drawn through the multisampled path pass, which discards its samples
//! once they are resolved (see `path_store_op`).

use gpui::{
    Bounds, ContentMask, DevicePixels, Hsla, Path, PlatformHeadlessRenderer as _, Quad,
    ScaledPixels, Scene, point, px, size,
};

use crate::WgpuHeadlessRenderer;

const RED: u32 = 0xff0000ff;
const BLUE: u32 = 0x0000ffff;

fn color(rgba: u32) -> Hsla {
    gpui::rgba(rgba).into()
}

fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
    Bounds {
        origin: point(ScaledPixels(x), ScaledPixels(y)),
        size: size(ScaledPixels(width), ScaledPixels(height)),
    }
}

fn everywhere() -> ContentMask<ScaledPixels> {
    ContentMask {
        bounds: bounds(0., 0., 4096., 4096.),
    }
}

fn rectangle_path(x: f32, y: f32, width: f32, height: f32, rgba: u32) -> Path<ScaledPixels> {
    let mut path = Path::new(point(px(x), px(y)));
    path.line_to(point(px(x + width), px(y)));
    path.line_to(point(px(x + width), px(y + height)));
    path.line_to(point(px(x), px(y + height)));
    path.line_to(point(px(x), px(y)));
    path.color = color(rgba).into();
    path.content_mask = ContentMask {
        bounds: Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(4096.), px(4096.)),
        },
    };
    path.scale(1.)
}

fn rectangle_quad(x: f32, y: f32, width: f32, height: f32, rgba: u32) -> Quad {
    Quad {
        bounds: bounds(x, y, width, height),
        content_mask: everywhere(),
        background: color(rgba).into(),
        ..Default::default()
    }
}

/// Two overlapping rectangles at `(x, y)`, drawn as paths or as quads, with a
/// quad over the first drawn before the second, so that as paths each is
/// rasterized in a pass of its own.
fn scene(x: f32, y: f32, as_paths: bool) -> Scene {
    let mut scene = Scene::default();
    let rectangle = |scene: &mut Scene, x, y, rgba| {
        if as_paths {
            scene.insert_primitive(rectangle_path(x, y, 24., 16., rgba));
        } else {
            scene.insert_primitive(rectangle_quad(x, y, 24., 16., rgba));
        }
    };
    rectangle(&mut scene, x, y, RED);
    scene.insert_primitive(rectangle_quad(x + 4., y + 4., 8., 8., 0x00ff00ff));
    rectangle(&mut scene, x + 12., y + 8., BLUE);
    scene.finish();
    scene
}

/// How many path rasterization passes `fast::frame` draws `scene` in.
fn path_passes(scene: &Scene) -> usize {
    let (mut vertices, mut sprites, mut batches) = (Vec::new(), Vec::new(), Vec::new());
    super::plan_paths(scene, &mut vertices, &mut sprites, &mut batches);
    batches.iter().filter(|batch| batch.group.is_some()).count()
}

fn assert_same_pixels(paths: &image::RgbaImage, quads: &image::RgbaImage, frame: usize) {
    assert_eq!(paths.dimensions(), quads.dimensions());
    for (x, y, path) in paths.enumerate_pixels() {
        let quad = quads.get_pixel(x, y);
        assert_eq!(
            path.0, quad.0,
            "frame {frame}, pixel ({x}, {y}): drawn as a path, as a quad"
        );
    }
}

/// Rectangles on whole pixels cover every sample of a pixel or none, so drawn
/// as paths through the multisampled pass they must come out exactly as quads
/// do: what the resolve writes is all the frame needs of the samples. Each
/// path pass starts from a cleared attachment, so the second frame, drawn
/// elsewhere, shows nothing of the first.
#[test]
fn multisampled_paths_on_whole_pixels_draw_as_quads_do() {
    let Ok(mut renderer) = WgpuHeadlessRenderer::new() else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    assert!(
        renderer.core.rendering_params.path_sample_count > 1,
        "the adapter rasterizes paths without multisampling, so the test shows nothing"
    );
    let target = size(DevicePixels(64), DevicePixels(64));
    for (frame, (x, y)) in [(4., 4.), (20., 30.)].into_iter().enumerate() {
        let path_scene = scene(x, y, true);
        assert_eq!(
            path_passes(&path_scene),
            2,
            "each path in a pass of its own"
        );
        let paths = renderer.render_scene_to_image(&path_scene, target).unwrap();
        let quads = renderer
            .render_scene_to_image(&scene(x, y, false), target)
            .unwrap();
        assert_same_pixels(&paths, &quads, frame);
    }
}

/// GPU time of frames of 8 layers of paths covering the target, each layer
/// drawn over a transparent quad that covers the one before so that it is
/// rasterized in a pass of its own, waiting for each frame to complete,
/// without readback.
#[test]
#[ignore = "manual GPU performance measurement"]
fn path_pass_frame_cost() {
    const LAYERS: usize = 8;
    let mut renderer = WgpuHeadlessRenderer::new().expect("timing requires a GPU");
    eprintln!(
        "path sample count {}",
        renderer.core.rendering_params.path_sample_count
    );
    for (width, height) in [(1440u32, 900u32), (2880, 1800)] {
        let mut scene = Scene::default();
        for layer in 0..LAYERS {
            if layer > 0 {
                scene.insert_primitive(rectangle_quad(
                    0.,
                    0.,
                    width as f32,
                    height as f32,
                    0x00000000,
                ));
            }
            for row in 0..height / 64 {
                for column in 0..width / 64 {
                    let rgba = if (row + column + layer as u32) % 2 == 0 {
                        RED
                    } else {
                        BLUE
                    };
                    scene.insert_primitive(rectangle_path(
                        column as f32 * 64. + 8.,
                        row as f32 * 64. + 8.,
                        48.,
                        48.,
                        rgba,
                    ));
                }
            }
        }
        scene.finish();
        assert_eq!(path_passes(&scene), LAYERS);
        let target = size(DevicePixels(width as i32), DevicePixels(height as i32));
        let mut samples = Vec::new();
        for frame in 0..120 {
            let start = std::time::Instant::now();
            renderer.render_scene(&scene, target).unwrap();
            renderer
                .core
                .resources
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .unwrap();
            if frame >= 20 {
                samples.push(start.elapsed());
            }
        }
        samples.sort();
        eprintln!(
            "{width}x{height}, {LAYERS} passes of {} paths: median {:?}, p90 {:?}",
            (width / 64) * (height / 64),
            samples[samples.len() / 2],
            samples[samples.len() * 9 / 10]
        );
    }
}

#[test]
fn path_intermediate_allocated_on_demand_and_released_after_idle_frames() {
    let Ok(mut renderer) = WgpuHeadlessRenderer::new() else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let target = size(DevicePixels(64), DevicePixels(64));
    let quad_only = scene(4., 4., false);
    let with_paths = scene(4., 4., true);
    let expect_msaa = renderer.core.rendering_params.path_sample_count > 1;

    // Pathless frames never allocate path_intermediate or path_msaa.
    for _ in 0..5 {
        renderer.render_scene(&quad_only, target).unwrap();
        assert_eq!(
            renderer.core.has_path_intermediate_textures(),
            (false, false),
            "pathless scenes must not allocate path_intermediate or path_msaa"
        );
    }

    // A scene containing paths allocates path_intermediate (+ path_msaa if MSAA > 1).
    renderer.render_scene(&with_paths, target).unwrap();
    assert_eq!(
        renderer.core.has_path_intermediate_textures(),
        (true, expect_msaa),
        "path scene must allocate path intermediate textures on demand"
    );

    // Textures remain alive for 179 consecutive pathless frames...
    let idle_limit = crate::wgpu_renderer::PATH_INTERMEDIATE_IDLE_RELEASE_FRAMES;
    for _ in 0..(idle_limit - 1) {
        renderer.render_scene(&quad_only, target).unwrap();
    }
    assert_eq!(
        renderer.core.has_path_intermediate_textures(),
        (true, expect_msaa),
        "path buffers should stay cached before hitting the {idle_limit}-frame idle threshold"
    );

    // ...and are released on the 180th consecutive pathless frame.
    renderer.render_scene(&quad_only, target).unwrap();
    assert_eq!(
        renderer.core.has_path_intermediate_textures(),
        (false, false),
        "path buffers must be released after {idle_limit} idle frames"
    );
}

#[test]
fn tile_cache_budget_matches_gpui_policy_and_fits_mobile_viewports() {
    assert_eq!(
        crate::fast::layers::tile_cache::DEFAULT_BUDGET_BYTES,
        gpui::DEFAULT_TILE_BUDGET_BYTES
    );
    // Xiaomi 15: 1200×2670 @ 3× -> 4×7 = 28 MiB <= 36 MiB (would fail old 24 MiB)
    let xiaomi_15 = size(px(1200.0 / 3.0), px(2670.0 / 3.0));
    assert!(gpui::viewport_fits_tile_budget(
        xiaomi_15,
        3.0,
        gpui::MOBILE_TILE_BUDGET_BYTES
    ));
    assert!(!gpui::viewport_fits_tile_budget(xiaomi_15, 3.0, 24 << 20));

    // QHD+: 1440×3200 @ 3.5× -> 4×8 = 32 MiB <= 36 MiB (would fail old 24 MiB)
    let qhd_plus = size(px(1440.0 / 3.5), px(3200.0 / 3.5));
    assert!(gpui::viewport_fits_tile_budget(
        qhd_plus,
        3.5,
        gpui::MOBILE_TILE_BUDGET_BYTES
    ));
    assert!(!gpui::viewport_fits_tile_budget(qhd_plus, 3.5, 24 << 20));
}

#[test]
fn gradient_srgb_and_oklab_color_spaces_render_accurately() {
    let Ok(mut renderer) = WgpuHeadlessRenderer::new() else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let target = size(DevicePixels(64), DevicePixels(16));
    let render_gradient = |renderer: &mut WgpuHeadlessRenderer, cs: gpui::ColorSpace| {
        let mut s = Scene::default();
        let bg = gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(color(RED), 0.),
            gpui::linear_color_stop(color(BLUE), 1.),
        )
        .color_space(cs);
        s.insert_primitive(Quad {
            bounds: bounds(0., 0., 64., 16.),
            content_mask: everywhere(),
            background: bg,
            ..Default::default()
        });
        s.finish();
        renderer.render_scene_to_image(&s, target).unwrap()
    };

    // sRGB midpoint between #ff0000 and #0000ff must be (~128, 0, ~128),
    // not the buggy double-gamma-roundtrip midpoint (~188, 0, ~188).
    let srgb_img = render_gradient(&mut renderer, gpui::ColorSpace::Srgb);
    let mid_srgb = srgb_img.get_pixel(32, 8).0;
    assert!(
        (mid_srgb[0] as i32 - 128).abs() <= 4
            && mid_srgb[1] <= 2
            && (mid_srgb[2] as i32 - 128).abs() <= 4,
        "expected sRGB midpoint near (128, 0, 128), got {mid_srgb:?}"
    );

    // Oklab midpoint of #ff0000 and #0000ff at pixel x=32 (t = 32.5 / 64 = 0.50781):
    //   sRGB -> linear -> Oklab -> mix(t) -> linear -> sRGB = (139, 83, 164).
    let oklab_img = render_gradient(&mut renderer, gpui::ColorSpace::Oklab);
    let mid_oklab = oklab_img.get_pixel(32, 8).0;
    assert!(
        (mid_oklab[0] as i32 - 139).abs() <= 4
            && (mid_oklab[1] as i32 - 83).abs() <= 4
            && (mid_oklab[2] as i32 - 164).abs() <= 4,
        "expected Oklab midpoint near (139, 83, 164), got {mid_oklab:?}"
    );
}

#[test]
fn backdrop_blur_blurs_background_and_respects_two_region_cap() {
    let Ok(mut renderer) = WgpuHeadlessRenderer::new() else {
        eprintln!("skipped: no wgpu adapter");
        return;
    };
    let target = size(DevicePixels(160), DevicePixels(120));

    // Sharp vertical split: left half pure red, right half pure blue.
    // Floating capsule straddling x = 80 with backdrop blur + subtle white tint.
    let mut blurred_scene = Scene::default();
    blurred_scene.insert_primitive(rectangle_quad(0., 0., 80., 120., RED));
    blurred_scene.insert_primitive(rectangle_quad(80., 0., 80., 120., BLUE));
    let glass = gpui::linear_gradient(
        180.,
        gpui::linear_color_stop(color(0xffffff60), 0.),
        gpui::linear_color_stop(color(0xffffff50), 1.),
    )
    .with_backdrop_blur(12.0);
    blurred_scene.insert_primitive(Quad {
        bounds: bounds(24., 36., 112., 48.),
        content_mask: everywhere(),
        background: glass,
        corner_radii: gpui::Corners::all(ScaledPixels(24.)),
        ..Default::default()
    });
    blurred_scene.finish();

    let img = renderer
        .render_scene_to_image(&blurred_scene, target)
        .unwrap();
    assert_eq!(renderer.core.fast_frame.backdrop_blur.allocated_slot_count(), 1);

    // Just left of the split (x = 76, inside the capsule) must pick up blue bleed
    // from the right half via the Dual-Kawase blur (more blue than the tint alone).
    let left_inside = img.get_pixel(76, 60).0;
    let far_left_inside = img.get_pixel(36, 60).0;
    assert!(
        left_inside[2] > far_left_inside[2] + 15,
        "expected blur bleed across x=80 split: near-split={left_inside:?}, far-left={far_left_inside:?}"
    );

    // Three backdrop-blur regions in one frame must cap scratch slots at MAX_REGIONS_PER_FRAME (2).
    let mut three_scene = Scene::default();
    three_scene.insert_primitive(rectangle_quad(0., 0., 160., 120., RED));
    for (i, y) in [8., 44., 80.].into_iter().enumerate() {
        three_scene.insert_primitive(Quad {
            bounds: bounds(16., y, 48. + i as f32 * 16., 24.),
            content_mask: everywhere(),
            background: glass,
            corner_radii: gpui::Corners::all(ScaledPixels(12.)),
            ..Default::default()
        });
    }
    three_scene.finish();
    renderer.render_scene(&three_scene, target).unwrap();
    assert_eq!(
        renderer.core.fast_frame.backdrop_blur.allocated_slot_count(),
        crate::fast::backdrop_blur::MAX_REGIONS_PER_FRAME
    );

    // Idle release after BACKDROP_BLUR_IDLE_RELEASE_FRAMES frames without backdrop blur.
    let plain = scene(4., 4., false);
    for _ in 0..crate::fast::backdrop_blur::BACKDROP_BLUR_IDLE_RELEASE_FRAMES {
        renderer.render_scene(&plain, target).unwrap();
    }
    assert_eq!(renderer.core.fast_frame.backdrop_blur.allocated_slot_count(), 0);
}
