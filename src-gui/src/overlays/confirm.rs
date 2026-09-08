use egui::Context;

use crate::app::Ctx;
use crate::bridge::Command;
use crate::theme::widgets::{self as w, font, space};

pub fn show(ctx: &Context, cx: &mut Ctx) {
    preset_delete(ctx, cx);
    zone_set_delete(ctx, cx);
}

fn preset_delete(ctx: &Context, cx: &mut Ctx) {
    let Some(index) = cx.ui_state.preset_delete_confirm else {
        return;
    };
    let entry = cx
        .bridge
        .shared
        .presets
        .read()
        .ok()
        .and_then(|s| s.entries.get(index).cloned());
    let Some(entry) = entry.filter(|e| e.user) else {
        cx.ui_state.preset_delete_confirm = None;
        return;
    };

    let pal = cx.pal;
    let dismissed = super::modal(ctx, pal, "confirm_preset_delete", "Delete preset", 360.0, |ui| {
        ui.label(
            egui::RichText::new(format!("“{}”", entry.name))
                .font(font::mono_big())
                .color(pal.contrast),
        );
        ui.add_space(4.0);
        w::dim(ui, pal, "This removes the file from presets/user/. It cannot be undone.");
        ui.add_space(space::ITEM_GAP);
        ui.horizontal(|ui| {
            if w::danger_button(ui, pal, "Delete", true).clicked() {
                cx.bridge.send(Command::DeleteUserPreset(index));
                cx.ui_state.preset_delete_confirm = None;
            }
            if w::button(ui, pal, "Cancel").clicked() {
                cx.ui_state.preset_delete_confirm = None;
            }
        });
    });
    if dismissed {
        cx.ui_state.preset_delete_confirm = None;
    }
}

fn zone_set_delete(ctx: &Context, cx: &mut Ctx) {
    let Some(index) = cx.ui_state.zone_preset_delete_confirm else {
        return;
    };
    let entry = cx
        .bridge
        .shared
        .zone_presets
        .read()
        .ok()
        .and_then(|s| s.entries.get(index).cloned());
    let Some(entry) = entry else {
        cx.ui_state.zone_preset_delete_confirm = None;
        return;
    };

    let pal = cx.pal;
    let dismissed = super::modal(ctx, pal, "confirm_zoneset_delete", "Delete zone set", 360.0, |ui| {
        ui.label(
            egui::RichText::new(format!("“{}”", entry.name()))
                .font(font::mono_big())
                .color(pal.contrast),
        );
        ui.add_space(4.0);
        let (a, b) = entry.preset.counts();
        w::dim(
            ui,
            pal,
            format!("A: {a} zone(s), B: {b} zone(s). Removes the file from presets/zones/."),
        );
        ui.add_space(space::ITEM_GAP);
        ui.horizontal(|ui| {
            if w::danger_button(ui, pal, "Delete", true).clicked() {
                cx.bridge.send(Command::DeleteZonePreset(index));
                cx.ui_state.zone_preset_delete_confirm = None;
            }
            if w::button(ui, pal, "Cancel").clicked() {
                cx.ui_state.zone_preset_delete_confirm = None;
            }
        });
    });
    if dismissed {
        cx.ui_state.zone_preset_delete_confirm = None;
    }
}
