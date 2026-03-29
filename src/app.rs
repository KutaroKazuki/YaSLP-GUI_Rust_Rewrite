#[cfg(not(target_os = "windows"))]
use std::io::Write;
use std::io::{BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use rayon::prelude::*;

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText, Stroke, Vec2};

use crate::fetch;
use crate::models::{AppSettings, ParamMode, Server};
use crate::settings as cfg_store;

// ── Palette ─────────────────────────────────────────────────────────────────
const C_BG: Color32 = Color32::from_rgb(10, 10, 20);
const C_PANEL: Color32 = Color32::from_rgb(18, 19, 36);
const C_CARD: Color32 = Color32::from_rgb(26, 28, 52);
const C_CARD_SEL: Color32 = Color32::from_rgb(40, 44, 80);
const C_BORDER: Color32 = Color32::from_rgb(50, 54, 100);
const C_ACCENT: Color32 = Color32::from_rgb(124, 131, 253);
const C_ACCENT2: Color32 = Color32::from_rgb(100, 108, 220);
const C_TEXT: Color32 = Color32::from_rgb(225, 226, 245);
const C_TEXT_DIM: Color32 = Color32::from_rgb(130, 134, 175);
const C_ONLINE: Color32 = Color32::from_rgb(74, 222, 128);
const C_OFFLINE: Color32 = Color32::from_rgb(100, 100, 130);
const C_WARN: Color32 = Color32::from_rgb(251, 191, 36);
const C_ERR: Color32 = Color32::from_rgb(248, 113, 113);

fn ping_color(ms: u128) -> Color32 {
    if ms < 100 {
        C_ONLINE
    } else if ms < 250 {
        C_WARN
    } else {
        C_ERR
    }
}

// ── Download state ───────────────────────────────────────────────────────────
#[derive(Default, PartialEq)]
enum DownloadState {
    #[default]
    Idle,
    Downloading,
    Done,
    Error(String),
}

// ── Load state machine ───────────────────────────────────────────────────────
#[derive(Default, PartialEq)]
enum LoadState {
    #[default]
    Idle,
    FetchingList,
    CheckingServers {
        done: usize,
        total: usize,
    },
    Done,
    Error(String),
}

// ── Shared state for background threads ─────────────────────────────────────
struct BgResult {
    servers: Vec<Server>,
    error: Option<String>,
    done: usize,
    total: usize,
    finished: bool,
}

// ── WinPcap / Npcap detection (Windows only) ────────────────────────────────
fn check_pcap() -> bool {
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        use winreg::RegKey;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        hklm.open_subkey("SOFTWARE\\Npcap").is_ok()
            || hklm.open_subkey("SOFTWARE\\WOW6432Node\\Npcap").is_ok()
            || hklm.open_subkey("SOFTWARE\\WinPcap").is_ok()
            || hklm.open_subkey("SOFTWARE\\WOW6432Node\\WinPcap").is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    true
}

// ── Main application ─────────────────────────────────────────────────────────
pub struct YaSLPApp {
    settings: AppSettings,
    servers: Vec<Server>,
    selected: Option<usize>,
    load_state: LoadState,
    show_settings: bool,
    show_quickconnect: bool,
    show_about: bool,
    hide_offline: bool,
    status_msg: String,

    // Background result channel
    bg: Option<Arc<Mutex<BgResult>>>,
    // Download state
    dl_state: DownloadState,
    dl_bg: Option<Arc<Mutex<Option<Result<(), String>>>>>,

    // WinPcap / Npcap present (always true on non-Windows)
    pcap_ok: bool,

    // Running lan-play process
    lan_play_child: Option<Child>,
    /// Elevated process handle (Windows only, from ShellExecuteExW with runas).
    #[cfg(target_os = "windows")]
    lan_play_elevated_handle: Option<isize>,
    /// Job object that contains cmd.exe + lan-play; TerminateJobObject kills both.
    #[cfg(target_os = "windows")]
    lan_play_job_handle: Option<isize>,

    // Console output window
    show_console: bool,
    console_cmd: String,
    console_output: Arc<Mutex<String>>,
    /// Length (bytes) of console_output the last time we rendered — used to
    /// detect new content so we can auto-scroll to bottom only then.
    console_last_len: usize,

    // Privileged (sudo) run state — Linux only
    #[cfg(not(target_os = "windows"))]
    show_sudo_dialog: bool,
    #[cfg(not(target_os = "windows"))]
    sudo_password: String,
    #[cfg(not(target_os = "windows"))]
    pending_connect_addr: String,
    #[cfg(not(target_os = "windows"))]
    sudo_password_for_kill: Option<String>,
    /// Exact PID of lan-play (child of sudo), resolved after spawn via /proc.
    #[cfg(not(target_os = "windows"))]
    lan_play_exact_pid: Arc<Mutex<Option<u32>>>,

    // Settings window edit buffer
    edit: AppSettings,
    // Quick-connect address buffer
    qc_addr: String,
}

impl Default for YaSLPApp {
    fn default() -> Self {
        let settings = cfg_store::load();
        let edit = settings.clone();
        Self {
            settings,
            servers: Vec::new(),
            selected: None,
            load_state: LoadState::Idle,
            show_settings: false,
            show_quickconnect: false,
            show_about: false,
            hide_offline: true,
            status_msg: "Ready — click Refresh to load servers.".into(),
            bg: None,
            dl_state: DownloadState::Idle,
            dl_bg: None,
            pcap_ok: check_pcap(),
            lan_play_child: None,
            #[cfg(target_os = "windows")]
            lan_play_elevated_handle: None,
            #[cfg(target_os = "windows")]
            lan_play_job_handle: None,
            show_console: false,
            console_cmd: String::new(),
            console_output: Arc::new(Mutex::new(String::new())),
            console_last_len: 0,
            #[cfg(not(target_os = "windows"))]
            show_sudo_dialog: false,
            #[cfg(not(target_os = "windows"))]
            sudo_password: String::new(),
            #[cfg(not(target_os = "windows"))]
            pending_connect_addr: String::new(),
            #[cfg(not(target_os = "windows"))]
            sudo_password_for_kill: None,
            #[cfg(not(target_os = "windows"))]
            lan_play_exact_pid: Arc::new(Mutex::new(None)),
            edit,
            qc_addr: String::new(),
        }
    }
}

impl eframe::App for YaSLPApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.poll_bg(ctx);
        self.poll_dl_bg();

        self.draw_top_bar(ctx);
        self.draw_side_panel(ctx);
        self.draw_server_list(ctx);
        self.draw_bottom_bar(ctx);

        if self.show_settings {
            self.draw_settings_window(ctx);
        }
        if self.show_quickconnect {
            self.draw_quickconnect_window(ctx);
        }
        if self.show_about {
            self.draw_about_window(ctx);
        }
        if self.show_console {
            self.draw_console_window(ctx);
        }
        #[cfg(not(target_os = "windows"))]
        if self.show_sudo_dialog {
            self.draw_sudo_dialog(ctx);
        }
    }
}

impl YaSLPApp {
    // ── Theme ───────────────────────────────────────────────────────────────
    fn apply_theme(&self, ctx: &egui::Context) {
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = C_PANEL;
        vis.window_fill = C_CARD;
        vis.override_text_color = Some(C_TEXT);
        vis.widgets.noninteractive.bg_fill = C_CARD;
        vis.widgets.noninteractive.bg_stroke = Stroke::new(1.0, C_BORDER);
        vis.widgets.inactive.bg_fill = C_CARD;
        vis.widgets.inactive.bg_stroke = Stroke::new(1.0, C_BORDER);
        vis.widgets.hovered.bg_fill = C_CARD_SEL;
        vis.widgets.hovered.bg_stroke = Stroke::new(1.5, C_ACCENT);
        vis.widgets.active.bg_fill = C_ACCENT2;
        vis.widgets.active.bg_stroke = Stroke::new(1.5, C_ACCENT);
        vis.selection.bg_fill = C_ACCENT2;
        vis.extreme_bg_color = C_BG;
        vis.faint_bg_color = C_CARD;
        vis.window_shadow.blur = 16;
        vis.window_shadow.color = Color32::from_black_alpha(100);
        ctx.set_visuals(vis);
    }

    // ── Top bar ─────────────────────────────────────────────────────────────
    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .exact_height(56.0)
            .frame(
                egui::Frame::NONE
                    .fill(C_BG)
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Logo circle
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::hover());
                    let painter = ui.painter();
                    painter.circle_filled(rect.center(), 18.0, C_ACCENT);
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "⊕",
                        FontId::proportional(20.0),
                        Color32::WHITE,
                    );

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("YaSLP-GUI")
                            .font(FontId::proportional(22.0))
                            .color(C_TEXT)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("Switch LAN Play")
                            .font(FontId::proportional(12.0))
                            .color(C_TEXT_DIM),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(accent_button("About"))
                            .on_hover_text("About YaSLP-GUI")
                            .clicked()
                        {
                            self.show_about = true;
                        }
                        ui.add_space(4.0);
                        if ui
                            .add(accent_button("⚙ Settings"))
                            .on_hover_text("Open settings")
                            .clicked()
                        {
                            self.edit = self.settings.clone();
                            self.show_settings = true;
                        }
                        ui.add_space(4.0);
                        if ui
                            .add(accent_button("⚡ Quick Connect"))
                            .on_hover_text("Connect directly to a relay")
                            .clicked()
                        {
                            self.show_quickconnect = true;
                        }
                        ui.add_space(4.0);
                        let refresh_label = match &self.load_state {
                            LoadState::FetchingList => "⟳ Fetching…",
                            LoadState::CheckingServers { .. } => "⟳ Checking…",
                            _ => "⟳ Refresh",
                        };
                        let refreshing = matches!(
                            self.load_state,
                            LoadState::FetchingList | LoadState::CheckingServers { .. }
                        );
                        let btn = egui::Button::new(
                            RichText::new(refresh_label)
                                .font(FontId::proportional(13.0))
                                .color(Color32::WHITE),
                        )
                        .fill(if refreshing { C_ACCENT2 } else { C_ACCENT })
                        .corner_radius(8.0)
                        .min_size(Vec2::new(110.0, 30.0));
                        if ui.add_enabled(!refreshing, btn).clicked() {
                            self.start_refresh();
                        }
                    });
                });
            });
    }

    // ── Server list (central panel) ─────────────────────────────────────────
    fn draw_server_list(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(C_PANEL).inner_margin(egui::Margin::same(12)))
            .show(ctx, |ui| {
                // Filter toggle
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Servers")
                            .font(FontId::proportional(15.0))
                            .color(C_TEXT)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.checkbox(&mut self.hide_offline, RichText::new("Hide offline").color(C_TEXT_DIM).small());
                        ui.add_space(8.0);
                        let online: usize = self.servers.iter().filter(|s| s.reachable).count();
                        ui.label(
                            RichText::new(format!("{online} online / {} total", self.servers.len()))
                                .small()
                                .color(C_TEXT_DIM),
                        );
                    });
                });
                ui.add_space(6.0);

                // Progress bar while checking
                if let LoadState::CheckingServers { done, total } = &self.load_state {
                    let progress = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
                    let pb = egui::ProgressBar::new(progress)
                        .show_percentage()
                        .desired_width(ui.available_width());
                    ui.add(pb);
                    ui.add_space(6.0);
                }

                let mut visible: Vec<(usize, &Server)> = self
                    .servers
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.entry.is_hidden())
                    .filter(|(_, s)| !self.hide_offline || s.reachable)
                    .collect();
                visible.sort_by(|(_, a), (_, b)| {
                    match (a.ping_ms, b.ping_ms) {
                        (Some(pa), Some(pb)) => pa.cmp(&pb),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    }
                });

                if visible.is_empty() {
                    ui.centered_and_justified(|ui| {
                        let msg = match &self.load_state {
                            LoadState::Idle => "Click Refresh to load servers.",
                            LoadState::FetchingList => "Fetching server list…",
                            LoadState::CheckingServers { .. } => "Checking servers…",
                            LoadState::Error(e) => e.as_str(),
                            LoadState::Done => "No servers found.",
                        };
                        ui.label(
                            RichText::new(msg)
                                .font(FontId::proportional(15.0))
                                .color(C_TEXT_DIM),
                        );
                    });
                    return;
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // Header row
                    draw_table_header(ui);
                    ui.add_space(2.0);
                    ui.painter().hline(
                        ui.available_rect_before_wrap().x_range(),
                        ui.cursor().top(),
                        Stroke::new(1.0, C_BORDER),
                    );
                    ui.add_space(4.0);

                    for (idx, server) in visible {
                        let is_selected = self.selected == Some(idx);
                        let row_color = if is_selected { C_CARD_SEL } else { C_CARD };
                        let resp = draw_server_row(ui, server, row_color, is_selected);
                        if resp.clicked() {
                            self.selected = Some(idx);
                        }
                    }
                });
            });
    }

    // ── Side panel (server details + connect) ────────────────────────────────
    fn draw_side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("side")
            .min_width(220.0)
            .max_width(260.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(C_BG)
                    .inner_margin(egui::Margin::same(14))
                    .stroke(Stroke::new(1.0, C_BORDER)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Server Details")
                        .font(FontId::proportional(14.0))
                        .color(C_TEXT_DIM)
                        .strong(),
                );
                ui.add_space(10.0);

                if let Some(idx) = self.selected {
                    if let Some(s) = self.servers.get(idx) {
                        let s = s.clone();
                        detail_row(ui, "Address", &s.entry.addr());
                        detail_row(ui, "Type", &s.entry.type_str());
                        detail_row(
                            ui,
                            "Status",
                            if s.reachable { "● Online" } else { "○ Offline" },
                        );
                        if let Some(flag) = &s.entry.flag {
                            detail_row(ui, "Region", flag);
                        }
                        detail_row(ui, "Version", s.version());
                        detail_row(ui, "Online", &s.online_count().to_string());
                        detail_row(ui, "Idle", &s.idle_count().to_string());
                        detail_row(ui, "Active", &s.active_count().to_string());
                        detail_row(ui, "Ping", &s.ping_label());

                        ui.add_space(20.0);
                        ui.add(egui::Separator::default());
                        ui.add_space(10.0);

                        if self.is_connected() {
                            let btn = egui::Button::new(
                                RichText::new("■  Disconnect")
                                    .font(FontId::proportional(14.0))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(C_ERR)
                            .corner_radius(8.0)
                            .min_size(Vec2::new(0.0, 32.0));
                            if ui.add(btn).clicked() {
                                self.disconnect(ctx);
                            }
                        } else {
                            let binary_exists = std::path::Path::new(&self.settings.client_binary()).exists();
                            let can_connect = s.reachable && binary_exists && self.pcap_ok;
                            let btn = egui::Button::new(
                                RichText::new("▶  Connect")
                                    .font(FontId::proportional(14.0))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(if can_connect { C_ACCENT } else { C_BORDER })
                            .corner_radius(8.0)
                            .min_size(Vec2::new(0.0, 32.0));
                            if ui.add_enabled(can_connect, btn).clicked() {
                                self.connect_to(&s.entry.addr(), ctx);
                            }
                            if !self.pcap_ok {
                                ui.label(
                                    RichText::new("WinPcap / Npcap not installed")
                                        .small()
                                        .color(C_ERR),
                                );
                            } else if !binary_exists {
                                ui.label(
                                    RichText::new("lan-play client not found — download it below")
                                        .small()
                                        .color(C_WARN),
                                );
                            } else if !s.reachable {
                                ui.label(
                                    RichText::new("Server is offline")
                                        .small()
                                        .color(C_TEXT_DIM),
                                );
                            }
                        }

                        ui.add_space(8.0);
                        self.draw_download_button(ui, ctx);
                    }
                } else {
                    ui.label(
                        RichText::new("Select a server\nto see details.")
                            .color(C_TEXT_DIM)
                            .italics(),
                    );
                    ui.add_space(20.0);
                    ui.add(egui::Separator::default());
                    ui.add_space(10.0);
                    self.draw_download_button(ui, ctx);
                }
            });
    }

    // ── Bottom status bar ────────────────────────────────────────────────────
    fn draw_bottom_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("statusbar")
            .exact_height(26.0)
            .frame(
                egui::Frame::NONE
                    .fill(C_BG)
                    .inner_margin(egui::Margin::symmetric(12, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&self.status_msg)
                            .small()
                            .color(C_TEXT_DIM),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(concat!("YaSLP-GUI v", env!("CARGO_PKG_VERSION")))
                                .small()
                                .color(C_TEXT_DIM),
                        );
                    });
                });
            });
    }

    // ── Settings window ──────────────────────────────────────────────────────
    fn draw_settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new(RichText::new("⚙  Settings").color(C_TEXT).strong())
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(480.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(C_CARD)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        // HTTP Timeout
                        ui.label(RichText::new("HTTP Timeout (ms)").color(C_TEXT));
                        let mut t = self.edit.http_timeout_ms.to_string();
                        if ui.add(egui::TextEdit::singleline(&mut t).desired_width(80.0)).changed() {
                            if let Ok(v) = t.parse::<u64>() {
                                self.edit.http_timeout_ms = v;
                            }
                        }
                        ui.end_row();

                        // Server list URL
                        ui.label(RichText::new("Server List URL").color(C_TEXT));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.edit.server_list_url)
                                .desired_width(340.0),
                        );
                        ui.end_row();

                        // Preset buttons
                        ui.label(RichText::new("Presets").color(C_TEXT_DIM).small());
                        ui.horizontal(|ui| {
                            if ui.add(accent_button("GreatWizard")).clicked() {
                                self.edit.server_list_url = "https://raw.githubusercontent.com/GreatWizard/lan-play-status/master/src/data/servers.json".into();
                            }
                        });
                        ui.end_row();

                        // Client directory
                        ui.label(RichText::new("Client Directory").color(C_TEXT));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.edit.client_dir)
                                    .desired_width(260.0),
                            );
                            if ui.add(accent_button("Browse…")).clicked() {
                                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                    self.edit.client_dir = path.display().to_string();
                                }
                            }
                        });
                        ui.end_row();

                        // Parameter mode
                        ui.label(RichText::new("Launch Mode").color(C_TEXT));
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.edit.param_mode,
                                ParamMode::Default,
                                "Default",
                            );
                            ui.selectable_value(
                                &mut self.edit.param_mode,
                                ParamMode::Acnh,
                                "ACNH",
                            );
                            ui.selectable_value(
                                &mut self.edit.param_mode,
                                ParamMode::Custom,
                                "Custom",
                            );
                        });
                        ui.end_row();

                        // Run as Administrator (Windows only)
                        #[cfg(target_os = "windows")]
                        {
                            ui.label(RichText::new("Run as Administrator").color(C_TEXT));
                            ui.checkbox(&mut self.edit.privileged, "UAC prompt on connect (recommended)");
                            ui.end_row();
                        }

                        // Custom params
                        if self.edit.param_mode == ParamMode::Custom {
                            ui.label(RichText::new("Custom Params").color(C_TEXT));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.edit.custom_params)
                                    .desired_width(340.0),
                            );
                            ui.end_row();
                        }

                    });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Save").color(Color32::WHITE),
                            )
                            .fill(C_ACCENT)
                            .corner_radius(6.0)
                            .min_size(Vec2::new(80.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.settings = self.edit.clone();
                        cfg_store::save(&self.settings);
                        self.show_settings = false;
                        self.status_msg = "Settings saved.".into();
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Cancel").color(C_TEXT_DIM))
                                .fill(C_CARD)
                                .stroke(Stroke::new(1.0, C_BORDER))
                                .corner_radius(6.0)
                                .min_size(Vec2::new(80.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.show_settings = false;
                    }
                });
            });
        self.show_settings = open;
    }

    // ── Quick Connect window ─────────────────────────────────────────────────
    fn draw_quickconnect_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_quickconnect;
        egui::Window::new(RichText::new("⚡  Quick Connect").color(C_TEXT).strong())
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(360.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(C_CARD)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new("Relay server address (host:port)").color(C_TEXT_DIM).small());
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.qc_addr)
                        .hint_text("e.g. lan-play.example.com:11451")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if self.lan_play_child.is_some() {
                        let btn = egui::Button::new(
                            RichText::new("■  Disconnect").color(Color32::WHITE),
                        )
                        .fill(C_ERR)
                        .corner_radius(6.0)
                        .min_size(Vec2::new(100.0, 28.0));
                        if ui.add(btn).clicked() {
                            self.disconnect(ctx);
                            self.show_quickconnect = false;
                        }
                    } else {
                        let binary_exists = std::path::Path::new(&self.settings.client_binary()).exists();
                        let can = !self.qc_addr.trim().is_empty() && binary_exists && self.pcap_ok;
                        let btn = egui::Button::new(
                            RichText::new("▶  Connect").color(Color32::WHITE),
                        )
                        .fill(if can { C_ACCENT } else { C_BORDER })
                        .corner_radius(6.0)
                        .min_size(Vec2::new(100.0, 28.0));
                        if ui.add_enabled(can, btn).clicked() {
                            let addr = self.qc_addr.trim().to_string();
                            self.connect_to(&addr, ctx);
                            self.show_quickconnect = false;
                        }
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Cancel").color(C_TEXT_DIM))
                                .fill(C_CARD)
                                .stroke(Stroke::new(1.0, C_BORDER))
                                .corner_radius(6.0)
                                .min_size(Vec2::new(80.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.show_quickconnect = false;
                    }
                });
            });
        self.show_quickconnect = open;
    }

    // ── About window ─────────────────────────────────────────────────────────
    fn draw_about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new(RichText::new("About YaSLP-GUI").color(C_TEXT).strong())
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .min_width(340.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(C_CARD)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    // Logo
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), egui::Sense::hover());
                    let p = ui.painter();
                    p.circle_filled(rect.center(), 32.0, C_ACCENT);
                    p.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "⊕",
                        FontId::proportional(34.0),
                        Color32::WHITE,
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("YaSLP-GUI")
                            .font(FontId::proportional(20.0))
                            .color(C_TEXT)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(concat!("v", env!("CARGO_PKG_VERSION"), "  —  Rust Edition"))
                            .color(C_ACCENT)
                            .small(),
                    );
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.label(RichText::new("Yet another Switch LAN Play GUI").color(C_TEXT_DIM));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Cross-platform rewrite of the original C# app.\n\
                             Works on Windows, Linux (X11 & Wayland).",
                        )
                        .color(C_TEXT_DIM)
                        .small(),
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new("License: GPL-3.0").color(C_TEXT_DIM).small());
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("LAN Play client: github.com/spacemeowx2/switch-lan-play")
                            .color(C_TEXT_DIM)
                            .small(),
                    );
                });
            });
        self.show_about = open;
    }

    // ── Background refresh logic ─────────────────────────────────────────────
    fn start_refresh(&mut self) {
        self.servers.clear();
        self.selected = None;
        self.status_msg = "Fetching server list…".into();
        self.load_state = LoadState::FetchingList;

        let settings = self.settings.clone();
        let shared = Arc::new(Mutex::new(BgResult {
            servers: Vec::new(),
            error: None,
            done: 0,
            total: 0,
            finished: false,
        }));
        self.bg = Some(shared.clone());

        thread::spawn(move || {
            let finish = |shared: &Arc<Mutex<BgResult>>, error: Option<String>, servers: Vec<Server>| {
                if let Ok(mut g) = shared.lock() {
                    g.error = error;
                    g.servers = servers;
                    g.finished = true;
                }
            };

            // Build one client for the whole refresh cycle
            let client = match fetch::build_client(settings.http_timeout_ms) {
                Ok(c) => c,
                Err(e) => { finish(&shared, Some(e), vec![]); return; }
            };

            let entries = match fetch::fetch_server_list(&settings) {
                Ok(e) => e,
                Err(e) => { finish(&shared, Some(e), vec![]); return; }
            };

            let servers: Vec<_> = entries
                .into_iter()
                .filter(|e| e.ip.is_some())
                .map(Server::from_entry)
                .collect();

            let total = servers.len();
            if let Ok(mut g) = shared.lock() {
                g.total = total;
            }

            let done_count = Arc::new(AtomicUsize::new(0));
            let results: Vec<Server> = servers.into_par_iter().map(|s| {
                let checked = fetch::check_server(&client, s);
                let done = done_count.fetch_add(1, Ordering::Relaxed) + 1;
                if let Ok(mut g) = shared.lock() {
                    g.done = done;
                    g.servers.push(checked.clone());
                }
                checked
            }).collect();

            finish(&shared, None, results);
        });
    }

    fn poll_bg(&mut self, ctx: &egui::Context) {
        let Some(shared) = &self.bg else { return };
        let (done, total, finished, error, servers) = {
            let g = shared.lock().unwrap_or_else(|e| e.into_inner());
            (g.done, g.total, g.finished, g.error.clone(), g.servers.clone())
        };

        if finished {
            if let Some(e) = error {
                self.load_state = LoadState::Error(e.clone());
                self.status_msg = format!("Error: {e}");
            } else {
                let online = servers.iter().filter(|s| s.reachable).count();
                self.servers = servers;
                self.load_state = LoadState::Done;
                self.status_msg = format!(
                    "Loaded {} servers — {} online.",
                    self.servers.len(),
                    online
                );
            }
            self.bg = None;
        } else {
            self.load_state = LoadState::CheckingServers { done, total };
            self.servers = servers;
            self.status_msg = format!("Checking servers… {done}/{total}");
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn is_connected(&self) -> bool {
        if self.lan_play_child.is_some() { return true; }
        #[cfg(target_os = "windows")]
        if self.lan_play_elevated_handle.is_some() { return true; }
        false
    }

    // ── Connect to a relay server ────────────────────────────────────────────

    fn connect_to(&mut self, addr: &str, ctx: &egui::Context) {
        let binary = self.settings.client_binary();
        let extra = self.settings.build_params();
        let mut lan_args: Vec<String> = Vec::new();
        if !extra.is_empty() {
            lan_args.extend(extra.split_whitespace().map(String::from));
        }
        lan_args.push("--relay-server-addr".into());
        lan_args.push(addr.to_string());
        // Disable C-runtime buffering on stdout/stderr so output arrives
        // immediately even when lan-play's stdout/stderr is a pipe, not a TTY.
        lan_args.push("--set-ionbf".into());

        #[cfg(not(target_os = "windows"))]
        if self.settings.privileged {
            // Show password dialog; actual spawn happens after the user confirms.
            self.pending_connect_addr = addr.to_string();
            self.sudo_password.clear();
            self.show_sudo_dialog = true;
            return;
        }
        #[cfg(target_os = "windows")]
        if self.settings.privileged {
            let cmd_display = format!("{} {}", binary, lan_args.join(" "));
            self.spawn_elevated_windows(binary, lan_args, cmd_display, ctx);
            return;
        }
        let cmd_display = format!("{} {}", binary, lan_args.join(" "));
        self.spawn_process(binary, lan_args, cmd_display, ctx);
    }

    fn spawn_process(
        &mut self,
        exe: String,
        args: Vec<String>,
        cmd_display: String,
        ctx: &egui::Context,
    ) {
        let mut cmd = Command::new(&exe);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        match cmd.spawn()
        {
            Ok(mut child) => {
                let output = Arc::new(Mutex::new(String::new()));
                self.console_output = output.clone();
                self.console_cmd = cmd_display;
                self.show_console = true;

                // Spawn reader for stdout
                if let Some(stdout) = child.stdout.take() {
                    let out = output.clone();
                    let ctx2 = ctx.clone();
                    thread::spawn(move || {
                        let mut reader = BufReader::new(stdout);
                        let mut buf = vec![0u8; 1024];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let s = String::from_utf8_lossy(&buf[..n]);
                                    if let Ok(mut g) = out.lock() { g.push_str(&s); }
                                    ctx2.request_repaint();
                                }
                            }
                        }
                    });
                }

                // Spawn reader for stderr
                if let Some(stderr) = child.stderr.take() {
                    let out = output.clone();
                    let ctx2 = ctx.clone();
                    thread::spawn(move || {
                        let mut reader = BufReader::new(stderr);
                        let mut buf = vec![0u8; 1024];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let s = String::from_utf8_lossy(&buf[..n]);
                                    if let Ok(mut g) = out.lock() { g.push_str(&s); }
                                    ctx2.request_repaint();
                                }
                            }
                        }
                    });
                }

                let addr = args.last().cloned().unwrap_or_default();
                self.lan_play_child = Some(child);
                self.status_msg = format!("Connected to {addr}");
            }
            Err(e) => {
                self.status_msg = format!("Failed to launch: {e}");
            }
        }
    }

    // ── Elevated (RunAs) connect — Windows only ──────────────────────────────

    #[cfg(target_os = "windows")]
    fn spawn_elevated_windows(
        &mut self,
        exe: String,
        args: Vec<String>,
        cmd_display: String,
        ctx: &egui::Context,
    ) {
        use std::io::BufReader;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE, TRUE};
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW};
        use windows_sys::Win32::UI::Shell::{
            ShellExecuteExW, SHELLEXECUTEINFOW, SHELLEXECUTEINFOW_0,
        };

        static PIPE_COUNTER: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);

        fn to_wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        // Unique named pipe for this launch so we can read stdout/stderr
        let pipe_id = PIPE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pipe_name = format!("\\\\.\\pipe\\yaslp_{}_{}", std::process::id(), pipe_id);
        let wide_pipe = to_wide(&pipe_name);

        let pipe_handle = unsafe {
            CreateNamedPipeW(
                wide_pipe.as_ptr(),
                0x00000001, // PIPE_ACCESS_INBOUND
                0x00000000, // PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT
                1,          // nMaxInstances
                4096,
                4096,
                0,                                        // default timeout
                std::ptr::null::<SECURITY_ATTRIBUTES>(), // default security descriptor
            )
        };

        if pipe_handle == INVALID_HANDLE_VALUE {
            self.status_msg = "Failed to create output pipe.".into();
            return;
        }

        // Create a job object so TerminateJobObject kills cmd.exe + lan-play together
        let job_handle = unsafe {
            CreateJobObjectW(std::ptr::null::<SECURITY_ATTRIBUTES>(), std::ptr::null())
        };

        // Build: cmd.exe /c "<exe> <args> > \\.\pipe\... 2>&1"
        let exe_quoted = if exe.contains(' ') {
            format!("\"{}\"", exe)
        } else {
            exe.clone()
        };
        let args_str = args
            .iter()
            .map(|a| if a.contains(' ') { format!("\"{}\"", a) } else { a.clone() })
            .collect::<Vec<_>>()
            .join(" ");
        let cmd_params = format!("/c \"{} {} > {} 2>&1\"", exe_quoted, args_str, pipe_name);

        let verb = to_wide("runas");
        let cmd_exe = to_wide("cmd.exe");
        let params_wide = to_wide(&cmd_params);

        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: 0x00000040, // SEE_MASK_NOCLOSEPROCESS
            hwnd: 0,
            lpVerb: verb.as_ptr(),
            lpFile: cmd_exe.as_ptr(),
            lpParameters: params_wide.as_ptr(),
            lpDirectory: std::ptr::null(),
            nShow: 0, // SW_HIDE
            hInstApp: 0,
            lpIDList: std::ptr::null_mut(),
            lpClass: std::ptr::null(),
            hkeyClass: 0,
            dwHotKey: 0,
            Anonymous: SHELLEXECUTEINFOW_0 { hIcon: 0 },
            hProcess: 0,
        };

        let ok = unsafe { ShellExecuteExW(&mut sei) };

        if ok == TRUE {
            let output = Arc::new(Mutex::new(String::new()));
            self.console_output = output.clone();
            self.console_cmd = cmd_display;
            self.show_console = true;

            // Reader thread: wait for cmd.exe to connect the pipe then stream output
            let ctx2 = ctx.clone();
            thread::spawn(move || {
                unsafe { ConnectNamedPipe(pipe_handle, std::ptr::null_mut()) };
                let raw = pipe_handle as usize as *mut std::ffi::c_void;
                let file = unsafe { std::fs::File::from_raw_handle(raw) };
                let mut reader = BufReader::new(file);
                let mut buf = vec![0u8; 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let s = String::from_utf8_lossy(&buf[..n]);
                            if let Ok(mut g) = output.lock() { g.push_str(&s); }
                            ctx2.request_repaint();
                        }
                    }
                }
            });

            // Assign cmd.exe (and by inheritance, lan-play) to the job so
            // TerminateJobObject kills the entire tree on disconnect.
            if job_handle != 0 {
                unsafe { AssignProcessToJobObject(job_handle, sei.hProcess) };
                self.lan_play_job_handle = Some(job_handle);
            }

            let addr = args.last().cloned().unwrap_or_default();
            self.lan_play_elevated_handle = Some(sei.hProcess);
            self.status_msg = format!("Connected to {addr} (Administrator)");
        } else {
            unsafe { CloseHandle(pipe_handle) };
            let err = unsafe { GetLastError() };
            if err == 1223 {
                // ERROR_CANCELLED — user dismissed UAC prompt
                self.status_msg = "Administrator prompt cancelled.".into();
            } else {
                self.status_msg = format!("Failed to start as Administrator (error {err}).");
            }
        }
        ctx.request_repaint();
    }

    // ── Privileged (sudo) connect — Linux only ───────────────────────────────

    #[cfg(not(target_os = "windows"))]
    /// Spawn `sudo -S <binary> <args>`, write the password to stdin, then
    /// start a background thread that resolves lan-play's exact child PID via
    /// /proc so we can kill it precisely on disconnect.
    fn spawn_privileged(
        &mut self,
        binary: String,
        lan_args: Vec<String>,
        password: String,
        ctx: &egui::Context,
    ) {
        let mut sudo_args = vec!["-S".to_string(), binary.clone()];
        sudo_args.extend(lan_args.iter().cloned());
        let cmd_display = format!("sudo {} {}", binary, lan_args.join(" "));

        match Command::new("sudo")
            .args(&sudo_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let sudo_pid = child.id();

                // Write the password to sudo's stdin synchronously, then
                // explicitly flush and drop (close) the pipe so sudo receives
                // a clean EOF after the password — required on Arch/newer sudo.
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(format!("{password}\n").as_bytes());
                    let _ = stdin.flush();
                    // stdin drops here, closing the pipe
                }

                // Store password for the kill step and reset the PID slot.
                self.sudo_password_for_kill = Some(password);
                if let Ok(mut g) = self.lan_play_exact_pid.lock() { *g = None; }

                // Resolve lan-play's exact PID (child of sudo) via /proc.
                let pid_arc = self.lan_play_exact_pid.clone();
                thread::spawn(move || {
                    let path = format!("/proc/{sudo_pid}/task/{sudo_pid}/children");
                    for _ in 0..50 {
                        thread::sleep(std::time::Duration::from_millis(100));
                        if let Ok(s) = std::fs::read_to_string(&path) {
                            if let Some(pid) = s.split_whitespace()
                                .next()
                                .and_then(|p| p.parse::<u32>().ok())
                            {
                                if let Ok(mut g) = pid_arc.lock() { *g = Some(pid); }
                                return;
                            }
                        }
                    }
                });

                let output = Arc::new(Mutex::new(String::new()));
                self.console_output = output.clone();
                self.console_cmd = cmd_display;
                self.show_console = true;

                if let Some(stdout) = child.stdout.take() {
                    let out = output.clone();
                    let ctx2 = ctx.clone();
                    thread::spawn(move || {
                        let mut reader = BufReader::new(stdout);
                        let mut buf = vec![0u8; 1024];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let s = String::from_utf8_lossy(&buf[..n]);
                                    if let Ok(mut g) = out.lock() { g.push_str(&s); }
                                    ctx2.request_repaint();
                                }
                            }
                        }
                    });
                }
                if let Some(stderr) = child.stderr.take() {
                    let out = output.clone();
                    let ctx2 = ctx.clone();
                    thread::spawn(move || {
                        let mut reader = BufReader::new(stderr);
                        let mut buf = vec![0u8; 1024];
                        loop {
                            match reader.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let s = String::from_utf8_lossy(&buf[..n]);
                                    if let Ok(mut g) = out.lock() { g.push_str(&s); }
                                    ctx2.request_repaint();
                                }
                            }
                        }
                    });
                }

                let addr = lan_args.last().cloned().unwrap_or_default();
                self.lan_play_child = Some(child);
                self.status_msg = format!("Connected to {addr}");
            }
            Err(e) => {
                self.status_msg = format!("Failed to launch: {e}");
            }
        }
    }

    // ── sudo password dialog — Linux only ───────────────────────────────────
    #[cfg(not(target_os = "windows"))]
    fn draw_sudo_dialog(&mut self, ctx: &egui::Context) {
        let mut do_connect = false;
        let mut do_cancel  = false;

        egui::Window::new(RichText::new("🔒  sudo password").color(C_TEXT).strong())
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .min_width(320.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(C_CARD)
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Privileged mode requires sudo. Enter your password:")
                        .color(C_TEXT_DIM).small(),
                );
                ui.add_space(8.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.sudo_password)
                        .password(true)
                        .hint_text("password")
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    do_connect = true;
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let can = !self.sudo_password.is_empty();
                    if ui.add_enabled(
                        can,
                        egui::Button::new(RichText::new("Connect").color(Color32::WHITE))
                            .fill(if can { C_ACCENT } else { C_BORDER })
                            .corner_radius(6.0)
                            .min_size(Vec2::new(90.0, 28.0)),
                    ).clicked() { do_connect = true; }
                    ui.add_space(8.0);
                    if ui.add(
                        egui::Button::new(RichText::new("Cancel").color(C_TEXT_DIM))
                            .fill(C_CARD)
                            .stroke(Stroke::new(1.0, C_BORDER))
                            .corner_radius(6.0)
                            .min_size(Vec2::new(70.0, 28.0)),
                    ).clicked() { do_cancel = true; }
                });
            });

        if do_connect && !self.sudo_password.is_empty() {
            let addr    = self.pending_connect_addr.clone();
            let binary  = self.settings.client_binary();
            let extra   = self.settings.build_params();
            let mut lan_args: Vec<String> = Vec::new();
            if !extra.is_empty() {
                lan_args.extend(extra.split_whitespace().map(String::from));
            }
            lan_args.push("--relay-server-addr".into());
            lan_args.push(addr);
            lan_args.push("--set-ionbf".into());
            let password = std::mem::take(&mut self.sudo_password);
            self.show_sudo_dialog = false;
            ctx.request_repaint();
            self.spawn_privileged(binary, lan_args, password, ctx);
        } else if do_cancel {
            self.sudo_password.clear();
            self.show_sudo_dialog = false;
            ctx.request_repaint();
        }
    }

    fn disconnect(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "windows")]
        if let Some(proc_handle) = self.lan_play_elevated_handle.take() {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::TerminateProcess;
            unsafe {
                // Prefer the job handle: TerminateJobObject kills cmd.exe AND lan-play.
                // Fall back to TerminateProcess (kills cmd.exe only) if job wasn't created.
                if let Some(job) = self.lan_play_job_handle.take() {
                    windows_sys::Win32::System::JobObjects::TerminateJobObject(job, 1);
                    CloseHandle(job);
                } else {
                    TerminateProcess(proc_handle, 1);
                }
                CloseHandle(proc_handle);
            }
            self.status_msg = "Disconnected.".into();
            self.show_console = false;
            ctx.request_repaint();
            return;
        }
        if let Some(mut child) = self.lan_play_child.take() {
            #[cfg(not(target_os = "windows"))]
            if let Some(password) = self.sudo_password_for_kill.take() {
                let pid = self.lan_play_exact_pid.lock().ok().and_then(|g| *g);

                // Step 1: refresh the sudo credential cache with the stored password.
                if let Ok(mut v) = Command::new("sudo")
                    .args(["-S", "-v"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(mut stdin) = v.stdin.take() {
                        let _ = stdin.write_all(format!("{password}\n").as_bytes());
                    }
                    let _ = v.wait();
                }

                // Step 2: kill lan-play by exact PID using the now-fresh cache
                // (sudo -n = non-interactive, no password prompt).
                if let Some(p) = pid {
                    let _ = Command::new("sudo")
                        .args(["-n", "kill", "-9", &p.to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .map(|mut c| c.wait());
                }
            }
            #[cfg(not(target_os = "windows"))]
            if let Ok(mut g) = self.lan_play_exact_pid.lock() { *g = None; }
            let _ = child.kill();
            let _ = child.wait();
            self.status_msg = "Disconnected.".into();
        }
        self.show_console = false;
        ctx.request_repaint();
    }

    // ── Download lan-play binary ─────────────────────────────────────────────
    fn start_download(&mut self, ctx: &egui::Context) {
        self.dl_state = DownloadState::Downloading;
        let dest = self.settings.client_binary();
        let shared: Arc<Mutex<Option<Result<(), String>>>> = Arc::new(Mutex::new(None));
        self.dl_bg = Some(shared.clone());
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let result = fetch::download_binary(&dest);
            if let Ok(mut g) = shared.lock() {
                *g = Some(result);
            }
            ctx2.request_repaint();
        });
    }

    fn poll_dl_bg(&mut self) {
        let Some(shared) = &self.dl_bg else { return };
        let result = {
            let g = shared.lock().unwrap_or_else(|e| e.into_inner());
            g.clone()
        };
        if let Some(result) = result {
            self.dl_bg = None;
            match result {
                Ok(()) => {
                    self.dl_state = DownloadState::Done;
                    self.status_msg = "Download complete.".into();
                }
                Err(e) => {
                    self.dl_state = DownloadState::Error(e.clone());
                    self.status_msg = format!("Download failed: {e}");
                }
            }
        }
    }

    // ── Console window ───────────────────────────────────────────────────────
    fn draw_console_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_console;
        egui::Window::new(RichText::new("Console").color(C_TEXT).strong())
            .open(&mut open)
            .resizable(true)
            .default_width(620.0)
            .default_height(340.0)
            .min_width(400.0)
            .min_height(200.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(Color32::from_rgb(8, 8, 16))
                    .stroke(Stroke::new(1.0, C_BORDER))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ctx, |ui| {
                // Command header
                ui.label(
                    RichText::new("$  ")
                        .color(C_ACCENT)
                        .font(FontId::monospace(11.0)),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&self.console_cmd)
                            .color(C_ONLINE)
                            .font(FontId::monospace(11.0)),
                    );
                });
                ui.add_space(4.0);
                ui.add(egui::Separator::default());
                ui.add_space(4.0);

                let output = self.console_output.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();

                // Detect new content so we can auto-scroll only when output grows.
                let new_content = output.len() != self.console_last_len;
                self.console_last_len = output.len();

                let available = ui.available_size();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(available.y - 4.0)
                    .show(ui, |ui| {
                        let mut text = output.as_str();
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .font(FontId::monospace(11.0))
                                .desired_width(f32::INFINITY)
                                .interactive(false)
                                .text_color(C_TEXT),
                        );
                        // Scroll to the layout cursor (end of content) whenever
                        // new output arrives. Between bursts the user can scroll
                        // up freely.
                        if new_content {
                            let bottom = ui.min_rect().max;
                            ui.scroll_to_rect(
                                egui::Rect::from_min_size(bottom, egui::Vec2::ZERO),
                                Some(Align::BOTTOM),
                            );
                        }
                    });
            });
        // Only allow the X button to close; never restore true from open.
        if !open {
            self.show_console = false;
        }
    }

    fn draw_download_button(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let dest = self.settings.client_binary();
        if std::path::Path::new(&dest).exists() {
            return;
        }
        let (dl_label, dl_color) = match &self.dl_state {
            DownloadState::Idle => ("⬇  Download lan-play", C_TEXT),
            DownloadState::Downloading => ("⟳  Downloading…", C_TEXT_DIM),
            DownloadState::Done => ("✓  Downloaded", C_ONLINE),
            DownloadState::Error(_) => ("✗  Download failed", C_ERR),
        };
        let can_dl = !matches!(self.dl_state, DownloadState::Downloading);
        let dl_btn = egui::Button::new(
            RichText::new(dl_label)
                .font(FontId::proportional(13.0))
                .color(dl_color),
        )
        .fill(C_CARD)
        .stroke(Stroke::new(1.0, C_BORDER))
        .corner_radius(6.0)
        .min_size(Vec2::new(0.0, 30.0));
        if ui.add_enabled(can_dl, dl_btn).on_hover_text(format!("Downloads to: {dest}")).clicked() {
            self.start_download(ctx);
        }
        if let DownloadState::Error(e) = &self.dl_state {
            ui.label(RichText::new(e).small().color(C_ERR));
        }
    }
}

// ── Helper widgets ────────────────────────────────────────────────────────────

fn accent_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).font(FontId::proportional(13.0)).color(C_TEXT))
        .fill(C_CARD)
        .stroke(Stroke::new(1.0, C_BORDER))
        .corner_radius(6.0)
        .min_size(Vec2::new(0.0, 28.0))
}

fn draw_table_header(ui: &mut egui::Ui) {
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 20.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let cy = rect.center().y;
        let x0 = rect.left();
        // Column x offsets must exactly match draw_server_row
        for (label, col_x) in [
            ("Server",  x0 + 30.0),
            ("Type",    x0 + 310.0),
            ("Online",  x0 + 382.0),
            ("Active",  x0 + 444.0),
            ("Ping",    x0 + 506.0),
            ("Version", x0 + 583.0),
        ] {
            painter.text(
                egui::pos2(col_x, cy),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                C_TEXT_DIM,
            );
        }
    }
}

fn draw_server_row(
    ui: &mut egui::Ui,
    server: &Server,
    bg: Color32,
    selected: bool,
) -> egui::Response {
    let height = 34.0;
    let width = ui.available_width().max(1.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();

        // Row background
        painter.rect_filled(rect, 6.0, bg);
        if selected {
            painter.rect_stroke(rect, 6.0, Stroke::new(1.0, C_ACCENT), egui::StrokeKind::Middle);
        }

        let mut x = rect.left() + 6.0;
        let cy = rect.center().y;

        // Status dot
        let dot_r = 5.0;
        let dot_color = if server.reachable { C_ONLINE } else { C_OFFLINE };
        painter.circle_filled(egui::pos2(x + dot_r, cy), dot_r, dot_color);
        x += 24.0;

        // Addr
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            server.entry.addr(),
            FontId::monospace(12.0),
            C_TEXT,
        );
        x += 280.0;

        // Type
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            server.entry.type_str(),
            FontId::proportional(11.0),
            C_TEXT_DIM,
        );
        x += 72.0;

        // Online
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            server.online_count().to_string(),
            FontId::proportional(12.0),
            if server.online_count() > 0 { C_TEXT } else { C_TEXT_DIM },
        );
        x += 62.0;

        // Active
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            server.active_count().to_string(),
            FontId::proportional(12.0),
            C_TEXT,
        );
        x += 62.0;

        // Ping
        let (ping_txt, ping_col) = match server.ping_ms {
            Some(ms) => (format!("{ms} ms"), ping_color(ms)),
            None => ("—".into(), C_TEXT_DIM),
        };
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            ping_txt,
            FontId::proportional(12.0),
            ping_col,
        );
        x += 77.0;

        // Version
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            server.version(),
            FontId::proportional(11.0),
            C_TEXT_DIM,
        );
    }

    ui.add_space(2.0);
    resp
}

fn detail_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(C_TEXT_DIM).small());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let color = match label {
                "Status" => {
                    if value.starts_with('●') { C_ONLINE } else { C_OFFLINE }
                }
                _ => C_TEXT,
            };
            ui.label(RichText::new(value).color(color).small().strong());
        });
    });
    ui.add_space(2.0);
}
