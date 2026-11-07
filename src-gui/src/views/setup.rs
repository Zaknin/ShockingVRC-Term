use egui::Ui;

use crate::app::Ctx;
use crate::bridge::Command;
use crate::theme::widgets::{self as w, font, space};

pub fn show(ui: &mut Ui, cx: &mut Ctx) -> bool {
    let mut start_tutorial = false;

    let gap = space::SHEET_GAP;
    let col_w = (ui.available_width() - gap) * 0.5;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        w::column(ui, col_w, |ui| connection(ui, cx));
        w::column(ui, col_w, |ui| osc(ui, cx));
    });

    ui.add_space(space::SHEET_GAP);
    preferences(ui, cx);

    ui.add_space(space::SHEET_GAP);
    remote_access(ui, cx);

    ui.add_space(space::SHEET_GAP);
    if help(ui, cx) {
        start_tutorial = true;
    }

    start_tutorial
}

fn remote_access(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    let mut settings = cx.ui_state.remote_settings.clone();
    let mut changed = false;
    let status = cx.remote.status();
    let mut invite_friend = false;
    let mut cancel_invitation = false;
    let mut revoke_terminals = Vec::new();
    let mut save_registration_key = false;
    let mut disconnect = false;

    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Remote Access");
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Listener", 120.0, false);
            changed |= ui
                .checkbox(&mut settings.remote_enabled, "Enable Remote Access")
                .changed();
            let listener_color = match status.listener {
                shocking_vrc_remote::ListenerState::Listening => pal.success,
                shocking_vrc_remote::ListenerState::Error => pal.error,
                _ => pal.warning,
            };
            ui.label(egui::RichText::new(status.listener.as_str()).font(font::small()).color(listener_color));
        });
        ui.add_space(space::ITEM_GAP);
        egui::CollapsingHeader::new("Advanced connectivity")
            .default_open(false)
            .show(ui, |ui| {
        if let Some(address) = status.address {
            ui.horizontal(|ui| {
                w::field_label(ui, pal, "Listen address", 120.0, false);
                w::body(ui, pal, address.to_string());
            });
            ui.horizontal(|ui| {
                w::field_label(ui, pal, "Local URL", 120.0, false);
                let local_url = format!("https://localhost:{}", address.port());
                w::body(ui, pal, &local_url);
                if w::button(ui, pal, "Copy URL").clicked() {
                    ui.ctx().copy_text(local_url);
                }
            });
        }

        ui.add_space(space::ITEM_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "TCP port", 120.0, false);
            let response = ui.add(egui::DragValue::new(&mut settings.listen_port).range(1..=u16::MAX));
            changed |= response.changed() && (response.drag_stopped() || response.lost_focus());
            w::dim(ui, pal, "Default: 4318");
        });

        ui.add_space(space::ROW_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Traversal", 120.0, false);
            changed |= ui.checkbox(&mut settings.traversal_enabled, "Enable ICE traversal").changed();
            let color = match status.traversal {
                shocking_vrc_remote::TraversalState::Registered | shocking_vrc_remote::TraversalState::Active => pal.success,
                shocking_vrc_remote::TraversalState::Error => pal.error,
                _ => pal.warning,
            };
            ui.label(egui::RichText::new(status.traversal.as_str()).font(font::small()).color(color));
        });
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Rendezvous URL", 120.0, false);
            changed |= ui.add(egui::TextEdit::singleline(&mut settings.rendezvous_url)
                .hint_text("wss://your-rendezvous.example/signal")
                .desired_width(420.0)).changed();
        });
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Public Host", 120.0, false);
            changed |= ui.add(egui::TextEdit::singleline(&mut settings.direct_host)
                .hint_text("host.example.com or public IPv4; blank for ICE-only")
                .desired_width(420.0)).changed();
        });
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "STUN/TURN URIs", 120.0, false);
            let response = ui.add(egui::TextEdit::multiline(&mut cx.ui_state.remote_ice_servers_edit)
                .hint_text("stun:host:3478 — leave empty to use host candidates only")
                .desired_rows(2).desired_width(420.0));
            if response.changed() {
                settings.ice_server_urls = cx.ui_state.remote_ice_servers_edit.lines()
                    .map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect();
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Registration key", 120.0, false);
            ui.add(egui::TextEdit::singleline(&mut cx.ui_state.remote_registration_key)
                .password(true).hint_text("Set or change rendezvous key").desired_width(250.0));
            save_registration_key = w::button_enabled(ui, pal, "Save key", !cx.ui_state.remote_registration_key.is_empty()).clicked();
            w::dim(ui, pal, if status.registration_key_configured { "Stored with Windows DPAPI." } else { "Not configured." });
        });
        w::dim(ui, pal, "Rendezvous and STUN/TURN are optional. No public endpoint is used by default.");

        ui.add_space(space::ROW_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Host ID", 120.0, false);
            w::body(ui, pal, status.server_id.as_deref().unwrap_or("Loading identity…"));
        });
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "TLS fingerprint", 120.0, false);
            if let Some(fingerprint) = status.fingerprint.as_deref() {
                w::body(ui, pal, fingerprint);
                if w::button(ui, pal, "Copy").clicked() {
                    ui.ctx().copy_text(fingerprint.to_owned());
                }
            } else {
                w::body(ui, pal, "Loading identity…");
            }
        });
            });

        ui.add_space(space::ITEM_GAP);
        w::heading(ui, pal, "PAIRED TERMINALS");
        ui.horizontal(|ui| {
            invite_friend = w::button_enabled(ui, pal, "Invite friend", settings.remote_enabled).clicked();
            let invitation = cx.ui_state.remote_invitation.lock().ok().and_then(|value| value.clone());
            if let Some(invitation) = invitation {
                if w::button(ui, pal, "Copy invitation").clicked() { ui.ctx().copy_text(invitation); }
                cancel_invitation = w::button(ui, pal, "Cancel invitation").clicked();
            }
        });
        if let Some(invitation) = cx.ui_state.remote_invitation.lock().ok().and_then(|value| value.clone()) {
            let mut invitation_text = invitation;
            ui.add(egui::TextEdit::multiline(&mut invitation_text).desired_rows(2)
                .desired_width(ui.available_width()).interactive(false));
        }
        if status.paired_terminals.is_empty() {
            w::dim(ui, pal, "No Terminals are paired.");
        } else {
            for terminal in &status.paired_terminals {
                ui.horizontal(|ui| {
                    w::body(ui, pal, format!("{} · paired {}", terminal.device_id, terminal.paired_at));
                    if w::button(ui, pal, "Revoke terminal").clicked() { revoke_terminals.push(terminal.device_id.clone()); }
                });
            }
        }

        ui.add_space(space::ITEM_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "API session", 120.0, false);
            w::body(ui, pal, status.session.as_str());
            if let Some(seconds) = status.last_contact_seconds_ago {
                w::dim(ui, pal, format!("Last API contact {seconds}s ago"));
            }
            disconnect = w::button_enabled(
                ui,
                pal,
                "Disconnect client",
                status.session != shocking_vrc_remote::auth::SessionPhase::Free,
            ).clicked();
        });
        w::dim(ui, pal, status.session.status_description());

        ui.add_space(space::ROW_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Remote limits", 120.0, false);
            changed |= ui
                .checkbox(&mut settings.remote_strength.enabled, "Enable Remote Maximum Strength")
                .changed();
        });
        w::dim(
            ui,
            pal,
            "Applies only to remote Power Limits min/max writes. Local controls and displayed Host values remain unrestricted.",
        );

        for (label, cap) in [
            ("Channel A", &mut settings.remote_strength.max_by_channel.a),
            ("Channel B", &mut settings.remote_strength.max_by_channel.b),
        ] {
            ui.add_space(space::ROW_GAP);
            ui.horizontal(|ui| {
                w::field_label(ui, pal, label, 120.0, false);
                match cap {
                    Some(value) => {
                        let response = ui.add(egui::DragValue::new(value).range(0..=200));
                        changed |= response.changed() && (response.drag_stopped() || response.lost_focus());
                    }
                    None => {
                        w::dim(ui, pal, "No cap configured");
                        if w::button(ui, pal, "Set cap").clicked() {
                            *cap = Some(0);
                            changed = true;
                        }
                    }
                }
            });
        }

        if settings.remote_strength.enabled
            && (settings.remote_strength.max_by_channel.a.is_none()
                || settings.remote_strength.max_by_channel.b.is_none())
        {
            ui.add_space(space::ROW_GAP);
            w::dim(ui, pal, "Set both channel caps before enabling this policy.");
        }

        if let Some(error) = status.error.as_ref() {
            ui.add_space(space::ROW_GAP);
            ui.label(egui::RichText::new(error).font(font::small()).color(pal.warning));
        }
    });

    if changed {
        cx.ui_state.remote_settings = settings.clone();
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.update_settings(settings).await {
                controller.report_ui_error(error);
            }
        });
    }
    if invite_friend {
        let controller = std::sync::Arc::clone(cx.remote);
        let invitation = std::sync::Arc::clone(&cx.ui_state.remote_invitation);
        tokio::spawn(async move {
            match controller.create_pairing_invitation().await {
                Ok(uri) => { if let Ok(mut current) = invitation.lock() { *current = Some(uri); } }
                Err(error) => controller.report_ui_error(error),
            }
        });
    }
    if cancel_invitation {
        if let Ok(mut invitation) = cx.ui_state.remote_invitation.lock() { *invitation = None; }
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.cancel_pairing_invitation().await { controller.report_ui_error(error); }
        });
    }
    for device_id in revoke_terminals {
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.revoke_terminal(device_id).await { controller.report_ui_error(error); }
        });
    }
    if save_registration_key {
        let key = std::mem::take(&mut cx.ui_state.remote_registration_key);
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.set_traversal_registration_key(key).await {
                controller.report_ui_error(error);
            }
        });
    }
    if disconnect {
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            controller.disconnect_client().await;
        });
    }
}

fn connection(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Connection");

        let vrc = cx.snap.vrchat_found;
        status_line(
            ui,
            cx,
            vrc,
            if vrc {
                "VRChat detected".to_string()
            } else {
                "VRChat not found — enable OSC in game".to_string()
            },
        );

        let count = cx.snap.devices.iter().filter(|d| d.connected).count();
        status_line(
            ui,
            cx,
            count > 0,
            match count {
                0 => "Coyote searching…".to_string(),
                1 => "Coyote connected".to_string(),
                n => format!("{n} Coyotes connected"),
            },
        );

        status_line(
            ui,
            cx,
            !cx.snap.avatar_zones.is_empty(),
            format!("{} avatar zone(s) discovered", cx.snap.avatar_zones.len()),
        );

        ui.add_space(space::ITEM_GAP);
        w::dim(
            ui,
            pal,
            "Devices are found and reconnected automatically — there is nothing to pair.",
        );
    });
}

fn status_line(ui: &mut Ui, cx: &Ctx, ok: bool, text: String) {
    let pal = cx.pal;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(r, w::SQUARE, if ok { pal.success } else { pal.warning });
        ui.label(
            egui::RichText::new(text)
                .font(font::body())
                .color(pal.contrast),
        );
    });
}

fn osc(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "OSC & OSCQuery");

        let port_line = if cx.snap.osc_port == 0 {
            "selecting…".to_string()
        } else if cx.snap.oscquery_port == Some(cx.snap.osc_port) {
            format!("{}", cx.snap.osc_port)
        } else {
            format!("{} (fixed, legacy mode)", cx.snap.osc_port)
        };

        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Port", 96.0, false);
            w::body(ui, pal, port_line);
        });
        if let Some(http) = cx.snap.oscquery_port
            && Some(cx.snap.osc_port) != Some(http) {
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "OSCQuery", 96.0, false);
                    w::body(ui, pal, http.to_string());
                });
            }

        ui.add_space(space::ITEM_GAP);
        w::dim(
            ui,
            pal,
            "A free port is chosen at startup, used for both OSC (UDP) and OSCQuery (HTTP), \
             then advertised to VRChat over mDNS. Just enable OSC in VRChat — it will find \
             this app on its own.",
        );

        ui.add_space(space::ITEM_GAP);
        w::caption(ui, pal, "Custom DGB contacts");
        ui.add_space(2.0);
        for line in [
            "1.  Add a VRC Contact Receiver to your avatar",
            "2.  Set Receiver Type to Proximity",
            "3.  Set Parameter to DGB/YourContactName",
            "4.  Upload the avatar",
        ] {
            ui.label(
                egui::RichText::new(line)
                    .font(font::small())
                    .color(pal.dim),
            );
        }
    });
}

fn preferences(ui: &mut Ui, cx: &mut Ctx) {
    let pal = cx.pal;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Preferences");

        toggle_row(
            ui,
            cx,
            "Auto-save",
            "Write this device's config to devices/ on every change",
            |s| s.auto_save,
            |s, v| s.auto_save = v,
            Command::SetAutoSave,
        );

        ui.add_space(space::ROW_GAP);

        toggle_row(
            ui,
            cx,
            "Alarm section",
            "Adds the wake-up alarm to the navigation. Turning it off disarms the alarm.",
            |s| s.alarm_section_visible,
            |s, v| s.alarm_section_visible = v,
            Command::SetAlarmTabVisible,
        );

        ui.add_space(space::ROW_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Nickname", 120.0, true);
            let resp = w::text_field(
                ui,
                pal,
                &mut cx.ui_state.nickname,
                "(no author)",
                220.0,
            );
            if resp.lost_focus() {
                let nick = cx.ui_state.nickname.clone();
                if let Err(e) = shocking_vrc_app::prefs::update_ui_prefs(|p| p.nickname = nick) {
                    log::warn!("[prefs] Failed to save nickname: {e}");
                }
            }
            ui.add_space(6.0);
            w::dim(ui, pal, "Credited as the author on presets you save");
        });
    });
}

fn toggle_row(
    ui: &mut Ui,
    cx: &mut Ctx,
    label: &str,
    hint: &str,
    get: impl Fn(&crate::app::UiState) -> bool,
    set: impl Fn(&mut crate::app::UiState, bool),
    cmd: impl Fn(bool) -> Command,
) {
    let pal = cx.pal;
    ui.horizontal(|ui| {
        w::field_label(ui, pal, label, 120.0, true);
        let on = get(cx.ui_state);
        if let Some(next) = w::toggle(ui, pal, on)
            && next != on {
                set(cx.ui_state, next);
                cx.send(cmd(next));
            }
        ui.add_space(6.0);
        w::dim(ui, pal, hint);
    });
}

fn help(ui: &mut Ui, cx: &mut Ctx) -> bool {
    let pal = cx.pal;
    let mut start = false;
    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Help");
        ui.horizontal(|ui| {
            if w::cta(ui, pal, "Open the guided tour").clicked() {
                start = true;
            }
            if w::button(ui, pal, "Check for updates").clicked() {
                cx.send(Command::CheckUpdate);
                cx.bridge.shared.notify(
                    crate::bridge::ToastKind::Info,
                    "Checking for updates…",
                );
            }
            if w::button(ui, pal, "About").clicked() {
                cx.ui_state.show_about = true;
            }
        });

        ui.add_space(space::ITEM_GAP);
        w::caption(ui, pal, "Keyboard");
        ui.add_space(2.0);
        for (keys, what) in [
            ("1 – 9", "jump to a section"),
            ("T", "open the theme panel"),
            ("Esc", "close the topmost dialog"),
        ] {
            ui.horizontal(|ui| {
                w::field_label(ui, pal, keys, 80.0, false);
                w::dim(ui, pal, what);
            });
        }

        ui.add_space(space::ITEM_GAP);
        w::caption(ui, pal, "Safety");
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Start with a low power ceiling — 20 to 30 — and raise it slowly. \
                 Presets and zone sets never change your limits.",
            )
            .font(font::small())
            .color(pal.warning),
        );
    });
    start
}
