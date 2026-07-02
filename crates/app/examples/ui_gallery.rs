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
        egui::CentralPanel::default().show_inside(ui, |ui| {
            theme::section(ui, "chip_row (current)");
            theme::card().show(ui, |ui| {
                if let Some(i) =
                    theme::chip_row(ui, &[(self.sel == 0, "Stacked"), (self.sel == 1, "Full cam"), (self.sel == 2, "Wide")])
                {
                    self.sel = i;
                }
                if let Some(i) = theme::chip_row(
                    ui,
                    &[(self.preset == Some(0), "Classic"), (self.preset == Some(1), "TikTok"), (self.preset == Some(2), "Podcast")],
                ) {
                    self.preset = Some(i);
                }
                if let Some(i) = theme::chip_row(
                    ui,
                    &[(self.preset == Some(3), "Minimal"), (self.preset == Some(4), "Gaming"), (self.preset == Some(5), "MrBeast")],
                ) {
                    self.preset = Some(i + 3);
                }
            });

            theme::section(ui, "min_size variant");
            theme::card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let w = ((ui.available_width() - gap * 2.0) / 3.0).floor();
                    for (i, label) in ["Classic", "TikTok", "Podcast"].iter().enumerate() {
                        let _ = ui.add(
                            theme::chip(self.preset == Some(i), label)
                                .min_size(egui::vec2(w, theme::chip_h(ui))),
                        );
                    }
                });
            });

            theme::section(ui, "full-width rows");
            theme::card().show(ui, |ui| {
                for (i, label) in ["Manual", "Active Speaker  ·  best for podcasts"].iter().enumerate() {
                    let _ = ui.add_sized(
                        [ui.available_width(), theme::chip_h(ui)],
                        theme::chip(self.sel == i, label),
                    );
                }
            });
        });
    }
}
