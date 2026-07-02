//! Throwaway visual harness for theme widgets (chips, rows, buttons): renders
//! the choice-row variants in one window so an automated capture can verify
//! layout symmetry without driving the full app into the Studio.
//!
//!   cargo run -p yt-clipper --example ui_gallery

#[path = "../src/theme.rs"]
mod theme;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 640.0])
            .with_title("ui-gallery"),
        ..Default::default()
    };
    eframe::run_native(
        "ui-gallery",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx, std::path::Path::new("assets/fonts/Anton-Regular.ttf"));
            Ok(Box::new(Gallery { sel: 1, preset: Some(0) }))
        }),
    )
}

struct Gallery {
    sel: usize,
    preset: Option<usize>,
}

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Replicate the Studio's EXACT properties-panel structure (right side
        // panel + scroll area + enabled_ui nesting) so panel-specific layout
        // bugs reproduce here, not just bare rows in a central panel.
        egui::Panel::right("gallery-props")
            .resizable(true)
            .default_size(300.0)
            .size_range(250.0..=420.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("props").show(ui, |ui| {
                    theme::section(ui, "Framing");
                    theme::card().show(ui, |ui| {
                        ui.add_enabled_ui(true, |ui| {
                            if let Some(i) = theme::chip_row(
                                ui,
                                &[
                                    (self.sel == 0, "Stacked"),
                                    (self.sel == 1, "Full cam"),
                                    (self.sel == 2, "Wide"),
                                ],
                            ) {
                                self.sel = i;
                            }
                            let _ =
                                theme::wide_button(ui, egui::Button::new("Reset to auto framing"));
                        });
                    });

                    theme::section(ui, "Caption presets");
                    theme::card().show(ui, |ui| {
                        if let Some(i) = theme::chip_row(
                            ui,
                            &[
                                (self.preset == Some(0), "Classic"),
                                (self.preset == Some(1), "TikTok"),
                                (self.preset == Some(2), "Podcast"),
                            ],
                        ) {
                            self.preset = Some(i);
                        }
                        if let Some(i) = theme::chip_row(
                            ui,
                            &[
                                (self.preset == Some(3), "Minimal"),
                                (self.preset == Some(4), "Gaming"),
                                (self.preset == Some(5), "MrBeast"),
                            ],
                        ) {
                            self.preset = Some(i + 3);
                        }
                        ui.weak("Pick a starting look — everything below stays editable.");
                    });

                    theme::section(ui, "Customize captions");
                    theme::card().show(ui, |ui| {
                        ui.label("Animation");
                        let _ = theme::chip_row(
                            ui,
                            &[(true, "Huge word"), (false, "Rolling"), (false, "Karaoke")],
                        );
                        ui.horizontal(|ui| {
                            ui.label("Size");
                            let mut size = 150u32;
                            ui.add(egui::Slider::new(&mut size, 40..=220).suffix(" px"));
                            let mut bold = false;
                            ui.checkbox(&mut bold, "Bold");
                        });
                    });
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::WELL))
            .show_inside(ui, |_ui| {});
    }
}
