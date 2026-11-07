pub mod alarm;
pub mod channels;
pub mod log;
pub mod modulation;
pub mod presets;
pub mod setup;
pub mod status;
pub mod tuning;
pub mod zones;

pub fn staged_preview_notice(ui: &mut egui::Ui, cx: &crate::app::Ctx) {
    if !cx.snap.staged_preview_only {
        return;
    }
    let pal = cx.pal;
    crate::theme::widgets::sheet(ui, pal, |ui| {
        crate::theme::widgets::heading(ui, pal, "DEVICE OFFLINE · PREVIEW ONLY");
        crate::theme::widgets::dim(
            ui,
            pal,
            "NOT APPLIED TO HARDWARE. Staged edits stay in memory and will not auto-apply when a Coyote connects.",
        );
    });
    ui.add_space(crate::theme::widgets::space::ROW_GAP);
}
