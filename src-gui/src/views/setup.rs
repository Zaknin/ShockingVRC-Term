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
    use egui::collapsing_header::CollapsingState;

    let pal = cx.pal;
    let mut settings = cx.ui_state.remote_settings.clone();
    let mut changed = false;
    let was_remote_enabled = settings.remote_enabled;
    let status = cx.remote.status();
    let mut newly_enabled = false;
    let mut refresh_interfaces = false;
    let mut generate_invite = false;
    let mut copy_invites = Vec::new();
    let mut revoke_invites = Vec::new();
    let mut revoke_terminals = Vec::new();
    let mut save_registration_key = false;
    let mut disconnect = false;

    w::sheet(ui, pal, |ui| {
        w::heading(ui, pal, "Remote Access");
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Listener", 220.0, false);
            let response = ui.checkbox(&mut settings.remote_enabled, "Enable Remote Access");
            if response.changed() {
                changed = true;
                newly_enabled = !was_remote_enabled && settings.remote_enabled;
            }
            let listener_color = match status.listener {
                shocking_vrc_remote::ListenerState::Listening => pal.success,
                shocking_vrc_remote::ListenerState::Error => pal.error,
                _ => pal.warning,
            };
            ui.label(egui::RichText::new(status.listener.as_str()).font(font::small()).color(listener_color));
        });
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Host display name", 220.0, false);
            changed |= ui.add(
                egui::TextEdit::singleline(&mut settings.host_display_name)
                    .hint_text("My Coyote")
                    .desired_width(300.0),
            ).changed();
            w::dim(ui, pal, "Shown to paired Terminals after an authenticated snapshot.");
        });

        let direct_ready = shocking_vrc_remote::settings::is_valid_direct_host(&settings.direct_host);
        let traversal_ready = settings.traversal_enabled
            && !settings.rendezvous_url.trim().is_empty()
            && status.registration_key_configured
            && matches!(
                status.traversal,
                shocking_vrc_remote::TraversalState::Registered | shocking_vrc_remote::TraversalState::Active
            );
        let invitation_ready = direct_ready || traversal_ready;
        let connectivity_incomplete = !invitation_ready;
        let advanced_id = ui.make_persistent_id("remote-advanced-connectivity");
        let mut advanced = CollapsingState::load_with_default_open(
            ui.ctx(),
            advanced_id,
            settings.remote_enabled && connectivity_incomplete,
        );
        if newly_enabled && connectivity_incomplete {
            advanced.set_open(true);
        }
        advanced
            .show_header(ui, |ui| {
                ui.label(egui::RichText::new("Advanced connectivity").font(font::body()).color(pal.contrast));
            })
            .body(|ui| {
                if let Some(address) = status.address {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Listen address", 220.0, false);
                        w::body(ui, pal, address.to_string());
                    });
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Local URL", 220.0, false);
                        w::body(ui, pal, format!("https://localhost:{}", address.port()));
                    });
                }

                ui.add_space(space::ITEM_GAP);
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "TCP port", 220.0, false);
                    let response = ui.add(egui::DragValue::new(&mut settings.listen_port).range(1..=u16::MAX));
                    changed |= response.changed() && (response.drag_stopped() || response.lost_focus());
                    w::dim(ui, pal, "Default: 4318");
                });

                ui.add_space(space::ROW_GAP);
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Traversal", 220.0, false);
                    changed |= ui.checkbox(&mut settings.traversal_enabled, "Enable ICE traversal").changed();
                    let color = match status.traversal {
                        shocking_vrc_remote::TraversalState::Registered | shocking_vrc_remote::TraversalState::Active => pal.success,
                        shocking_vrc_remote::TraversalState::Error => pal.error,
                        _ => pal.warning,
                    };
                    ui.label(egui::RichText::new(status.traversal.as_str()).font(font::small()).color(color));
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Rendezvous URL", 220.0, false);
                    changed |= ui.add(
                        egui::TextEdit::singleline(&mut settings.rendezvous_url)
                            .hint_text("wss://rendezvous.example.com/api/signal/v1")
                            .desired_width(420.0),
                    ).changed();
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Direct address (IP/domain)", 220.0, false)
                        .on_hover_text("A local IPv4 address works on the same PC/LAN. It is not automatically reachable from the Internet.");
                    changed |= ui.add(
                        egui::TextEdit::singleline(&mut settings.direct_host)
                            .hint_text("LAN IPv4 or public IP/domain; blank for ICE-only")
                            .desired_width(360.0),
                    ).changed();
                    refresh_interfaces = w::button(ui, pal, "Refresh interfaces").clicked();
                });
                if !cx.ui_state.local_ipv4_interfaces.is_empty() {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Available LAN IPv4", 220.0, false);
                        let selected = cx.ui_state.local_ipv4_interfaces.iter()
                            .find(|interface| interface.address.to_string() == settings.direct_host)
                            .map(|interface| format!("{} · {}", interface.name, interface.address))
                            .unwrap_or_else(|| "Choose an interface…".to_owned());
                        egui::ComboBox::from_id_salt("remote-direct-ipv4")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for interface in &cx.ui_state.local_ipv4_interfaces {
                                    let review = if interface.requires_review { " · verify VPN/virtual adapter" } else { "" };
                                    let label = format!(
                                        "{} · {} · {}{}",
                                        interface.name, interface.kind, interface.address, review
                                    );
                                    if ui.selectable_value(
                                        &mut settings.direct_host,
                                        interface.address.to_string(),
                                        label,
                                    ).changed() {
                                        changed = true;
                                    }
                                }
                            });
                        if cx.ui_state.local_ipv4_interfaces.iter().any(|item| item.requires_review) {
                            w::dim(ui, pal, "VPN/virtual adapters are marked for manual review; nothing is selected automatically.");
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Available LAN IPv4", 220.0, false);
                        w::dim(ui, pal, "No suitable active interface found. You can enter a LAN IPv4 manually.");
                    });
                }
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "STUN/TURN URIs", 220.0, false);
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut cx.ui_state.remote_ice_servers_edit)
                            .hint_text("stun:host:3478 — leave empty to use host candidates only")
                            .desired_rows(2)
                            .desired_width(420.0),
                    );
                    if response.changed() {
                        settings.ice_server_urls = cx.ui_state.remote_ice_servers_edit.lines()
                            .map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect();
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Reg key", 220.0, false);
                    ui.add(
                        egui::TextEdit::singleline(&mut cx.ui_state.remote_registration_key)
                            .password(true)
                            .hint_text("Set or change rendezvous key")
                            .desired_width(250.0),
                    );
                    save_registration_key = w::button_enabled(
                        ui, pal, "Save key", !cx.ui_state.remote_registration_key.is_empty()
                    ).clicked();
                    w::dim(ui, pal, if status.registration_key_configured { "Stored with Windows DPAPI." } else { "Not configured." });
                });
                w::dim(ui, pal, "Rendezvous and STUN/TURN are optional. No public endpoint is contacted by default.");
                if settings.traversal_enabled && !traversal_ready {
                    ui.add_space(space::ROW_GAP);
                    w::dim(ui, pal, "Rendezvous is not ready for invitations until this Host is registered with a valid key and policy entry.");
                }
                if status.listener == shocking_vrc_remote::ListenerState::Listening && connectivity_incomplete {
                    ui.add_space(space::ROW_GAP);
                    w::dim(ui, pal, "The local HTTPS listener is running, but no invitation route is configured yet.");
                }
                egui::CollapsingHeader::new("Host identity details").default_open(false).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Host ID", 220.0, false);
                        w::body(ui, pal, status.server_id.as_deref().unwrap_or("Loading identity…"));
                    });
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "TLS fingerprint", 220.0, false);
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
            });

        ui.add_space(space::ITEM_GAP);
        w::heading(ui, pal, "INVITATIONS");
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Description", 220.0, false);
            ui.add(
                egui::TextEdit::singleline(&mut cx.ui_state.invitation_description)
                    .hint_text("Optional, e.g. Laptop")
                    .desired_width(250.0),
            );
            generate_invite = w::button_enabled(
                ui, pal, "Generate invite", settings.remote_enabled && invitation_ready
            ).clicked();
        });
        if status.active_invitations.is_empty() {
            w::dim(ui, pal, "No active invitations.");
        } else {
            for invitation in &status.active_invitations {
                ui.horizontal_wrapped(|ui| {
                    let title = invitation.description.as_deref().unwrap_or("Invitation");
                    w::body(ui, pal, title);
                    let remaining = invitation.expires_at.saturating_sub(
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                            .map(|value| value.as_secs()).unwrap_or_default()
                    );
                    w::dim(ui, pal, format!("ACTIVE · {}:{:02} remaining", remaining / 60, remaining % 60));
                    w::dim(ui, pal, format!("Created {}", host_time(invitation.created_at)));
                    if w::button(ui, pal, "Copy link").clicked() {
                        copy_invites.push(invitation.id);
                    }
                    if w::danger_button(ui, pal, "Revoke invite", true).clicked() {
                        revoke_invites.push(invitation.id);
                    }
                });
            }
        }

        let historical_count = status.invitation_history.iter()
            .filter(|record| record.status != shocking_vrc_remote::auth::InvitationStatus::Active)
            .count();
        egui::CollapsingHeader::new(format!("Invitation history · {historical_count}"))
            .default_open(false)
            .show(ui, |ui| {
                for invitation in status.invitation_history.iter()
                    .filter(|record| record.status != shocking_vrc_remote::auth::InvitationStatus::Active)
                {
                    let status_label = format!("{:?}", invitation.status).to_ascii_uppercase();
                    let status_time = invitation.accepted_at.or(invitation.expired_at).or(invitation.revoked_at)
                        .map(host_time).unwrap_or_else(|| "—".to_owned());
                    ui.horizontal_wrapped(|ui| {
                        w::body(ui, pal, invitation.description.as_deref().unwrap_or("Invitation"));
                        w::dim(ui, pal, format!("{status_label} · {status_time}"));
                        w::dim(ui, pal, format!("Created {}", host_time(invitation.created_at)));
                        if let Some(device_id) = &invitation.consumed_by_device_id {
                            w::dim(ui, pal, format!("Paired device {}", &device_id.chars().take(8).collect::<String>()));
                        }
                    });
                }
                if historical_count == 0 {
                    w::dim(ui, pal, "Accepted, expired and revoked invitations appear here.");
                }
            });

        ui.add_space(space::ITEM_GAP);
        w::heading(ui, pal, "PAIRED DEVICES");
        if status.paired_terminals.is_empty() {
            w::dim(ui, pal, "No Terminals are paired.");
        } else {
            for terminal in &status.paired_terminals {
                let device_id = terminal.device_id.clone();
                let current_name = terminal.friendly_name.as_deref()
                    .filter(|name| !name.trim().is_empty()).unwrap_or("Terminal");
                ui.horizontal_wrapped(|ui| {
                    w::body(ui, pal, current_name);
                    if w::danger_button(ui, pal, "Revoke access", true).clicked() {
                        revoke_terminals.push(device_id.clone());
                    }
                });
                w::dim(ui, pal, format!("Paired {}", host_time(terminal.paired_at)));
                egui::CollapsingHeader::new("Connection details")
                    .id_salt(("paired-terminal-details", terminal.device_id.as_str()))
                    .default_open(false)
                    .show(ui, |ui| {
                        w::body(ui, pal, format!("Device ID: {}", terminal.device_id));
                        w::body(ui, pal, format!("Invitation: {}", terminal.invitation_id.map(|id| id.to_string()).unwrap_or_else(|| "Legacy pairing".to_owned())));
                    });
            }
        }

        ui.add_space(space::ROW_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "API session", 220.0, false);
            w::body(ui, pal, status.session.as_str());
            if let Some(seconds) = status.last_contact_seconds_ago {
                w::dim(ui, pal, format!("Last API contact {seconds}s ago"));
            }
            disconnect = w::button_enabled(
                ui, pal, "Disconnect client",
                status.session != shocking_vrc_remote::auth::SessionPhase::Free,
            ).clicked();
        });
        w::dim(ui, pal, status.session.status_description());

        ui.add_space(space::ROW_GAP);
        ui.horizontal(|ui| {
            w::field_label(ui, pal, "Remote limits", 220.0, false);
            let mut remote_limit_enabled = settings.remote_strength.enabled;
            if ui.checkbox(&mut remote_limit_enabled, "Enable remote limit").changed() {
                settings.remote_strength.set_enabled(remote_limit_enabled);
                changed = true;
            }
        });
        w::dim(
            ui, pal,
            "Applies only to remote Power Limits min/max writes. Local controls and device safety limits are unchanged.",
        );

        for (label, cap) in [
            ("Channel A remote limit", &mut settings.remote_strength.max_by_channel.a),
            ("Channel B remote limit", &mut settings.remote_strength.max_by_channel.b),
        ] {
            ui.add_space(space::ROW_GAP);
            ui.horizontal(|ui| {
                w::field_label(ui, pal, label, 230.0, false);
                let value = (*cap).unwrap_or(200);
                let slider = w::Slider::new(value as f32, 0.0, 200.0).width(280.0);
                if let Some(next) = slider.show(ui, pal) {
                    let next = next.round().clamp(0.0, 200.0) as u8;
                    if *cap != Some(next) {
                        *cap = Some(next);
                        changed = true;
                    }
                }
                w::body(ui, pal, format!("{value}"));
                if cap.is_none() {
                    w::dim(ui, pal, "No cap saved; editing starts at 200.");
                }
            });
        }

        if let Some(error) = status.error.as_ref() {
            ui.add_space(space::ROW_GAP);
            ui.label(egui::RichText::new(error).font(font::small()).color(pal.warning));
        }
    });

    if refresh_interfaces {
        cx.ui_state.local_ipv4_interfaces = shocking_vrc_remote::available_local_ipv4();
    }
    if changed {
        cx.ui_state.remote_settings = settings.clone();
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.update_settings(settings).await {
                controller.report_ui_error(error);
            }
        });
    }
    if generate_invite {
        let controller = std::sync::Arc::clone(cx.remote);
        let description = cx.ui_state.invitation_description.trim().to_owned();
        cx.ui_state.invitation_description.clear();
        tokio::spawn(async move {
            if let Err(error) = controller.create_pairing_invitation(Some(description)).await {
                controller.report_ui_error(error);
            }
        });
    }
    for invitation_id in copy_invites {
        let controller = std::sync::Arc::clone(cx.remote);
        let clipboard = ui.ctx().clone();
        tokio::spawn(async move {
            if let Some(uri) = controller.invitation_uri(invitation_id).await {
                clipboard.copy_text(uri);
            }
        });
    }
    for invitation_id in revoke_invites {
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.revoke_pairing_invitation(invitation_id).await {
                controller.report_ui_error(error);
            }
        });
    }
    for device_id in revoke_terminals {
        let controller = std::sync::Arc::clone(cx.remote);
        tokio::spawn(async move {
            if let Err(error) = controller.revoke_terminal(device_id).await {
                controller.report_ui_error(error);
            }
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

fn host_time(timestamp: u64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_opt(timestamp.min(i64::MAX as u64) as i64, 0)
        .single()
        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "Unknown time".to_owned())
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
