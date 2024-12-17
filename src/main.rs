use app::App;
use egui_winit::winit;

mod app;
mod egui_renderer;
mod event;

pub struct AppState {
    name: String,
    age: i32,
}

impl AppState {
    pub fn update(&mut self, ctx: &egui::Context) {
        egui::Window::new("My Window")
            .resizable(true)
            .vscroll(true)
            .default_open(false)
            .show(ctx, |ui| {
                ui.heading("My egui Application");
                ui.horizontal(|ui| {
                    let name_label = ui.label("Your name: ");
                    ui.text_edit_singleline(&mut self.name)
                        .labelled_by(name_label.id);
                });
                ui.add(egui::Slider::new(&mut self.age, 0..=120).text("age"));
                if ui.button("Increment").clicked() {
                    self.age += 1;
                }
                ui.label(format!("Hello '{}', age {}", self.name, self.age));
            });
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = winit::event_loop::EventLoop::with_user_event().build()?;
    let mut app = App::new(
        &event_loop,
        [320., 240.],
        AppState {
            name: "John Doe".to_owned(),
            age: 42,
        },
    );
    event_loop.run_app(&mut app)?;
    app.return_result
}
