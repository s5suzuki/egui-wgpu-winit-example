use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Instant,
};

use egui::{Vec2, ViewportId, ViewportInfo};
use egui_wgpu::ScreenDescriptor;
use egui_winit::winit::{
    self,
    application::ApplicationHandler,
    event::DeviceEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::Window,
};

use crate::{
    egui_renderer::EguiRenderer,
    event::{EventResult, UserEvent},
    AppState,
};

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    surface: wgpu::Surface<'static>,
    state: AppState,
    egui_renderer: EguiRenderer,
}

impl Renderer {
    async fn new(
        instance: &wgpu::Instance,
        egui_ctx: egui::Context,
        window: Arc<Window>,
        width: u32,
        height: u32,
        state: AppState,
    ) -> anyhow::Result<Self> {
        let surface = instance.create_surface(window.clone())?;

        let power_pref = wgpu::PowerPreference::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_pref,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("Failed to find an appropriate adapter");

        let features = wgpu::Features::empty();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: features,
                    required_limits: Default::default(),
                    memory_hints: Default::default(),
                },
                None,
            )
            .await
            .expect("Failed to create device");

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        let selected_format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let swapchain_format = swapchain_capabilities
            .formats
            .iter()
            .find(|d| **d == selected_format)
            .expect("failed to select proper surface texture format!");

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: *swapchain_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 0,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        let egui_renderer = EguiRenderer::new(&device, egui_ctx, window, &surface_config)?;

        Ok(Self {
            device,
            queue,
            surface,
            surface_config,
            state,
            egui_renderer,
        })
    }

    fn run_ui_and_paint(&mut self, window: &Window) -> anyhow::Result<EventResult> {
        let Self {
            device,
            queue,
            surface_config,
            surface,
            state,
            egui_renderer,
        } = self;

        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [surface_config.width, surface_config.height],
            pixels_per_point: window.scale_factor() as f32,
        };

        let surface_texture = surface.get_current_texture()?;

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        let result = egui_renderer.run_ui_and_paint(
            device,
            queue,
            &mut encoder,
            &surface_view,
            screen_descriptor,
            window,
            state,
        )?;

        queue.submit(Some(encoder.finish()));
        surface_texture.present();

        Ok(result)
    }

    fn on_window_event(
        &mut self,
        event: &winit::event::WindowEvent,
        window: &Window,
    ) -> EventResult {
        let Self {
            device,
            surface_config,
            surface,
            egui_renderer,
            ..
        } = self;
        let mut repaint_asap = false;

        match event {
            winit::event::WindowEvent::Resized(physical_size) => {
                if let (Some(width), Some(height)) = (
                    NonZeroU32::new(physical_size.width),
                    NonZeroU32::new(physical_size.height),
                ) {
                    repaint_asap = true;
                    surface_config.width = width.get();
                    surface_config.height = height.get();
                    surface.configure(device, surface_config);
                }
            }

            winit::event::WindowEvent::CloseRequested => {
                if egui_renderer.close {
                    return EventResult::Exit;
                }

                egui_renderer.info.events.push(egui::ViewportEvent::Close);

                egui_renderer
                    .egui_winit
                    .egui_ctx()
                    .request_repaint_of(ViewportId::ROOT);
            }
            _ => {}
        };

        let event_response = egui_renderer.on_window_event(window, event);

        if egui_renderer.close {
            EventResult::Exit
        } else if event_response.repaint {
            if repaint_asap {
                EventResult::RepaintNow
            } else {
                EventResult::RepaintNext
            }
        } else {
            EventResult::Wait
        }
    }

    fn on_device_event(&mut self, event: DeviceEvent) -> EventResult {
        if let winit::event::DeviceEvent::MouseMotion { delta } = event {
            self.egui_renderer.egui_winit.on_mouse_motion(delta);
            return EventResult::RepaintNext;
        }
        EventResult::Wait
    }

    fn on_user_event(&self, event: UserEvent) -> EventResult {
        match event {
            UserEvent::RequestRepaint {
                when,
                cumulative_pass_nr,
            } => {
                let current_pass_nr = self
                    .egui_renderer
                    .egui_winit
                    .egui_ctx()
                    .cumulative_pass_nr_for(ViewportId::ROOT);
                if current_pass_nr == cumulative_pass_nr
                    || current_pass_nr == cumulative_pass_nr + 1
                {
                    EventResult::RepaintAt(when)
                } else {
                    EventResult::Wait
                }
            }
        }
    }
}

pub struct App {
    windows_next_repaint_time: Option<Instant>,
    repaint_proxy: Arc<Mutex<EventLoopProxy<UserEvent>>>,
    instance: wgpu::Instance,
    renderer: Option<Renderer>,
    window: Option<Arc<Window>>,
    window_size: Vec2,
    app_state: Option<AppState>,
    pub return_result: anyhow::Result<()>,
}

impl App {
    pub fn new(
        event_loop: &EventLoop<UserEvent>,
        window_size: impl Into<Vec2>,
        app_state: AppState,
    ) -> Self {
        let instance = egui_wgpu::wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        Self {
            windows_next_repaint_time: None,
            repaint_proxy: Arc::new(Mutex::new(event_loop.create_proxy())),
            instance,
            renderer: None,
            window: None,
            window_size: window_size.into(),
            app_state: Some(app_state),
            return_result: Ok(()),
        }
    }

    fn create_window(
        &self,
        egui_ctx: &egui::Context,
        event_loop: &ActiveEventLoop,
    ) -> Result<Window, winit::error::OsError> {
        let viewport_builder = egui::ViewportBuilder::default()
            .with_inner_size(self.window_size)
            .with_visible(false);
        let window = egui_winit::create_window(egui_ctx, event_loop, &viewport_builder)?;
        Ok(window)
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let egui_ctx = EguiRenderer::create_egui_context();
        let window = self.create_window(&egui_ctx, event_loop)?;
        self.init_run_state(egui_ctx, window)?;
        Ok(())
    }

    fn init_run_state(&mut self, egui_ctx: egui::Context, window: Window) -> anyhow::Result<()> {
        let window = Arc::new(window);

        {
            let event_loop_proxy = self.repaint_proxy.clone();
            egui_ctx.set_request_repaint_callback(move |info| {
                let when = Instant::now() + info.delay;
                let cumulative_pass_nr = info.current_cumulative_pass_nr;
                event_loop_proxy
                    .lock()
                    .unwrap()
                    .send_event(UserEvent::RequestRepaint {
                        when,
                        cumulative_pass_nr,
                    })
                    .ok();
            });
        }

        let mut info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut info, &egui_ctx, &window, true);

        let state = pollster::block_on(Renderer::new(
            &self.instance,
            egui_ctx,
            window.clone(),
            self.window_size.x as u32,
            self.window_size.y as u32,
            self.app_state.take().unwrap(),
        ))?;
        self.renderer = Some(state);
        self.window = Some(window);

        Ok(())
    }

    fn run_ui_and_paint(&mut self, window: &Window) -> anyhow::Result<EventResult> {
        if let Some(renderer) = &mut self.renderer {
            renderer.run_ui_and_paint(window)
        } else {
            Ok(EventResult::Wait)
        }
    }

    fn handle_event_result(
        &mut self,
        event_loop: &ActiveEventLoop,
        event_result: anyhow::Result<EventResult>,
    ) {
        let mut exit = false;

        let combined_result = event_result.and_then(|event_result| match event_result {
            EventResult::Wait => {
                event_loop.set_control_flow(ControlFlow::Wait);
                Ok(event_result)
            }
            EventResult::RepaintNow => {
                if cfg!(target_os = "windows") {
                    if let Some(ref window) = self.window.as_ref().cloned() {
                        self.run_ui_and_paint(window)
                    } else {
                        event_loop.set_control_flow(ControlFlow::Wait);
                        Ok(event_result)
                    }
                } else {
                    self.windows_next_repaint_time = Some(Instant::now());
                    Ok(event_result)
                }
            }
            EventResult::RepaintNext => {
                self.windows_next_repaint_time = Some(Instant::now());
                Ok(event_result)
            }
            EventResult::RepaintAt(repaint_time) => {
                self.windows_next_repaint_time = Some(
                    self.windows_next_repaint_time
                        .map_or(repaint_time, |last| last.min(repaint_time)),
                );
                Ok(event_result)
            }
            EventResult::Exit => {
                exit = true;
                Ok(event_result)
            }
        });

        if let Err(err) = combined_result {
            exit = true;
            self.return_result = Err(err);
        };

        if exit {
            event_loop.exit();
        }

        self.check_redraw_requests(event_loop);
    }

    fn check_redraw_requests(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some(next_repaint_time) = self.windows_next_repaint_time {
            if now >= next_repaint_time {
                self.windows_next_repaint_time = None;
                if let Some(ref window) = self.window {
                    window.request_redraw();
                }
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(next_repaint_time));
            }
        }
    }

    fn on_window_event(
        &mut self,
        event: winit::event::WindowEvent,
        window: &Window,
    ) -> anyhow::Result<EventResult> {
        if let Some(renderer) = &mut self.renderer {
            Ok(renderer.on_window_event(&event, window))
        } else {
            Ok(EventResult::Wait)
        }
    }

    fn on_device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) -> anyhow::Result<EventResult> {
        if let Some(renderer) = &mut self.renderer {
            Ok(renderer.on_device_event(event))
        } else {
            Ok(EventResult::Wait)
        }
    }

    fn on_resumed(&mut self, event_loop: &ActiveEventLoop) -> Result<EventResult, anyhow::Error> {
        if self.window.is_none() {
            self.initialize(event_loop)?;
        }
        Ok(EventResult::RepaintNow)
    }

    fn on_user_event(&mut self, event: UserEvent) -> Result<EventResult, anyhow::Error> {
        if let Some(renderer) = &mut self.renderer {
            return Ok(renderer.on_user_event(event));
        }
        Ok(EventResult::Wait)
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.handle_event_result(event_loop, Ok(EventResult::Wait));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let event_result = self.on_resumed(event_loop);
        self.handle_event_result(event_loop, event_result);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        let event_result = self.on_device_event(event_loop, device_id, event);
        self.handle_event_result(event_loop, event_result);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let event_result = self.on_user_event(event);
        self.handle_event_result(event_loop, event_result);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _: winit::event::StartCause) {
        self.check_redraw_requests(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &egui_winit::winit::event_loop::ActiveEventLoop,
        _: egui_winit::winit::window::WindowId,
        event: egui_winit::winit::event::WindowEvent,
    ) {
        let event_result = {
            if let Some(window) = self.window.as_ref().cloned() {
                match event {
                    winit::event::WindowEvent::RedrawRequested => self.run_ui_and_paint(&window),
                    _ => self.on_window_event(event, &window),
                }
            } else {
                Ok(EventResult::Wait)
            }
        };
        self.handle_event_result(event_loop, event_result);
    }
}
