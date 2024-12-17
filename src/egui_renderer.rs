use std::{sync::Arc, time::Instant};

use egui::{
    ahash::HashSet, epaint::textures, ClippedPrimitive, FullOutput, ViewportId, ViewportIdMap,
    ViewportInfo, ViewportOutput,
};
use egui_wgpu::{Renderer, ScreenDescriptor};
use egui_winit::{winit::window::Window, ActionRequested, EventResponse};
use wgpu::{CommandEncoder, Device, Queue, StoreOp, SurfaceConfiguration, TextureView};

use crate::{event::EventResult, AppState};

pub struct EguiRenderer {
    pub beginning: Instant,
    pub egui_winit: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    pub info: ViewportInfo,
    deferred_commands: Vec<egui::viewport::ViewportCommand>,
    actions_requested: HashSet<ActionRequested>,
    pending_full_output: egui::FullOutput,
    pub close: bool,
    is_first_frame: bool,
}

impl EguiRenderer {
    pub fn new(
        device: &Device,
        egui_ctx: egui::Context,
        window: Arc<Window>,
        surface_config: &SurfaceConfiguration,
    ) -> anyhow::Result<Self> {
        let egui_winit = egui_winit::State::new(
            egui_ctx,
            egui::viewport::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(2 * 1024),
        );
        let renderer = Renderer::new(device, surface_config.format, None, 1, true);

        let mut info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut info, egui_winit.egui_ctx(), &window, true);

        Ok(Self {
            beginning: Instant::now(),
            egui_winit,
            renderer,
            info,
            deferred_commands: Default::default(),
            pending_full_output: Default::default(),
            actions_requested: Default::default(),
            close: false,
            is_first_frame: true,
        })
    }

    pub fn create_egui_context() -> egui::Context {
        let egui_ctx = egui::Context::default();
        egui_ctx.set_embed_viewports(false);
        egui_ctx.options_mut(|o| {
            o.max_passes = 2.try_into().unwrap();
        });
        egui_ctx
    }

    fn update(&mut self, mut raw_input: egui::RawInput, app: &mut AppState) -> FullOutput {
        raw_input.time = Some(self.beginning.elapsed().as_secs_f64());

        let close_requested = raw_input.viewport().close_requested();

        let full_output = self.egui_winit.egui_ctx().run(raw_input, |egui_ctx| {
            app.update(egui_ctx);
        });

        if close_requested {
            let canceled = full_output.viewport_output[&ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose);
            if !canceled {
                self.close = true;
            }
        }

        self.pending_full_output.append(full_output);
        std::mem::take(&mut self.pending_full_output)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run_ui_and_paint(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        window_surface_view: &TextureView,
        screen_descriptor: ScreenDescriptor,
        window: &Window,
        app: &mut AppState,
    ) -> anyhow::Result<EventResult> {
        let raw_input = {
            egui_winit::update_viewport_info(
                &mut self.info,
                self.egui_winit.egui_ctx(),
                window,
                false,
            );

            let mut raw_input = self.egui_winit.take_egui_input(window);

            raw_input.time = Some(self.beginning.elapsed().as_secs_f64());
            raw_input
                .viewports
                .insert(ViewportId::ROOT, self.info.clone());
            raw_input
        };

        let full_output = self.update(raw_input, app);

        let FullOutput {
            platform_output,
            shapes,
            pixels_per_point,
            viewport_output,
            textures_delta,
        } = full_output;

        self.info.events.clear();

        self.egui_winit
            .handle_platform_output(window, platform_output);

        let clipped_primitives = self
            .egui_winit
            .egui_ctx()
            .tessellate(shapes, pixels_per_point);

        self.paint_and_update_textures(
            device,
            queue,
            encoder,
            window_surface_view,
            screen_descriptor,
            clipped_primitives,
            textures_delta,
        );

        for action in self.actions_requested.drain() {
            match action {
                ActionRequested::Cut => {
                    self.egui_winit
                        .egui_input_mut()
                        .events
                        .push(egui::Event::Cut);
                }
                ActionRequested::Copy => {
                    self.egui_winit
                        .egui_input_mut()
                        .events
                        .push(egui::Event::Copy);
                }
                ActionRequested::Paste => {
                    if let Some(contents) = self.egui_winit.clipboard_text() {
                        let contents = contents.replace("\r\n", "\n");
                        if !contents.is_empty() {
                            self.egui_winit
                                .egui_input_mut()
                                .events
                                .push(egui::Event::Paste(contents));
                        }
                    }
                }
                _ => {}
            }
        }

        if std::mem::take(&mut self.is_first_frame) {
            window.set_visible(true);
        }

        self.handle_viewport_output(&viewport_output, window);

        if window.is_minimized() == Some(true) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        if self.close {
            Ok(EventResult::Exit)
        } else {
            Ok(EventResult::Wait)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint_and_update_textures(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        window_surface_view: &TextureView,
        screen_descriptor: ScreenDescriptor,
        clipped_primitives: Vec<ClippedPrimitive>,
        textures_delta: textures::TexturesDelta,
    ) {
        self.egui_winit
            .egui_ctx()
            .set_pixels_per_point(screen_descriptor.pixels_per_point);

        for (id, image_delta) in &textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        self.renderer.update_buffers(
            device,
            queue,
            encoder,
            &clipped_primitives,
            &screen_descriptor,
        );
        let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: window_surface_view,
                resolve_target: None,
                ops: egui_wgpu::wgpu::Operations {
                    load: egui_wgpu::wgpu::LoadOp::Load,
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            label: Some("egui main render pass"),
            occlusion_query_set: None,
        });

        self.renderer.render(
            &mut rpass.forget_lifetime(),
            &clipped_primitives,
            &screen_descriptor,
        );
        for x in &textures_delta.free {
            self.renderer.free_texture(x)
        }
    }

    fn handle_viewport_output(
        &mut self,
        viewport_output: &ViewportIdMap<ViewportOutput>,
        window: &Window,
    ) {
        for (
            _,
            ViewportOutput {
                parent: _,
                class: _,
                builder: _,
                viewport_ui_cb: _,
                mut commands,
                repaint_delay: _,
            },
        ) in viewport_output.clone()
        {
            self.deferred_commands.append(&mut commands);
            egui_winit::process_viewport_commands(
                self.egui_winit.egui_ctx(),
                &mut self.info,
                std::mem::take(&mut self.deferred_commands),
                window,
                &mut self.actions_requested,
            );
        }
    }

    pub(crate) fn on_window_event(
        &mut self,
        window: &Window,
        event: &egui_winit::winit::event::WindowEvent,
    ) -> EventResponse {
        self.egui_winit.on_window_event(window, event)
    }
}
