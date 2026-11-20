// Modified by Longbridge for gpui-fast.
//! GPU-written textures sampled directly by the ordinary sprite pipeline.
//!
//! The context owns the lookup table; the producer owns each entry through
//! `WgpuDynamicTexture`. No GPU objects enter GPUI's process-wide upload
//! backend, so browser handles stay on their browser thread too.

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use anyhow::Result;
use gpui::{
    Bounds, DevicePixels, DynamicTexture, DynamicTextureId, DynamicTexturePlatform, Pixels, Size,
    Window,
};
use parking_lot::Mutex;

use crate::{WgpuContext, wgpu_context::DeviceErrorState};

struct StoredTexture {
    view: wgpu::TextureView,
    texture: Option<wgpu::Texture>,
}

#[derive(Clone, Default)]
pub(crate) struct TextureStore(Arc<Mutex<HashMap<DynamicTextureId, StoredTexture>>>);

impl TextureStore {
    pub(crate) fn view(&self, id: DynamicTextureId) -> Option<wgpu::TextureView> {
        self.0.lock().get(&id).map(|entry| entry.view.clone())
    }

    fn texture(&self, id: DynamicTextureId) -> Option<wgpu::Texture> {
        self.0.lock().get(&id).and_then(|entry| entry.texture.clone())
    }
}

/// Mailstream (MS-450): CPU-uploaded dynamic texture backend for `gpui-fast-wgpu`.
/// Uploads tightly packed BGRA8 rows directly via `queue.write_texture`, swizzling
/// to RGBA8 only when the device's `color_texture_format` is `Rgba8Unorm`.
struct WgpuDynamicTexturePlatform {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    color_texture_format: wgpu::TextureFormat,
    errors: Arc<DeviceErrorState>,
    store: TextureStore,
}

impl WgpuDynamicTexturePlatform {
    fn new(context: &WgpuContext) -> Self {
        Self {
            device: Arc::clone(&context.device),
            queue: Arc::clone(&context.queue),
            color_texture_format: context.color_texture_format(),
            errors: Arc::clone(context.errors()),
            store: context.fast_dynamic_textures.clone(),
        }
    }
}

fn prepare_bgra_upload<'a>(bytes: &'a [u8], format: wgpu::TextureFormat) -> Cow<'a, [u8]> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => {
            let mut data = bytes.to_vec();
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Cow::Owned(data)
        }
        _ => Cow::Borrowed(bytes),
    }
}

impl DynamicTexturePlatform for WgpuDynamicTexturePlatform {
    fn create_texture(
        &self,
        id: DynamicTextureId,
        size: Size<DevicePixels>,
        bytes: &[u8],
    ) -> Result<()> {
        anyhow::ensure!(!self.errors.device_lost(), "dynamic texture device is lost");
        let limit = self.device.limits().max_texture_dimension_2d;
        anyhow::ensure!(
            size.width.0 > 0
                && size.height.0 > 0
                && size.width.0 as u32 <= limit
                && size.height.0 as u32 <= limit,
            "dynamic texture size must be positive and within the device's {limit} pixel limit"
        );
        let width = size.width.0 as u32;
        let height = size.height.0 as u32;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dynamic_texture_upload"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.color_texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let data = prepare_bgra_upload(bytes, self.color_texture_format);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        self.store.0.lock().insert(
            id,
            StoredTexture {
                view,
                texture: Some(texture),
            },
        );
        Ok(())
    }

    fn update_texture(
        &self,
        id: DynamicTextureId,
        bounds: Bounds<DevicePixels>,
        bytes: &[u8],
    ) -> Result<()> {
        anyhow::ensure!(!self.errors.device_lost(), "dynamic texture device is lost");
        let texture = self
            .store
            .texture(id)
            .ok_or_else(|| anyhow::anyhow!("dynamic texture was removed or is not CPU-writable"))?;
        let width = bounds.size.width.0 as u32;
        let height = bounds.size.height.0 as u32;
        let data = prepare_bgra_upload(bytes, self.color_texture_format);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: bounds.origin.x.0 as u32,
                    y: bounds.origin.y.0 as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    fn remove_texture(&self, id: DynamicTextureId) {
        self.store.0.lock().remove(&id);
    }
}

pub(crate) fn register_upload_platform(context: &WgpuContext) {
    gpui::set_dynamic_texture_platform(Arc::new(WgpuDynamicTexturePlatform::new(context)));
}

/// An owned render target that GPUI samples without an upload or transfer copy.
///
/// Allocate it from the context drawing the window. Dropping it frees its
/// entry and retires its sprite id, so later sprite lookups skip it.
/// Notify the view when removing content, as for other view state changes.
/// A different context cannot resolve its id, even on the same adapter.
///
/// Check the creating context's `device_lost()` when handling device loss,
/// even if contents change rarely. Drop its targets promptly, before replacing
/// the context: each owner retains a context clone and its device. Recreate
/// targets from the replacement context and render their contents again.
/// `render` refuses a lost device; `paint` can still succeed for a stale owner,
/// but the replacement context skips its sprite and displays no texture.
pub struct WgpuDynamicTexture {
    context: WgpuContext,
    texture: DynamicTexture,
}

impl WgpuContext {
    /// Allocates a GPU-written dynamic texture on this context's device.
    ///
    /// The view uses `color_texture_format()` and `RENDER_ATTACHMENT |
    /// TEXTURE_BINDING`. There is no initial pixel upload. Render new contents
    /// with `WgpuDynamicTexture::render`, then paint with its `paint` method.
    pub fn create_dynamic_texture(&self, size: Size<DevicePixels>) -> Result<WgpuDynamicTexture> {
        anyhow::ensure!(!self.device_lost(), "dynamic texture device is lost");
        let limit = self.device.limits().max_texture_dimension_2d;
        anyhow::ensure!(
            size.width.0 > 0
                && size.height.0 > 0
                && size.width.0 as u32 <= limit
                && size.height.0 as u32 <= limit,
            "dynamic texture size must be positive and within the device's {limit} pixel limit"
        );
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dynamic_texture"),
            size: wgpu::Extent3d {
                width: size.width.0 as u32,
                height: size.height.0 as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.color_texture_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let texture = DynamicTexture::new_gpu(size)?;
        self.fast_dynamic_textures.0.lock().insert(
            texture.id(),
            StoredTexture {
                view: target.create_view(&Default::default()),
                texture: None,
            },
        );
        Ok(WgpuDynamicTexture {
            context: self.clone(),
            texture,
        })
    }
}

impl WgpuDynamicTexture {
    /// Writes new contents and refreshes the window only after success.
    ///
    /// Submit the producer's commands on the creating context's queue before
    /// returning. GPUI's later submission samples the same texture; queue
    /// ordering makes a CPU wait or transfer copy unnecessary. Do not submit
    /// partial writes and then return an error. Call only when contents change.
    /// Outside paint, success requests a redraw. During paint it updates the
    /// current frame without scheduling another frame.
    /// Only this window is refreshed; notify other windows displaying the
    /// target so they redraw its new contents too.
    pub fn render<T>(
        &self,
        window: &mut Window,
        render: impl FnOnce(&wgpu::TextureView) -> Result<T>,
    ) -> Result<T> {
        anyhow::ensure!(
            !self.context.device_lost(),
            "dynamic texture device is lost"
        );
        let view = self
            .context
            .fast_dynamic_textures
            .view(self.texture.id())
            .ok_or_else(|| anyhow::anyhow!("dynamic texture was removed"))?;
        let result = render(&view)?;
        window.refresh_dynamic_texture(self.texture)?;
        Ok(result)
    }

    /// Paints with the window's current clipping, opacity and painter order.
    /// Call during paint in a window using the creating context.
    pub fn paint(&self, window: &mut Window, bounds: Bounds<Pixels>) -> Result<()> {
        window.paint_dynamic_texture(bounds, self.texture)
    }
}

impl Drop for WgpuDynamicTexture {
    fn drop(&mut self) {
        self.context
            .fast_dynamic_textures
            .0
            .lock()
            .remove(&self.texture.id());
        self.texture.remove();
    }
}

#[cfg(all(test, feature = "test-support", not(target_family = "wasm")))]
mod tests;
