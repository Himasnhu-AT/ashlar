//! Screenshots without a presented frame: the debugger's [`Offscreen`] renderer on eframe's own
//! wgpu device and egui renderer. A window that is covered, in the background or on a locked
//! screen presents nothing (the system gives it no drawable), so a capture of the presented frame
//! never arrives; rendering the frame's shapes into a texture of our own always works.

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

use ashlar_ui::devtools::{Offscreen, OffscreenFrame};
use eframe::egui_wgpu::{RenderState, ScreenDescriptor, wgpu};

const WAIT: Duration = Duration::from_secs(5);

/// The renderer for eframe's wgpu state.
pub(crate) fn renderer(rs: RenderState) -> Offscreen {
    Arc::new(move |f: &OffscreenFrame<'_>| render(&rs, f))
}

fn render(rs: &RenderState, f: &OffscreenFrame<'_>) -> Result<egui::ColorImage, String> {
    let [w, h] = f.size_in_pixels;
    let max = rs.device.limits().max_texture_dimension_2d;
    if w == 0 || h == 0 || w > max || h > max {
        return Err(format!("cannot render {w}×{h} off screen (limit {max})"));
    }
    let rgba = match rs.target_format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => [0, 1, 2, 3],
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => [2, 1, 0, 3],
        other => return Err(format!("cannot read back a {other:?} frame")),
    };
    let screen = ScreenDescriptor { size_in_pixels: [w, h], pixels_per_point: f.pixels_per_point };
    let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ashlar.devtools.offscreen"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: rs.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ashlar.devtools.offscreen") });
    let user_buffers = {
        let mut renderer = rs.renderer.write();
        // this frame's new textures (glyphs, images) are not uploaded yet: the backend uploads
        // them after this; uploading twice is harmless, freeing is left to the backend
        for (id, deltas) in &f.textures.set {
            for delta in deltas {
                renderer.update_texture(&rs.device, &rs.queue, *id, delta);
            }
        }
        let user_buffers = renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, f.primitives, &screen);
        let c = egui::Rgba::from(f.clear);
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ashlar.devtools.offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: c.r().into(), g: c.g().into(), b: c.b().into(), a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, f.primitives, &screen);
        user_buffers
    };

    let unpadded = (w as usize).saturating_mul(4);
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = unpadded.div_ceil(align).saturating_mul(align);
    let buffer = rs.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ashlar.devtools.offscreen.readback"),
        size: (padded as u64).saturating_mul(u64::from(h)),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded as u32), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    let index = rs.queue.submit(user_buffers.into_iter().chain(std::iter::once(encoder.finish())));
    let slice = buffer.slice(..);
    let (tx, rx) = channel();
    slice.map_async(wgpu::MapMode::Read, move |r| drop(tx.send(r)));
    rs.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(WAIT) }).map_err(|e| format!("GPU wait: {e}"))?;
    rx.recv_timeout(WAIT).map_err(|_| "the GPU did not hand the frame back".to_string())?.map_err(|e| format!("reading the frame back: {e}"))?;
    let pixels = {
        let data = slice.get_mapped_range().map_err(|e| format!("reading the frame back: {e}"))?;
        let mut pixels = Vec::with_capacity((w as usize).saturating_mul(h as usize));
        for row in data.chunks(padded) {
            for p in row.get(..unpadded).unwrap_or(&[]).chunks_exact(4) {
                let at = |i: usize| p.get(rgba.get(i).copied().unwrap_or(0)).copied().unwrap_or(0);
                pixels.push(egui::Color32::from_rgba_premultiplied(at(0), at(1), at(2), 255));
            }
        }
        pixels
    };
    buffer.unmap();
    if pixels.len() != (w as usize).saturating_mul(h as usize) {
        return Err("the frame read back has the wrong size".into());
    }
    Ok(egui::ColorImage::new([w as usize, h as usize], pixels))
}
