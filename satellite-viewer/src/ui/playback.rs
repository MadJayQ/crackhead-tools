/// NEXRAD radar loop playback controls (bottom panel).

use chrono::{Duration, Utc};

use crate::app::RadarGpuLayer;
use crate::data::nexrad::TimeRange;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackSpeed {
    Fps(f32),
}

impl PlaybackSpeed {
    const PRESETS: &'static [(f32, &'static str)] = &[
        (0.5, "½×"), (1.0, "1×"), (2.0, "2×"), (5.0, "5×"), (10.0, "10×"),
    ];

    fn fps(self) -> f32 {
        match self { PlaybackSpeed::Fps(f) => f }
    }

    fn frame_duration(self) -> std::time::Duration {
        std::time::Duration::from_secs_f32(1.0 / self.fps())
    }
}

pub struct PlaybackPanel {
    pub playing:      bool,
    pub loop_enabled: bool,
    pub speed:        PlaybackSpeed,
    pub hours_back:   u32,
    last_advance:     std::time::Instant,
}

impl Default for PlaybackPanel {
    fn default() -> Self {
        Self {
            playing:      false,
            loop_enabled: true,
            speed:        PlaybackSpeed::Fps(5.0),
            hours_back:   2,
            last_advance: std::time::Instant::now(),
        }
    }
}

impl PlaybackPanel {
    pub fn time_range(&self) -> TimeRange {
        let end   = Utc::now();
        let start = end - Duration::hours(self.hours_back as i64);
        TimeRange { start, end }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, radar: &mut RadarGpuLayer) {
        let frame_count = radar.store.len();

        ui.add_space(6.0);

        // ── Transport row ──────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "⏸" } else { "▶" })
                .on_hover_text("Play / Pause")
                .clicked()
            {
                self.playing = !self.playing;
                self.last_advance = std::time::Instant::now();
            }

            if ui.add_enabled(frame_count > 0, egui::Button::new("⏮"))
                .on_hover_text("Previous frame")
                .clicked()
                && frame_count > 0
            {
                radar.store.current_index =
                    (radar.store.current_index + frame_count - 1) % frame_count;
            }

            if ui.add_enabled(frame_count > 0, egui::Button::new("⏭"))
                .on_hover_text("Next frame")
                .clicked()
                && frame_count > 0
            {
                radar.store.current_index =
                    (radar.store.current_index + 1) % frame_count;
            }

            ui.separator();
            ui.toggle_value(&mut self.loop_enabled, "🔁").on_hover_text("Loop");
            ui.separator();

            for (fps, label) in PlaybackSpeed::PRESETS {
                if ui.selectable_label((self.speed.fps() - fps).abs() < 0.01, *label)
                    .clicked()
                {
                    self.speed = PlaybackSpeed::Fps(*fps);
                }
            }

            ui.separator();

            if let Some(frame) = radar.store.current() {
                ui.monospace(frame.timestamp.format("%Y-%m-%d %H:%M UTC").to_string());
            } else if frame_count == 0 {
                ui.label(egui::RichText::new("No frames loaded").italics());
            }

            if frame_count > 0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{}/{}", radar.store.current_index + 1, frame_count));
                });
            }
        });

        // ── Scrubber ───────────────────────────────────────────────────────────
        if frame_count > 1 {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("🕐");
                let mut idx = radar.store.current_index;
                if ui.add(
                    egui::Slider::new(&mut idx, 0..=frame_count - 1)
                        .show_value(false)
                        .clamping(egui::SliderClamping::Always),
                ).changed() {
                    self.playing = false;
                    radar.store.current_index = idx;
                }
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

        // ── Auto-advance ───────────────────────────────────────────────────────
        if self.playing && frame_count > 1 {
            if self.last_advance.elapsed() >= self.speed.frame_duration() {
                self.last_advance = std::time::Instant::now();
                let next = (radar.store.current_index + 1) % frame_count;
                if next == 0 && !self.loop_enabled {
                    self.playing = false;
                } else {
                    radar.store.current_index = next;
                }
            }
            ui.ctx().request_repaint();
        }
    }
}
