/// NEXRAD data layer.
///
/// Data source: Iowa Environmental Mesonet (IEM) NEXRAD composite PNGs.
/// These are CONUS-wide base-reflectivity composites, produced every 5 minutes.
///
/// URL pattern:
///   https://mesonet.agron.iastate.edu/archive/data/{YYYY}/{MM}/{DD}/
///           GIS/uscomp/n0r_{YYYYMMDDHHmm}.png
///
/// The PNG covers roughly (-126 … -66, 23 … 50) in WGS-84.
/// A companion .wld world-file gives the exact transform; we use the
/// well-known constants for the n0r product published by IEM.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, DurationRound, Utc};
use super::GeoImage;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Known geographic extent of IEM's n0r CONUS composite.
pub const NEXRAD_CONUS_BOUNDS: super::GeoBounds = super::GeoBounds {
    min_lon: -126.0,
    min_lat: 23.0,
    max_lon: -66.0,
    max_lat: 50.0,
};

/// Composite product resolution — 5-minute frames.
const FRAME_INTERVAL_MINUTES: i64 = 5;

// ── Events ────────────────────────────────────────────────────────────────────

/// Messages sent from the background fetch task to the UI thread.
pub enum NexradEvent {
    /// A new frame was fetched successfully.
    FrameReady {
        timestamp: DateTime<Utc>,
        image: GeoImage,
    },
    /// The fetch for a specific timestamp failed (non-fatal).
    FrameError {
        timestamp: DateTime<Utc>,
        error: String,
    },
    /// All requested frames have been fetched (or attempted).
    FetchComplete,
}

// ── Time range ────────────────────────────────────────────────────────────────

/// The time window the user wants to load as a radar loop.
#[derive(Debug, Clone)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeRange {
    /// Returns an iterator over every 5-minute timestamp in [start, end].
    pub fn frames(&self) -> Vec<DateTime<Utc>> {
        let interval = Duration::minutes(FRAME_INTERVAL_MINUTES);
        // Snap start to the nearest 5-minute boundary.
        let start = self
            .start
            .duration_trunc(interval)
            .unwrap_or(self.start);
        let mut ts = start;
        let mut frames = Vec::new();
        while ts <= self.end {
            frames.push(ts);
            ts = ts + interval;
        }
        frames
    }
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Shareable, cloneable handle to the NEXRAD fetch client.
/// The inner `Arc` means cloning is cheap — all clones share the same state.
#[derive(Clone)]
pub struct NexradClient {
    inner: Arc<NexradClientInner>,
}

struct NexradClientInner {
    http: reqwest::Client,
    tx: mpsc::SyncSender<crate::app::AppEvent>,
    egui_ctx: egui::Context,
    fetching: AtomicBool,
    /// Wall-clock time of the last successful fetch — used to rate-limit
    /// redundant network requests.
    last_fetch: std::sync::Mutex<Option<Instant>>,
}

impl NexradClient {
    pub fn new(
        tx: mpsc::SyncSender<crate::app::AppEvent>,
        egui_ctx: egui::Context,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("satellite-viewer/0.1 (github.com/madjayq/crackhead-tools)")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            inner: Arc::new(NexradClientInner {
                http,
                tx,
                egui_ctx,
                fetching: AtomicBool::new(false),
                last_fetch: std::sync::Mutex::new(None),
            }),
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.inner.fetching.load(Ordering::Relaxed)
    }

    /// Returns true when it's time to re-fetch (respects a 60-second cooldown).
    pub fn needs_refresh(&self) -> bool {
        if self.inner.fetching.load(Ordering::Relaxed) {
            return false;
        }
        let guard = self.inner.last_fetch.lock().unwrap();
        match *guard {
            None => true,
            Some(t) => t.elapsed().as_secs() >= 60,
        }
    }

    /// Fetch all radar frames in `range`.  Each frame is sent back to the UI
    /// thread as it arrives via the `AppEvent` channel.
    pub async fn fetch_frames(&self, range: TimeRange) -> Result<()> {
        self.inner.fetching.store(true, Ordering::Relaxed);

        let timestamps = range.frames();
        log::info!(
            "NEXRAD: fetching {} frames ({} → {})",
            timestamps.len(),
            range.start,
            range.end,
        );

        for ts in timestamps {
            match self.fetch_single_frame(ts).await {
                Ok(image) => {
                    let _ = self.inner.tx.send(crate::app::AppEvent::Nexrad(
                        NexradEvent::FrameReady { timestamp: ts, image },
                    ));
                }
                Err(e) => {
                    log::debug!("NEXRAD frame {ts}: {e}");
                    let _ = self.inner.tx.send(crate::app::AppEvent::Nexrad(
                        NexradEvent::FrameError {
                            timestamp: ts,
                            error: e.to_string(),
                        },
                    ));
                }
            }
            // Polite to the server — 50 ms between requests.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let _ = self
            .inner
            .tx
            .send(crate::app::AppEvent::Nexrad(NexradEvent::FetchComplete));
        self.inner.fetching.store(false, Ordering::Relaxed);
        *self.inner.last_fetch.lock().unwrap() = Some(Instant::now());
        self.inner.egui_ctx.request_repaint();
        Ok(())
    }

    async fn fetch_single_frame(&self, ts: DateTime<Utc>) -> Result<GeoImage> {
        let url = iem_composite_url(ts);
        log::trace!("GET {url}");

        let response = self
            .inner
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("HTTP GET {url}"))?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("frame not yet available (404)");
        }

        let status = response.status();
        anyhow::ensure!(status.is_success(), "HTTP {status} for {url}");

        let bytes = response.bytes().await.context("reading response body")?;
        decode_png_to_geo_image(&bytes)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn iem_composite_url(ts: DateTime<Utc>) -> String {
    format!(
        "https://mesonet.agron.iastate.edu/archive/data/{year}/{month:02}/{day:02}/\
         GIS/uscomp/n0r_{year}{month:02}{day:02}{hour:02}{minute:02}.png",
        year   = ts.format("%Y"),
        month  = ts.format("%m"),
        day    = ts.format("%d"),
        hour   = ts.format("%H"),
        minute = ts.format("%M"),
    )
}

fn decode_png_to_geo_image(bytes: &[u8]) -> Result<GeoImage> {
    let img = image::load_from_memory(bytes).context("decoding PNG")?;
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(GeoImage {
        bounds: NEXRAD_CONUS_BOUNDS,
        pixels: rgba.into_raw(),
        width,
        height,
    })
}

// ── Radar frame ring-buffer (held by the map layer) ───────────────────────────

/// Stores all fetched radar frames and tracks which one is "current".
pub struct RadarFrameStore {
    /// Frames sorted oldest → newest.
    pub frames: Vec<RadarFrame>,
    pub current_index: usize,
}

pub struct RadarFrame {
    pub timestamp: DateTime<Utc>,
    pub image: GeoImage,
}

impl Default for RadarFrameStore {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            current_index: 0,
        }
    }
}

impl RadarFrameStore {
    pub fn insert(&mut self, timestamp: DateTime<Utc>, image: GeoImage) {
        // Keep sorted; replace if we already have this timestamp.
        if let Some(pos) = self.frames.iter().position(|f| f.timestamp == timestamp) {
            self.frames[pos].image = image;
        } else {
            let pos = self
                .frames
                .partition_point(|f| f.timestamp < timestamp);
            self.frames.insert(pos, RadarFrame { timestamp, image });
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn current(&self) -> Option<&RadarFrame> {
        self.frames.get(self.current_index)
    }

    pub fn advance(&mut self) -> bool {
        if self.frames.is_empty() {
            return false;
        }
        self.current_index = (self.current_index + 1) % self.frames.len();
        true
    }

    pub fn timestamps(&self) -> Vec<DateTime<Utc>> {
        self.frames.iter().map(|f| f.timestamp).collect()
    }
}
