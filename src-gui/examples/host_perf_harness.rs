use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shocking_vrc_app::app_state::AppState;
use shocking_vrc_app::logger::LogBuffer;
use shocking_vrc_core::AvatarScanner;
use shocking_vrc_core::cli::{AlarmConfig, AlarmController};
use shocking_vrc_gui::app::GuiApp;
use shocking_vrc_gui::bridge;
use shocking_vrc_gui::fixtures;
use shocking_vrc_gui::remote_adapter::BridgeRemoteBackend;
use shocking_vrc_remote::{HostRemoteController, RemoteAccessSettings};

struct FrameSample {
    utc_ms: u128,
    elapsed_us: u128,
    frame_gap_us: u128,
    ui_us: u128,
    focused: bool,
    minimized: bool,
    scheduled_repaint: bool,
    width: f32,
    height: f32,
    repaint_requests: u64,
    bridge_repaint_requests: u64,
}

struct MeasuredGui {
    inner: GuiApp,
    output: PathBuf,
    repaint_requests: Arc<std::sync::atomic::AtomicU64>,
    bridge_repaint_requests: Arc<std::sync::atomic::AtomicU64>,
    started: Instant,
    previous_frame: Option<Instant>,
    close_after: Duration,
    close_requested: bool,
    samples: Vec<FrameSample>,
}

impl MeasuredGui {
    fn new(
        inner: GuiApp,
        output: PathBuf,
        repaint_requests: Arc<std::sync::atomic::AtomicU64>,
        bridge_repaint_requests: Arc<std::sync::atomic::AtomicU64>,
        close_after: Duration,
    ) -> Self {
        Self {
            inner,
            output,
            repaint_requests,
            bridge_repaint_requests,
            started: Instant::now(),
            previous_frame: None,
            close_after,
            close_requested: false,
            samples: Vec::with_capacity(20_000),
        }
    }
}

impl eframe::App for MeasuredGui {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let started = Instant::now();
        let (focused, minimized) = ui.ctx().input(|input| {
            let viewport = input.viewport();
            (viewport.focused == Some(true), viewport.minimized == Some(true))
        });
        let gap = self
            .previous_frame
            .replace(started)
            .map(|previous| started.duration_since(previous))
            .unwrap_or_default();
        let width = ui.max_rect().width();
        let height = ui.max_rect().height();

        self.inner.ui(ui, frame);
        if !self.close_requested && self.started.elapsed() >= self.close_after {
            self.close_requested = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        self.samples.push(FrameSample {
            utc_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            elapsed_us: self.started.elapsed().as_micros(),
            frame_gap_us: gap.as_micros(),
            ui_us: started.elapsed().as_micros(),
            focused,
            minimized,
            scheduled_repaint: !minimized,
            width,
            height,
            repaint_requests: self.repaint_requests.load(std::sync::atomic::Ordering::Relaxed),
            bridge_repaint_requests: self.bridge_repaint_requests.load(std::sync::atomic::Ordering::Relaxed),
        });
    }
}

impl Drop for MeasuredGui {
    fn drop(&mut self) {
        let result = (|| -> std::io::Result<()> {
            use std::io::Write;

            let mut file = std::fs::File::create(&self.output)?;
            writeln!(
                file,
                "utc_ms,elapsed_us,frame_gap_us,ui_us,focused,minimized,scheduled_repaint,width,height,repaint_requests,bridge_repaint_requests"
            )?;
            for sample in &self.samples {
                writeln!(
                    file,
                    "{},{},{},{},{},{},{},{:.1},{:.1},{},{}",
                    sample.utc_ms,
                    sample.elapsed_us,
                    sample.frame_gap_us,
                    sample.ui_us,
                    sample.focused,
                    sample.minimized,
                    sample.scheduled_repaint,
                    sample.width,
                    sample.height,
                    sample.repaint_requests,
                    sample.bridge_repaint_requests
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("could not write profiling frame metrics: {error}");
        }
    }
}

fn output_dir() -> PathBuf {
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--output-dir" {
            if let Some(path) = args.next() {
                return PathBuf::from(path);
            }
        }
    }
    eprintln!("usage: host_perf_harness --output-dir <existing directory>");
    std::process::exit(2);
}

fn has_flag(flag: &str) -> bool {
    std::env::args_os().any(|argument| argument == flag)
}

fn close_after() -> Duration {
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--close-after-seconds" {
            if let Some(seconds) = args.next().and_then(|value| value.to_string_lossy().parse::<u64>().ok()) {
                return Duration::from_secs(seconds.max(1));
            }
        }
    }
    Duration::from_secs(4 * 60 * 60)
}

fn main() -> eframe::Result<()> {
    let output_dir = output_dir();
    let animate_theme = has_flag("--animate-theme");
    let close_after = close_after();
    if !output_dir.is_absolute() || !output_dir.is_dir() {
        eprintln!("output directory must already exist and be absolute");
        std::process::exit(2);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("create isolated synthetic runtime");

    let state = Arc::new(AppState {
        scanner: AvatarScanner::new(None),
        monitor_enabled: Arc::new(AtomicBool::new(false)),
        default_config: fixtures::demo_config(),
        device_slots: Arc::new(RwLock::new(Vec::new())),
        alarm: AlarmController::new(AlarmConfig::default()),
    });
    let logs: LogBuffer = Arc::new(Mutex::new(VecDeque::with_capacity(256)));
    let worker_slot = Arc::new(Mutex::new(None));
    let bridge_slot = Arc::new(Mutex::new(None));
    let worker_slot_in_creator = Arc::clone(&worker_slot);
    let bridge_slot_in_creator = Arc::clone(&bridge_slot);
    let frame_metrics_path = output_dir.join("frames.csv");
    let worker_metrics_path = output_dir.join("worker.csv");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([900.0, 620.0])
            .with_title("ShockingVRC Host Performance Harness")
            .with_app_id(format!(
                "shockingvrc-host-perf-harness-{}",
                std::process::id()
            )),
        ..Default::default()
    };

    let result = {
        let _enter = runtime.enter();
        eframe::run_native(
            "ShockingVRC Host Performance Harness",
            native_options,
            Box::new(move |creation| {
                let repaint_context = creation.egui_ctx.clone();
                let bridge_repaint_requests = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let bridge_repaint_requests_for_worker = Arc::clone(&bridge_repaint_requests);
                let (bridge, worker) = bridge::spawn_for_perf_harness(
                    state,
                    move || {
                        bridge_repaint_requests_for_worker.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        repaint_context.request_repaint();
                    },
                    worker_metrics_path,
                );
                fixtures::fill_shared(&bridge.shared);

                let backend = BridgeRemoteBackend::new(bridge.clone(), Arc::clone(&logs), None);
                let remote = HostRemoteController::new(backend, RemoteAccessSettings::default());
                let repaint_requests = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let repaint_requests_for_app = Arc::clone(&repaint_requests);
                let mut app = GuiApp::new(creation, bridge.clone(), logs, remote);
                app.set_perf_repaint_counter(repaint_requests_for_app);
                if animate_theme {
                    app.start_perf_theme_transition();
                }

                *worker_slot_in_creator.lock().expect("worker slot") = Some(worker);
                *bridge_slot_in_creator.lock().expect("bridge slot") = Some(bridge);
                Ok(Box::new(MeasuredGui::new(
                    app,
                    frame_metrics_path,
                    repaint_requests,
                    bridge_repaint_requests,
                    close_after,
                )))
            }),
        )
    };

    if let Some(bridge) = bridge_slot.lock().expect("bridge slot").take() {
        bridge.stop_performance_harness();
    }
    if let Some(worker) = worker_slot.lock().expect("worker slot").take() {
        runtime.block_on(worker).expect("synthetic Host Worker exits cleanly");
    }
    result
}
