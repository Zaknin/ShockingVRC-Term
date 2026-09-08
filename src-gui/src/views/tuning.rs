use egui::Ui;

use crate::app::Ctx;
use crate::bridge::Command;
use crate::model::{NormField, UkfField};
use crate::theme::widgets::{self as w, space};

pub fn show(ui: &mut Ui, cx: &mut Ctx) {
    let gap = space::SHEET_GAP;
    let col_w = (ui.available_width() - gap) * 0.5;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        w::column(ui, col_w, |ui| ukf(ui, cx));
        w::column(ui, col_w, |ui| norms(ui, cx));
    });
    ui.add_space(space::SHEET_GAP);
    live_readout(ui, cx);
}

fn ukf(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    let p = cx.snap.config.ukf;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Unscented Kalman filter");
        w::dim(
            ui,
            pal,
            "Smooths the contact stream before it drives the output. \
             Lower Q is calmer but slower to react.",
        );
        ui.add_space(space::ITEM_GAP);

        for f in UkfField::ALL {
            let (min, max) = f.range();
            let changed = w::slider_row(
                ui,
                pal,
                f.label(),
                128.0,
                w::Slider::new(f.get(&p), min, max),
                &f.format(&p),
                68.0,
                None,
            );
            if let Some(v) = changed {
                let mut next = p;
                f.set(&mut next, v);
                cx.send(Command::SetUkf(next));
            }
        }

        ui.add_space(space::ROW_GAP);
        if w::button(ui, pal, "Reset defaults").clicked() {
            cx.send(Command::ResetUkf);
        }
    });
}

fn norms(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    let n = cx.snap.config.norms;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Motion normalisation");
        w::dim(
            ui,
            pal,
            "Divisors that turn raw motion into a 0–100% level. \
             Raise one to make that mode less sensitive.",
        );
        ui.add_space(space::ITEM_GAP);

        for f in NormField::ALL {
            let (min, max) = f.range();
            let changed = w::slider_row(
                ui,
                pal,
                f.label(),
                128.0,
                w::Slider::new(f.get(&n), min, max),
                &format!("{:.3}", f.get(&n)),
                68.0,
                None,
            );
            if let Some(v) = changed {
                let mut next = n;
                f.set(&mut next, v);
                cx.send(Command::SetNorms(next));
            }
        }

        ui.add_space(space::ROW_GAP);
        if w::button(ui, pal, "Reset defaults").clicked() {
            cx.send(Command::ResetNorms);
        }
    });
}

fn live_readout(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Live kinematics");
        w::dim(
            ui,
            pal,
            "Normalised values feeding the channels. If one pins at 100%, its divisor is too low.",
        );
        ui.add_space(space::ITEM_GAP);

        let gap = space::SHEET_GAP;
        let col_w = (ui.available_width() - gap) * 0.5;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for ch in crate::model::Channel::ALL {
                w::column(ui, col_w, |ui| {
                    let k = &cx.snap.channel_status(ch).kinematics;
                    w::caption(ui, pal, &format!("Channel {}", ch.label()));
                    ui.add_space(2.0);
                    for (label, v) in [
                        ("Depth", k.depth),
                        ("Speed", k.speed),
                        ("Accel", k.acc),
                        ("Recoil", k.recoil),
                    ] {
                        let pinned = v >= 0.999;
                        w::gauge_row(
                            ui,
                            pal,
                            label,
                            60.0,
                            v,
                            if pinned { pal.warning } else { pal.accent },
                            &format!("{:.0}%", v * 100.0),
                            52.0,
                        );
                    }
                });
            }
        });
    });
}
