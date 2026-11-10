//! The pixels behind [`Driver`](crate::Driver) screenshots. By default there are none: a
//! screenshot is a flat grey image of the screen's size, which keeps GPU-less tests fast.
//! [`Renderer::wgpu`] renders real frames on a wgpu device with no window or surface, through
//! the same off-screen path a covered window uses, so `ui.screenshot`, annotate and trace give
//! real pixels in tests and CI.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How a [`Driver`](crate::Driver) renders screenshots. Cheap to clone (the device is shared).
#[derive(Clone, Default)]
pub struct Renderer {
    #[cfg(not(target_arch = "wasm32"))]
    state: Option<eframe::egui_wgpu::RenderState>,
    /// The last off-screen render worked (false until the first one).
    ok: Arc<AtomicBool>,
    /// Why [`Renderer::wgpu`] gave no GPU renderer.
    reason: Option<String>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer").field("name", &self.name()).field("adapter", &self.adapter()).field("reason", &self.reason).finish()
    }
}

impl Renderer {
    /// No GPU: screenshots are flat grey (the default).
    pub fn none() -> Self {
        Self::default()
    }

    /// Render on a wgpu device with no window. Without an adapter (or on the web) this is
    /// [`Renderer::none`], with [`Renderer::reason`] saying why; it never panics.
    pub fn wgpu() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let made = std::panic::catch_unwind(gpu::create).unwrap_or_else(|_| Err("creating the wgpu device panicked".into()));
            match made {
                Ok(state) => Renderer { state: Some(state), ..Self::default() },
                Err(e) => {
                    log::info!("headless renderer: no GPU ({e}); screenshots are grey");
                    Renderer { reason: Some(e), ..Self::default() }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        Renderer { reason: Some("no headless GPU renderer on the web".into()), ..Self::default() }
    }

    /// A GPU renderer is set up.
    pub fn is_gpu(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.state.is_some();
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// `"wgpu"` or `"none"`.
    pub fn name(&self) -> &'static str {
        if self.is_gpu() { "wgpu" } else { "none" }
    }

    /// Why [`Renderer::wgpu`] gave no GPU renderer.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The adapter in use, e.g. `"Apple M2 (Metal)"`.
    pub fn adapter(&self) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        return self.state.as_ref().map(|s| {
            let i = s.adapter.get_info();
            format!("{} ({:?})", i.name, i.backend)
        });
        #[cfg(target_arch = "wasm32")]
        None
    }

    /// What the last screenshot was rendered with: `"wgpu"`, or `"none"` when there is no GPU
    /// renderer or its last render failed (the grey image was used).
    pub(crate) fn last_used(&self) -> &'static str {
        if self.is_gpu() && self.ok.load(Ordering::Relaxed) { "wgpu" } else { "none" }
    }

    /// The debugger's off-screen renderer on this device (`None`: no GPU).
    pub(crate) fn offscreen(&self) -> Option<ashlar_ui::devtools::Offscreen> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(state) = &self.state {
            let inner = crate::offscreen::renderer(state.clone());
            let ok = Arc::clone(&self.ok);
            return Some(Arc::new(move |f: &ashlar_ui::devtools::OffscreenFrame<'_>| {
                let r = inner(f);
                ok.store(r.is_ok(), Ordering::Relaxed);
                r
            }));
        }
        None
    }

    /// Hand a frame's texture changes to the device, as a window's backend does after each frame.
    pub(crate) fn textures(&self, delta: &egui::TexturesDelta) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(state) = &self.state {
            let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::offscreen::apply_textures(state, delta)));
            if applied.is_err() {
                log::warn!("headless renderer: uploading textures panicked");
                self.ok.store(false, Ordering::Relaxed);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = delta;
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod gpu {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    use eframe::egui_wgpu::{RenderState, RendererOptions, WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew, wgpu};

    /// A device with no surface. Software adapters first (the same pixels on every machine that
    /// has one), then a discrete GPU, then any other.
    pub(super) fn create() -> Result<RenderState, String> {
        let mut setup = WgpuSetupCreateNew::without_display_handle();
        setup.instance_descriptor.backends.remove(wgpu::Backends::BROWSER_WEBGPU);
        setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
            let mut adapters: Vec<&wgpu::Adapter> = adapters.iter().filter(|a| a.get_info().backend != wgpu::Backend::Noop).collect();
            adapters.sort_by_key(|a| match a.get_info().device_type {
                wgpu::DeviceType::Cpu => 0,
                wgpu::DeviceType::DiscreteGpu => 1,
                _ => 2,
            });
            adapters.first().map(|a| (*a).clone()).ok_or_else(|| "no wgpu adapter found".to_owned())
        }));
        let setup = WgpuSetup::CreateNew(setup);
        let instance = block_on(setup.new_instance());
        let config = WgpuConfiguration { wgpu_setup: setup, ..Default::default() };
        block_on(RenderState::create(&config, &instance, None, RendererOptions::PREDICTABLE)).map_err(|e| e.to_string())
    }

    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    /// Run a future to completion on this thread (wgpu's setup futures resolve without an
    /// executor on native).
    fn block_on<F: Future>(f: F) -> F::Output {
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park();
        }
    }
}
