/// NEXRAD radar loop playback controls.
///
/// The panel renders at the bottom of the window and provides:
///   • Play / Pause button
///   • Frame scrubber (timeline slider)
///   • Speed selector
///   • Loop toggle
///   • Time-range picker (how many hours back to load)
///   • Current-frame timestamp display

use chrono::{Duration, Utc};

use crate::data::nexrad::TimeRange;
use crate::ui::map_view::RadarLayer;

// ── Playback speed ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackSpeed {
    /// Frames per second.
    Fps(f32),
}

impl PlaybackSpeed {
    const PRESETS: &'static [(f32, &'static str)] = &[
        (1.0, "1×"),
        (2.0, "2×"),
        (5.0, "5×"),
        (10.0, "10×"),
        (0.5, "½×"),
    ];

    fn fps(self) -> f32 {
        match self {
            PlaybackSpeed::Fps(f) => f,
        }
    }

    fn frame_duration(self) -> std::time::Duration {
        std::time::Duration::from_secs_f32(1.0 / self.fps())
    }
}

// ── PlaybackPanel ─────────────────────────────────────────────────────────────

pub struct PlaybackPanel {
    pub playing: bool,
    pub loop_enabled: bool,
    pub speed: PlaybackSpeed,
    /// How many hours back from "now" to load.
    pub hours_back: u32,
    last_advance: std::time::Instant,
}

impl Default for PlaybackPanel {
    fn default() -> Self {
        Self {
            playing: false,
            loop_enabled: true,
            speed: PlaybackSpeed::Fps(5.0),
            hours_back: 2,
            last_advance: std::time::Instant::now(),
        }
    }
}

impl PlaybackPanel {
    /// Returns the time range the user has configured.
    pub fn time_range(&self) -> TimeRange {
        let end = Utc::now();
        let start = end - Duration::hours(self.hours_back as i64);
        TimeRange { start, end }
    }

    /// Keep the `PlaybackPanel`'s frame count consistent with what's loaded.
    pub fn sync_from_radar(&mut self, _radar: &RadarLayer) {
        // No persistent frame count here — the panel queries the layer directly.
    }

    /// Advance playback if enough time has elapsed.
    ///
    /// Returns `Some(new_frame_index)` when the frame should change.
    pub fn tick(&mut self) -> Option<usize> {
        None // Actual advancing happens inside `show` (needs layer reference).
    }

    /// Draw the playback panel and mutate `radar`.
    pub fn show(&mut self, ui: &mut egui::Ui, radar: &mut RadarLayer) {
        let frame_count = radar.store.len();

        ui.add_space(6.0);

        // ── Transport controls row ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            // ❙❙  / ▶
            let play_label = if self.playing { "⏸" } else { "▶" };
            if ui.button(play_label).on_hover_text("Play / Pause").clicked() {
                self.playing = !self.playing;
                self.last_advance = std::time::Instant::now();
            }

            // ⏮  (step back)
            if ui
                .add_enabled(frame_count > 0, egui::Button::new("⏮"))
                .on_hover_text("Previous frame")
                .clicked()
            {
                let n = radar.store.len();
                if n > 0 {
                    radar.store.current_index =
                        (radar.store.current_index + n - 1) % n;
                }
            }

            // ⏭  (step forward)
            if ui
                .add_enabled(frame_count > 0, egui::Button::new("⏭"))
                .on_hover_text("Next frame")
                .clicked()
                && frame_count > 0
            {
                radar.store.current_index =
                    (radar.store.current_index + 1) % frame_count;
            }

            ui.separator();

            // Loop toggle.
            ui.toggle_value(&mut self.loop_enabled, "🔁")
                .on_hover_text("Loop");

            ui.separator();

            // Speed presets.
            for (fps, label) in PlaybackSpeed::PRESETS {
                let is_current =
                    (self.speed.fps() - fps).abs() < 0.01;
                if ui.selectable_label(is_current, *label).clicked() {
                    self.speed = PlaybackSpeed::Fps(*fps);
                }
            }

            ui.separator();

            // Current frame timestamp.
            if let Some(ts) = radar.current_timestamp() {
                ui.monospace(ts.format("%Y-%m-%d %H:%M UTC").to_string());
            } else if frame_count == 0 {
                ui.label(egui::RichText::new("No frames loaded").italics());
            }

            // Frame counter.
            if frame_count > 0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{}/{}", radar.store.current_index + 1, frame_count));
                });
            }
        });

        // ── Scrubber ──────────────────────────────────────────────────────────
        if frame_count > 1 {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("🕐");
                let mut idx = radar.store.current_index;
                let changed = ui
                    .add(
                        egui::Slider::new(&mut idx, 0..=frame_count - 1)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always),
                    )
                    .changed();
                if changed {
                    self.playing = false;
                    radar.store.current_index = idx;
                }
                // Show first/last timestamps.
                if let (Some(first), Some(last)) = (
                    radar.frames().first().map(|f| f.timestamp),
                    radar.frames().last().map(|f| f.timestamp),
                ) {
                    ui.monospace(format!(
                        "{} → {}",
                        first.format("%H:%M"),
                        last.format("%H:%M UTC"),
                    ));
                }
            });
        }

        // ── Auto-advance ──────────────────────────────────────────────────────
        if self.playing && frame_count > 1 {
            let elapsed = self.last_advance.elapsed();
            if elapsed >= self.speed.frame_duration() {
                self.last_advance = std::time::Instant::now();
                let next = (radar.store.current_index + 1) % frame_count;
                if next == 0 && !self.loop_enabled {
                    self.playing = false;
                } else {
                    radar.store.current_index = next;
                }
            }
            // Keep repainting while playing.
            ui.ctx().request_repaint();
        }
    }
}
