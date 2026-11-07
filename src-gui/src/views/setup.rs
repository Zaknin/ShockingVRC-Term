use egui::Ui;

use crate::app::Ctx;
use crate::bridge::Command;
use crate::theme::widgets::{self as w, font, space};

fn configured_ice_servers(stun: &str, turn: &str) -> Vec<String> {
    stun.lines()
        .chain(turn.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn registration_diagnostic_label(traversal: shocking_vrc_remote::TraversalState, error: Option<&str>) -> &'static str {
    match traversal {
        shocking_vrc_remote::TraversalState::Registered | shocking_vrc_remote::TraversalState::Active => "REGISTRATION_OK",
        shocking_vrc_remote::TraversalState::Registering => "REGISTRATION_PENDING",
        shocking_vrc_remote::TraversalState::Disabled => "REGISTRATION_DISABLED",
        shocking_vrc_remote::TraversalState::Error if error.is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("auth_required") || value.contains("authentication required")
        }) => "REGISTRATION_AUTH",
        shocking_vrc_remote::TraversalState::Error => "REGISTRATION_ERROR",
    }
}

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
    let mut run_local_probe = false;

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

        ui.horizontal_wrapped(|ui| {
            w::field_label(ui, pal, "Invitation route", 220.0, false);
            for (mode, label) in [
                (shocking_vrc_remote::InvitationMode::Direct, "LAN / Direct"),
                (shocking_vrc_remote::InvitationMode::Internet, "Internet / ICE-only"),
                (shocking_vrc_remote::InvitationMode::Hybrid, "Hybrid / Automatic"),
            ] {
                if ui.radio_value(&mut settings.invitation_mode, mode, label).changed() {
                    changed = true;
                }
            }
        });
        w::dim(ui, pal, match settings.invitation_mode {
            shocking_vrc_remote::InvitationMode::Direct => "Invitation includes only the Direct endpoint; share it only with the intended recipient.",
            shocking_vrc_remote::InvitationMode::Internet => "ICE-only invitation; no Direct endpoint or local IP is included.",
            shocking_vrc_remote::InvitationMode::Hybrid => "Invitation includes both routes. Terminal tries Direct first, then ICE fallback; the Direct endpoint is shared with the intended recipient.",
        });

        let readiness = shocking_vrc_remote::controller::invitation_route_readiness(&settings, &status);
        let invitation_ready = readiness.is_ok();
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
                    if settings.invitation_mode != shocking_vrc_remote::InvitationMode::Internet {
                        ui.horizontal(|ui| {
                            w::field_label(ui, pal, "Local URL", 220.0, false);
                            w::body(ui, pal, format!("https://localhost:{}", address.port()));
                        });
                    }
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
                    w::field_label(ui, pal, "Rendezvous", 220.0, false);
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
                    w::field_label(ui, pal, "Registration", 220.0, false);
                    w::dim(ui, pal, if status.registration_key_configured { "Key stored · registration status shown above" } else { "Key not configured" });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Registration result", 220.0, false);
                    w::dim(ui, pal, registration_diagnostic_label(status.traversal, status.error.as_deref()));
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "ICE servers configured", 220.0, false);
                    w::dim(ui, pal, match status.ice_servers_configured {
                        Some(true) => "Yes · active room",
                        Some(false) => "No ICE servers in active room",
                        None => "No active ICE room",
                    });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Rendezvous STUN", 220.0, false);
                    w::dim(ui, pal, match status.rendezvous_stun_received {
                        Some(true) => "Recommendations received",
                        Some(false) => "No STUN recommendations received",
                        None => "No active ICE room",
                    });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "TURN credentials", 220.0, false);
                    w::dim(ui, pal, match status.turn_credentials_received {
                        Some(true) => "Temporary credentials received for this room",
                        Some(false) => "No credentials received for this room",
                        None => "No active ICE room",
                    });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Local ICE candidates", 220.0, false);
                    w::dim(ui, pal, match status.local_candidates_gathered {
                        Some(true) => "Gathered",
                        Some(false) => "None reported",
                        None => "No active ICE room",
                    });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Selected ICE route", 220.0, false);
                    w::dim(ui, pal, status.ice_candidate_pair.as_deref().unwrap_or("No route selected"));
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
                    w::dim(ui, pal, "Stored with Windows DPAPI when saved.");
                });
                if settings.invitation_mode != shocking_vrc_remote::InvitationMode::Internet {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Direct status", 220.0, false);
                        let direct_configured = shocking_vrc_remote::settings::is_valid_direct_host(&settings.direct_host);
                        w::dim(ui, pal, if direct_configured { "Configured · not probed" } else { "Not configured · not probed" });
                    });
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Direct address (IP/domain)", 220.0, false)
                            .on_hover_text("A local IPv4 address works on the same PC/LAN. It is not automatically reachable from the Internet.");
                        changed |= ui.add(
                            egui::TextEdit::singleline(&mut settings.direct_host)
                                .hint_text("LAN IPv4 or public IP/domain")
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
                } else {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Direct endpoint", 220.0, false);
                        w::dim(ui, pal, "Not included in Internet / ICE-only invitations.");
                    });
                }
                ui.add_space(space::ROW_GAP);
                w::heading(ui, pal, "STUN discovery");
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "STUN status", 220.0, false);
                    w::dim(ui, pal, if !settings.traversal_enabled {
                        "Automatic Rendezvous discovery is available when traversal is enabled"
                    } else if cx.ui_state.remote_stun_servers_edit.trim().is_empty() {
                        "Automatic via Rendezvous · no custom STUN additions"
                    } else {
                        "Automatic via Rendezvous + custom STUN additions · not probed"
                    });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Custom STUN URIs (additional)", 220.0, false);
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut cx.ui_state.remote_stun_servers_edit)
                            .hint_text("stun:host:3478")
                            .desired_rows(2)
                            .desired_width(420.0),
                    );
                    if response.changed() {
                        settings.ice_server_urls = configured_ice_servers(
                            &cx.ui_state.remote_stun_servers_edit,
                            &cx.ui_state.remote_turn_servers_edit,
                        );
                        changed = true;
                    }
                });
                w::dim(ui, pal, "Rendezvous recommendations are requested automatically for each ICE room; custom STUN URIs are additional. STUN does not prove a TURN relay path.");

                ui.add_space(space::ROW_GAP);
                w::heading(ui, pal, "TURN relay");
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "TURN status", 220.0, false);
                    w::dim(ui, pal, if !settings.traversal_enabled {
                        "Temporary TURN credentials are requested when Rendezvous traversal is enabled"
                    } else if cx.ui_state.remote_turn_servers_edit.trim().is_empty() {
                        "Rendezvous credentials are requested per room · no custom TURN additions"
                    } else {
                        "Rendezvous credentials are requested per room · custom TURN URI added"
                    });
                });
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Custom TURN URIs (additional)", 220.0, false);
                    let response = ui.add(
                        egui::TextEdit::multiline(&mut cx.ui_state.remote_turn_servers_edit)
                            .hint_text("turn:host:3478 or turns:host:5349")
                            .desired_rows(2)
                            .desired_width(420.0),
                    );
                    if response.changed() {
                        settings.ice_server_urls = configured_ice_servers(
                            &cx.ui_state.remote_stun_servers_edit,
                            &cx.ui_state.remote_turn_servers_edit,
                        );
                        changed = true;
                    }
                });
                w::dim(ui, pal, "Temporary credentials come from Rendezvous for each authorized room. Custom URIs are additional and must not contain credentials.");
                ui.add_space(space::ROW_GAP);
                w::heading(ui, pal, "Local connectivity checks");
                ui.horizontal(|ui| {
                    w::field_label(ui, pal, "Probe interface", 220.0, false);
                    let selected = cx.ui_state.remote_probe_bind_address
                        .map(|address| address.to_string())
                        .unwrap_or_else(|| "Use OS routing (no interface bind)".to_owned());
                    egui::ComboBox::from_id_salt("remote-probe-interface")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut cx.ui_state.remote_probe_bind_address,
                                None,
                                "Use OS routing (no interface bind)",
                            );
                            for interface in &cx.ui_state.local_ipv4_interfaces {
                                ui.selectable_value(
                                    &mut cx.ui_state.remote_probe_bind_address,
                                    Some(interface.address.into()),
                                    format!("{} · {} · {}", interface.name, interface.kind, interface.address),
                                );
                            }
                        });
                });
                run_local_probe = w::button_enabled(
                    ui,
                    pal,
                    if cx.ui_state.remote_probe_running.load(std::sync::atomic::Ordering::Relaxed) {
                        "Checks running…"
                    } else {
                        "Run local connectivity checks"
                    },
                    !cx.ui_state.remote_probe_running.load(std::sync::atomic::Ordering::Relaxed),
                ).clicked();
                w::dim(ui, pal, "Runs DNS/TCP/TLS/WebSocket checks for Rendezvous and UDP STUN Binding only after you press the button. DNS uses the OS resolver; the selected local address binds TCP/UDP. It does not change routes, register, create a room, or contact TURN.");
                if let Ok(report) = cx.ui_state.remote_probe_results.read()
                    && let Some(report) = report.as_ref()
                {
                    ui.add(egui::Label::new(egui::RichText::new(report).font(font::small()).color(pal.dim)).wrap());
                }
                if status.listener == shocking_vrc_remote::ListenerState::Listening && connectivity_incomplete {
                    ui.add_space(space::ROW_GAP);
                    w::dim(ui, pal, "The local HTTPS listener is running, but no invitation route is configured yet.");
                }
                egui::CollapsingHeader::new("Host identity details").default_open(false).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        w::field_label(ui, pal, "Host ID", 220.0, false);
                        if let Some(host_id) = status.server_id.as_deref() {
                            w::body(ui, pal, host_id);
                            if w::button(ui, pal, "Copy Host ID").clicked() {
                                ui.ctx().copy_text(host_id.to_owned());
                            }
                        } else {
                            w::body(ui, pal, "Loading identity…");
                        }
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
        if let Err(reason) = &readiness {
            w::dim(ui, pal, reason);
        }
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
    let probe_rendezvous_url = settings.rendezvous_url.clone();
    let probe_ice_server_urls = settings.ice_server_urls.clone();
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
    if run_local_probe {
        let rendezvous_url = probe_rendezvous_url;
        let ice_server_urls = probe_ice_server_urls;
        let bind_address = cx.ui_state.remote_probe_bind_address;
        let results = std::sync::Arc::clone(&cx.ui_state.remote_probe_results);
        let running = std::sync::Arc::clone(&cx.ui_state.remote_probe_running);
        let repaint = ui.ctx().clone();
        if running.compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        ).is_ok() {
            if let Ok(mut report) = results.write() {
                *report = Some("Connectivity checks are running…".to_owned());
            }
            tokio::spawn(async move {
                let lines = shocking_vrc_remote::diagnostics::probe_local_services(
                    &rendezvous_url,
                    &ice_server_urls,
                    bind_address,
                ).await;
                if let Ok(mut report) = results.write() {
                    *report = Some(lines.iter().map(|line| line.display_line()).collect::<Vec<_>>().join("\n"));
                }
                running.store(false, std::sync::atomic::Ordering::Release);
                repaint.request_repaint();
            });
        }
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

#[cfg(test)]
mod tests {
    use super::{configured_ice_servers, registration_diagnostic_label};

    #[test]
    fn stun_and_turn_edits_merge_without_mixing_status_or_registration_state() {
        assert_eq!(
            configured_ice_servers(" stun:stun.example:3478\n\nstuns:secure.example:5349 ", "turn:relay.example:3478\nturns:relay.example:5349"),
            [
                "stun:stun.example:3478",
                "stuns:secure.example:5349",
                "turn:relay.example:3478",
                "turns:relay.example:5349",
            ]
        );
    }

    #[test]
    fn registration_auth_is_classified_separately_from_transport_and_other_errors() {
        use shocking_vrc_remote::TraversalState;
        assert_eq!(registration_diagnostic_label(TraversalState::Registered, None), "REGISTRATION_OK");
        assert_eq!(registration_diagnostic_label(TraversalState::Registering, None), "REGISTRATION_PENDING");
        assert_eq!(registration_diagnostic_label(TraversalState::Error, Some("auth_required")), "REGISTRATION_AUTH");
        assert_eq!(registration_diagnostic_label(TraversalState::Error, Some("socket closed")), "REGISTRATION_ERROR");
    }
}
