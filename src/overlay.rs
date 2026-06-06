use crate::audio::dsp::{compress_activity, lerp, normalize_level, smoothing_factor};
use crate::audio::AudioVisualizationSnapshot;
use crate::config::StatusUiConfig;
use crate::state::SessionPhase;
use anyhow::{anyhow, Result};
use std::thread;

use anyhow::Context;
use smithay_client_toolkit::{
    compositor::{CompositorState, Region},
    output::OutputState,
    registry::RegistryState,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface},
        WaylandSurface,
    },
};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wayland_client::{globals::registry_queue_init, Connection};

mod renderer;
mod wayland;

use renderer::{FrameParams, Renderer};

const COMMAND_THREAD_NAME: &str = "ut-status-ui";
const DEFAULT_TICK_MS: u64 = 16;

#[derive(Debug)]
pub struct OverlayHandle {
    enabled: bool,
    sender: Option<mpsc::Sender<OverlayCommand>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl OverlayHandle {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_phase(&self, phase: SessionPhase) -> Result<()> {
        self.send(OverlayCommand::Phase(phase))
    }

    pub fn push_audio_snapshot(&self, snapshot: AudioVisualizationSnapshot) -> Result<()> {
        self.send(OverlayCommand::Audio(snapshot))
    }

    pub fn request_abort(&self) -> Result<()> {
        self.send(OverlayCommand::Abort)
    }

    pub fn request_fade_out(&self) -> Result<()> {
        self.send(OverlayCommand::FadeOut)
    }

    pub fn shutdown(mut self) -> Result<()> {
        if self.enabled {
            if let Some(sender) = self.sender.take() {
                let _ = sender.send(OverlayCommand::Abort);
            }

            if let Some(thread) = self.thread.take() {
                thread
                    .join()
                    .map_err(|_| anyhow!("status UI thread panicked"))?;
            }
        }

        Ok(())
    }

    fn send(&self, command: OverlayCommand) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let sender = self
            .sender
            .as_ref()
            .context("status UI command channel is not available")?;
        sender
            .send(command)
            .map_err(|_| anyhow!("status UI thread already closed"))
    }
}

pub fn spawn(config: StatusUiConfig) -> Result<OverlayHandle> {
    if !config.enabled {
        return Ok(OverlayHandle {
            enabled: false,
            sender: None,
            thread: None,
        });
    }

    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name(COMMAND_THREAD_NAME.to_string())
        .spawn(move || run_overlay_thread(config, ready_tx))
        .context("failed to spawn status UI thread")?;

    match ready_rx
        .recv()
        .context("status UI thread closed before initialization completed")?
    {
        Ok(sender) => Ok(OverlayHandle {
            enabled: true,
            sender: Some(sender),
            thread: Some(thread),
        }),
        Err(err) => {
            let _ = thread.join();
            Err(err)
        }
    }
}

fn run_overlay_thread(
    config: StatusUiConfig,
    ready_tx: mpsc::Sender<Result<mpsc::Sender<OverlayCommand>>>,
) {
    let result = (|| -> Result<()> {
        let conn = Connection::connect_to_env().context("failed to connect to Wayland")?;
        let (globals, mut event_queue) =
            registry_queue_init(&conn).context("failed to initialize Wayland registry")?;
        let qh = event_queue.handle();

        let compositor_state =
            CompositorState::bind(&globals, &qh).context("wl_compositor is not available")?;
        let layer_shell =
            LayerShell::bind(&globals, &qh).context("layer shell is not available")?;

        let surface = compositor_state.create_surface(&qh);
        let layer_surface =
            layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some("ut-status-ui"), None);
        layer_surface.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Keep the overlay passive so it does not reserve desktop space.
        layer_surface.set_exclusive_zone(0);
        layer_surface.set_margin(0, 0, 0, 0);
        layer_surface.set_size(0, 0);

        let passive_region =
            Region::new(&compositor_state).context("failed to create passive input region")?;
        layer_surface.set_input_region(Some(passive_region.wl_region()));
        layer_surface.set_opaque_region(Some(passive_region.wl_region()));
        layer_surface.commit();

        let registry_state = RegistryState::new(&globals);
        let output_state = OutputState::new(&globals, &qh);
        let graphics = Renderer::new(&conn, &layer_surface, config.width, config.height)
            .context("failed to initialize status UI renderer")?;

        let (command_tx, command_rx) = mpsc::channel();
        ready_tx
            .send(Ok(command_tx))
            .context("failed to send status UI handle to caller")?;

        let mut app = OverlayApp::new(
            config,
            registry_state,
            output_state,
            layer_surface,
            passive_region,
            graphics,
            command_rx,
        );
        app.run(&conn, &mut event_queue)
    })();

    if let Err(err) = result {
        let _ = ready_tx.send(Err(err));
    }
}

#[derive(Debug)]
pub(super) enum OverlayCommand {
    Phase(SessionPhase),
    Audio(AudioVisualizationSnapshot),
    Abort,
    FadeOut,
}

#[derive(Debug)]
pub(super) struct OverlayApp {
    pub(super) registry_state: RegistryState,
    pub(super) output_state: OutputState,
    pub(super) config: StatusUiConfig,
    #[allow(dead_code)]
    pub(super) layer_surface: LayerSurface,
    #[allow(dead_code)]
    pub(super) passive_region: Region,
    pub(super) graphics: Renderer,
    pub(super) command_rx: mpsc::Receiver<OverlayCommand>,
    pub(super) phase: SessionPhase,
    pub(super) latest_audio: Option<AudioVisualizationSnapshot>,
    pub(super) displayed_audio: Option<AudioVisualizationSnapshot>,
    pub(super) fade_requested: bool,
    pub(super) fade_started_at: Option<Instant>,
    pub(super) visible_started_at: Option<Instant>,
    pub(super) processing_started_at: Option<Instant>,
    pub(super) started_at: Instant,
    pub(super) fbm_rotation_phase: f32,
    pub(super) fbm_translation_phase: f32,
    pub(super) abort_requested: bool,
    pub(super) first_configure: bool,
    pub(super) position_committed: bool,
    pub(super) last_frame_at: Instant,
}

impl OverlayApp {
    fn new(
        config: StatusUiConfig,
        registry_state: RegistryState,
        output_state: OutputState,
        layer_surface: LayerSurface,
        passive_region: Region,
        graphics: Renderer,
        command_rx: mpsc::Receiver<OverlayCommand>,
    ) -> Self {
        Self {
            registry_state,
            output_state,
            config,
            layer_surface,
            passive_region,
            graphics,
            command_rx,
            phase: SessionPhase::Idle,
            latest_audio: None,
            displayed_audio: None,
            fade_requested: false,
            fade_started_at: None,
            visible_started_at: None,
            processing_started_at: None,
            started_at: Instant::now(),
            fbm_rotation_phase: 0.0,
            fbm_translation_phase: 0.0,
            abort_requested: false,
            first_configure: true,
            position_committed: false,
            last_frame_at: Instant::now(),
        }
    }

    fn position_surface(&self, available_width: u32, available_height: u32) {
        let width = self.config.width.max(1);
        let height = self.config.height.max(1);
        let max_left = available_width.saturating_sub(width) as f32;
        let max_top = available_height.saturating_sub(height) as f32;
        let left =
            (available_width as f32 * self.config.x - width as f32 * 0.5).clamp(0.0, max_left);
        let top =
            (available_height as f32 * self.config.y - height as f32 * 0.5).clamp(0.0, max_top);

        self.layer_surface.set_anchor(Anchor::TOP | Anchor::LEFT);
        self.layer_surface.set_size(width, height);
        self.layer_surface
            .set_margin(top.round() as i32, 0, 0, left.round() as i32);
        self.layer_surface.commit();
    }

    fn run(
        &mut self,
        conn: &Connection,
        event_queue: &mut wayland_client::EventQueue<Self>,
    ) -> Result<()> {
        loop {
            while let Ok(command) = self.command_rx.try_recv() {
                self.handle_command(command);
            }

            event_queue
                .dispatch_pending(self)
                .context("failed to dispatch status UI events")?;

            if self.abort_requested {
                break;
            }

            let now = Instant::now();
            let dt = now.saturating_duration_since(self.last_frame_at);
            self.last_frame_at = now;
            self.maybe_start_fade();
            self.update_displayed_audio(dt);

            if self.close_if_fade_completed(now) {
                break;
            }

            if !self.first_configure {
                self.graphics.render(FrameParams {
                    processing_elapsed: self.shader_phase_value(now),
                    audio: self.displayed_audio.as_ref(),
                    fade_alpha: self.fade_alpha(now),
                    fbm_rotation_phase: self.fbm_rotation_phase,
                    fbm_translation_phase: self.fbm_translation_phase,
                    elapsed: now.saturating_duration_since(self.started_at),
                })?;
            }

            conn.flush().context("failed to flush Wayland connection")?;
            thread::sleep(Duration::from_millis(DEFAULT_TICK_MS));
        }

        Ok(())
    }

    fn handle_command(&mut self, command: OverlayCommand) {
        match command {
            OverlayCommand::Phase(phase) => {
                let entering_visible =
                    phase != SessionPhase::Idle && self.phase == SessionPhase::Idle;
                let entering_recording =
                    phase == SessionPhase::Recording && self.phase != SessionPhase::Recording;
                let entering_processing_like =
                    matches!(phase, SessionPhase::Processing | SessionPhase::Pasting)
                        && !matches!(self.phase, SessionPhase::Processing | SessionPhase::Pasting);
                self.phase = phase;
                if entering_visible {
                    self.visible_started_at = Some(Instant::now());
                    self.displayed_audio = Some(zero_audio_snapshot());
                }
                if entering_recording {
                    self.processing_started_at = None;
                    self.fbm_rotation_phase = 0.0;
                    self.fbm_translation_phase = 0.0;
                }
                if entering_processing_like {
                    self.processing_started_at = Some(Instant::now());
                }
                if phase != SessionPhase::Idle {
                    self.fade_started_at = None;
                }
                self.maybe_start_fade();
            }
            OverlayCommand::Audio(snapshot) => {
                self.latest_audio = Some(snapshot);
            }
            OverlayCommand::Abort => {
                self.abort_requested = true;
            }
            OverlayCommand::FadeOut => {
                self.fade_requested = true;
                self.maybe_start_fade();
            }
        }
    }

    fn maybe_start_fade(&mut self) {
        if self.phase == SessionPhase::Idle && self.fade_requested && self.fade_started_at.is_none()
        {
            self.fade_started_at = Some(Instant::now());
        }
    }

    fn update_displayed_audio(&mut self, dt: Duration) {
        let mut target = self
            .latest_audio
            .clone()
            .unwrap_or_else(zero_audio_snapshot);
        let processing_like =
            matches!(self.phase, SessionPhase::Processing | SessionPhase::Pasting)
                || (self.fade_started_at.is_some() && self.phase == SessionPhase::Idle);

        if self.phase == SessionPhase::Recording {
            target = normalize_recording_snapshot(target);
            // Swirl is driven by the voice pulse (level) and onsets (transient).
            // Scaling with the square of the pulse means quiet speech barely
            // swirls and it builds smoothly; transient influence is kept small
            // so onsets don't cause jerky churn.
            let swirl = target.level * target.level;
            // Keep a barely-moving baseline drift when VAD is closed so the
            // texture does not freeze completely between phrases.
            let idle_rotation_rate = 0.010;
            let idle_translation_rate = 0.008;
            let rotation_rate = idle_rotation_rate + swirl * 0.34 + target.transient * 0.05;
            let translation_rate =
                idle_translation_rate + swirl * 0.30 + target.transient * 0.08;
            self.fbm_rotation_phase += dt.as_secs_f32() * rotation_rate;
            self.fbm_translation_phase += dt.as_secs_f32() * translation_rate;
        } else if processing_like {
            target = processing_audio_snapshot();
            self.fbm_rotation_phase = 0.0;
            self.fbm_translation_phase = 0.0;
        } else {
            target = normalize_legacy_snapshot(target);
        }

        let current = self.displayed_audio.get_or_insert_with(|| target.clone());

        let level_attack = smoothing_factor(dt, 24.0);
        let level_release = smoothing_factor(dt, 16.0);
        let transient_attack = smoothing_factor(dt, 28.0);
        let transient_release = smoothing_factor(dt, 18.0);

        current.frame_index = target.frame_index;
        current.sample_rate = target.sample_rate;
        current.peak = approach(
            current.peak,
            target.peak,
            transient_attack,
            transient_release,
        );
        current.level = approach(current.level, target.level, level_attack, level_release);
        current.transient = approach(
            current.transient,
            target.transient,
            transient_attack,
            transient_release,
        );
    }

    fn fade_alpha(&self, now: Instant) -> f32 {
        if self.phase == SessionPhase::Idle && self.fade_started_at.is_none() {
            return 0.0;
        }

        let duration = Duration::from_millis(self.config.fade_out_ms);
        let visible_now = self.phase != SessionPhase::Idle || self.fade_started_at.is_some();
        let fade_in_alpha = if visible_now {
            if let Some(started_at) = self.visible_started_at {
                if duration.is_zero() {
                    1.0
                } else {
                    let elapsed = now.saturating_duration_since(started_at);
                    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
                }
            } else {
                1.0
            }
        } else {
            1.0
        };

        let fade_out_alpha = if let Some(started_at) = self.fade_started_at {
            if duration.is_zero() {
                0.0
            } else {
                let elapsed = now.saturating_duration_since(started_at);
                let ratio = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
                1.0 - ratio
            }
        } else {
            1.0
        };

        fade_in_alpha * fade_out_alpha
    }

    fn shader_phase_value(&self, now: Instant) -> f32 {
        let processing_like =
            matches!(self.phase, SessionPhase::Processing | SessionPhase::Pasting)
                || (self.fade_started_at.is_some() && self.phase == SessionPhase::Idle);
        if !processing_like {
            return 0.0;
        }

        let Some(started_at) = self.processing_started_at else {
            return 1e-4;
        };

        now.saturating_duration_since(started_at)
            .as_secs_f32()
            .max(1e-4)
    }

    fn close_if_fade_completed(&self, now: Instant) -> bool {
        if let Some(started_at) = self.fade_started_at {
            let duration = Duration::from_millis(self.config.fade_out_ms);
            return duration.is_zero() || now.saturating_duration_since(started_at) >= duration;
        }

        false
    }
}

fn zero_audio_snapshot() -> AudioVisualizationSnapshot {
    AudioVisualizationSnapshot::default()
}

fn approach(current: f32, target: f32, attack: f32, release: f32) -> f32 {
    if target > current {
        lerp(current, target, attack)
    } else {
        lerp(current, target, release)
    }
}

fn normalize_legacy_snapshot(
    mut snapshot: AudioVisualizationSnapshot,
) -> AudioVisualizationSnapshot {
    // Idle-before-fade ambient: a gentle pulse from the lightly-normalized
    // envelope. No voice gate here — this state is barely visible.
    let level = normalize_level(snapshot.level, 0.05, 0.80);
    snapshot.level = level;
    snapshot.peak = normalize_level(snapshot.peak, 0.020, 0.300);
    snapshot.transient = snapshot.peak;
    snapshot
}

fn normalize_recording_snapshot(
    mut snapshot: AudioVisualizationSnapshot,
) -> AudioVisualizationSnapshot {
    let base_level =
        (0.10 + 0.90 * compress_activity(snapshot.level.max(0.0), 4.0)).clamp(0.0, 1.0);
    let base_transient = compress_activity(snapshot.transient.max(0.0), 14.0)
        .powf(0.85)
        .clamp(0.0, 1.0);

    // Voice-activity detection (RNNoise) decides whether this is speech; the
    // loudness envelope sets how far the blob swells. A single voice pulse
    // drives everything — `level` is the value the shader reads as `audio.x`.
    let gate = snapshot.voice_probability.clamp(0.0, 1.0);
    let pulse = (gate * base_level).clamp(0.0, 1.0);

    snapshot.level = pulse;
    snapshot.transient = (base_transient * gate).clamp(0.0, 1.0);
    snapshot.peak = (snapshot.transient.max(pulse * 0.72)).clamp(0.0, 1.0);
    snapshot
}

fn processing_audio_snapshot() -> AudioVisualizationSnapshot {
    AudioVisualizationSnapshot::default()
}
