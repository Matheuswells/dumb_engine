//! Shader viewer: a live preview of a surface shader (.wgsl) on a sphere, cube, plane or
//! cylinder, with its compile errors and preview parameters. Saving the shader (in the
//! built-in code editor or any other) recompiles it and the preview updates at once.

use crate::editor::Action;
use crate::viewport::PreviewViewport;
use dumb_asset::{AlphaMode, AssetDatabase, MaterialData};
use dumb_core::{builtin, AssetId, Color, Mat4, Quat, Vec3, Vec4};
use dumb_render::{push_model, InkSettings, PointLightData, RenderView, Renderer};

pub struct ShaderViewer {
    pub id: AssetId,
    pub open: bool,
    pub preview: PreviewViewport,
    mesh: AssetId,
    /// In-memory material that uses the shader (never saved).
    material_id: AssetId,
    material: MaterialData,
    show_sky: bool,
    show_point_light: bool,
    ink: bool,
    sun_angle: f32,
    exposure: f32,
    started: std::time::Instant,
}

const SIDE: f32 = 360.0;

impl ShaderViewer {
    pub fn new(id: AssetId, renderer: &mut Renderer) -> Self {
        let mut preview = PreviewViewport::new(renderer);
        preview.camera.distance = 2.2;
        preview.camera.pitch = -0.25;
        preview.auto_rotate = true;
        ShaderViewer {
            id,
            open: true,
            preview,
            mesh: builtin::SPHERE,
            material_id: AssetId::new(),
            material: MaterialData { shader: id, ..Default::default() },
            show_sky: true,
            show_point_light: false,
            ink: false,
            sun_angle: 35.0,
            exposure: 1.0,
            started: std::time::Instant::now(),
        }
    }

    pub fn render_view(&mut self, db: &mut AssetDatabase) -> Option<RenderView> {
        db.set_virtual_material(self.material_id, MaterialData { shader: self.id, ..self.material.clone() });
        let model = db.model(self.mesh)?;
        let mut v = RenderView::new(self.preview.target);
        v.view = self.preview.camera.view();
        v.proj = self.preview.camera.proj(self.preview.aspect());
        v.camera_pos = self.preview.camera.position();
        v.clear_color = Color::rgb(0.1, 0.105, 0.12);
        v.sky = self.show_sky;
        v.time = self.started.elapsed().as_secs_f32();
        v.lighting.exposure = self.exposure;
        v.lighting.sun_dir = Quat::from_rotation_y(self.sun_angle.to_radians()) * Vec3::new(-0.4, -1.0, -0.3).normalize();
        if self.show_point_light {
            v.lighting.points.push(PointLightData { position: Vec3::new(1.2, 0.8, 1.4), range: 6.0, color: Color::rgb(1.0, 0.85, 0.7), intensity: 6.0 });
        }
        if self.ink {
            v.ink = Some(InkSettings::default());
        }
        let scale = Mat4::from_scale(Vec3::splat(if self.mesh == builtin::PLANE { 0.15 } else { 1.0 }));
        push_model(&mut v, &model, self.mesh, self.material_id, scale, Color::WHITE, None, false, None);
        Some(v)
    }

    pub fn ui(&mut self, ctx: &egui::Context, db: &mut AssetDatabase, renderer: &mut Renderer, actions: &mut Vec<Action>) {
        let mut open = self.open;
        egui::Window::new(format!("✨ Shader — {}", db.display_name(self.id)))
            .id(egui::Id::new(("shader_viewer", self.id)))
            .open(&mut open)
            .default_size([960.0, 600.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Preview:");
                    for (id, n) in [(builtin::SPHERE, "Sphere"), (builtin::CUBE, "Cube"), (builtin::PLANE, "Plane"), (builtin::CYLINDER, "Cylinder")] {
                        ui.selectable_value(&mut self.mesh, id, n);
                    }
                    ui.separator();
                    if ui.button("📝 Edit code").clicked() {
                        if let Some(p) = db.abs_path(self.id) {
                            crate::script_tools::open_in_code_editor(&p, None, None);
                        }
                    }
                    if ui.button("🎨 Create material").on_hover_text("A material using this shader with these parameters").clicked() {
                        actions.push(Action::CreateShaderMaterial(self.id, MaterialData { shader: self.id, ..self.material.clone() }));
                    }
                });
                ui.separator();
                let avail = ui.available_size();
                ui.horizontal_top(|ui| {
                    let size = egui::vec2((avail.x - SIDE - 16.0).max(220.0), avail.y.max(220.0));
                    ui.vertical(|ui| {
                        self.preview.show(ui, renderer, size - egui::vec2(0.0, 26.0));
                        ui.horizontal(|ui| {
                            ui.toggle_value(&mut self.preview.auto_rotate, "⟲ Turntable");
                            ui.toggle_value(&mut self.show_sky, "☀ Sky");
                            ui.toggle_value(&mut self.show_point_light, "💡 Point light");
                            ui.toggle_value(&mut self.ink, "🖊 Ink outline");
                            ui.label("Sun");
                            ui.add(egui::DragValue::new(&mut self.sun_angle).speed(1.0).suffix("°"));
                            ui.label("Exposure");
                            ui.add(egui::DragValue::new(&mut self.exposure).speed(0.01).range(0.1..=4.0));
                        });
                    });
                    ui.allocate_ui_with_layout(egui::vec2(SIDE, avail.y.max(220.0)), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_width(SIDE);
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            self.status_ui(ui, db, renderer);
                            ui.separator();
                            ui.strong("Preview material");
                            ui.horizontal(|ui| {
                                ui.label("Color");
                                let mut a = self.material.albedo.to_array();
                                if ui.color_edit_button_rgba_unmultiplied(&mut a).changed() {
                                    self.material.albedo = Color::rgba(a[0], a[1], a[2], a[3]);
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("Metallic");
                                ui.add(egui::Slider::new(&mut self.material.metallic, 0.0..=1.0));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Roughness");
                                ui.add(egui::Slider::new(&mut self.material.roughness, 0.0..=1.0));
                            });
                            ui.horizontal(|ui| {
                                let mut blend = self.material.alpha_mode == AlphaMode::Blend;
                                if ui.checkbox(&mut blend, "Transparent").changed() {
                                    self.material.alpha_mode = if blend { AlphaMode::Blend } else { AlphaMode::Opaque };
                                }
                                ui.checkbox(&mut self.material.double_sided, "Double sided");
                            });
                            ui.separator();
                            shader_params_ui(ui, &mut self.material.shader_params0, &mut self.material.shader_params1);
                            ui.separator();
                            api_help(ui);
                        });
                    });
                });
            });
        self.open = open;
    }

    /// Compile state: OK, or the errors with links to their lines.
    fn status_ui(&self, ui: &mut egui::Ui, db: &AssetDatabase, renderer: &Renderer) {
        match renderer.shader_errors(self.id) {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("compiling…");
                });
            }
            Some([]) => {
                ui.colored_label(egui::Color32::from_rgb(110, 200, 120), "✔ Compiled");
            }
            Some(errors) => {
                ui.colored_label(egui::Color32::from_rgb(240, 110, 100), format!("✖ {} error(s) — the preview shows the last version that compiled", errors.len()));
                for e in errors {
                    let text = if e.line > 0 { format!("line {}: {}", e.line, e.message) } else { e.message.clone() };
                    if ui.link(egui::RichText::new(text).monospace().size(11.5)).on_hover_text("Open at this line").clicked() {
                        if let Some(p) = db.abs_path(self.id) {
                            crate::script_tools::open_in_code_editor(&p, (e.line > 0).then_some(e.line as u32), None);
                        }
                    }
                }
            }
        }
    }
}

/// The two free vec4 parameters a custom shader reads with `params0()` / `params1()`.
pub fn shader_params_ui(ui: &mut egui::Ui, p0: &mut Vec4, p1: &mut Vec4) -> bool {
    let mut changed = false;
    ui.strong("Shader params");
    for (name, v) in [("params0", p0), ("params1", p1)] {
        ui.horizontal(|ui| {
            ui.add_sized([56.0, 18.0], egui::Label::new(egui::RichText::new(name).monospace()));
            let mut a = v.to_array();
            for (axis, c) in ["x", "y", "z", "w"].into_iter().zip(a.iter_mut()) {
                changed |= ui.add(egui::DragValue::new(c).speed(0.01).prefix(format!("{axis} "))).changed();
            }
            let mut rgb = [a[0], a[1], a[2]];
            if ui.color_edit_button_rgb(&mut rgb).on_hover_text("Edit xyz as a color").changed() {
                a[..3].copy_from_slice(&rgb);
                changed = true;
            }
            *v = Vec4::from_array(a);
        });
    }
    changed
}

fn api_help(ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Shader API").default_open(false).show(ui, |ui| {
        ui.label(egui::RichText::new(API_HELP).monospace().size(11.0));
    });
}

const API_HELP: &str = "fn surface(s: Surface) -> vec4<f32>   (required)
fn vertex(v: Vertex) -> Vertex        (optional)

Surface: world_pos, normal, view_dir, uv,
         albedo, pixel, object_pos, time
Vertex:  world_pos, normal, uv, object_pos

lit(s, albedo, metallic, roughness)  standard light
tonemap(color)       linear HDR -> display
shadow(s)            0 shadowed .. 1 lit
sun_direction()  sun_color()  ambient(n)
sky(dir, blur)   albedo_texture(uv)
params0()  params1()  time()  camera_pos()";
