/// NEXRAD data layer.
///
/// Data source: Iowa Environmental Mesonet (IEM) NEXRAD composite PNGs.
/// Frames are 5-minute CONUS base-reflectivity composites.
///
/// URL pattern:
///   https://mesonet.agron.iastate.edu/archive/data/{YYYY}/{MM}/{DD}/
///           GIS/uscomp/n0r_{YYYYMMDDHHmm}.png
///
/// HTTP is handled synchronously via `ureq`; requests run on worker threads
/// in our `Executor` thread pool.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Instant;
use std::io::Read;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, DurationRound, Utc};

use super::GeoImage;
use crate::executor::Executor;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const NEXRAD_CONUS_BOUNDS: super::GeoBounds = super::GeoBounds {
    min_lon: -126.0,
    min_lat:   23.0,
    max_lon:  -66.0,
    max_lat:   50.0,
};

const FRAME_INTERVAL_MINUTES: i64 = 5;

// ── Events ────────────────────────────────────────────────────────────────────

pub enum NexradEvent {
    FrameReady  { timestamp: DateTime<Utc>, image: GeoImage },
    FrameError  { timestamp: DateTime<Utc>, error: String },
    FetchComplete,
}

// ── Time range ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end:   DateTime<Utc>,
}

impl TimeRange {
    pub fn frames(&self) -> Vec<DateTime<Utc>> {
        let interval = Duration::minutes(FRAME_INTERVAL_MINUTES);
        let start    = self.start.duration_trunc(interval).unwrap_or(self.start);
        let mut ts   = start;
        let mut out  = Vec::new();
        while ts <= self.end {
            out.push(ts);
            ts = ts + interval;
        }
        out
    }
}

// ── Client ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NexradClient {
    inner: Arc<NexradClientInner>,
}

struct NexradClientInner {
    agent:      ureq::Agent,
    tx:         mpsc::SyncSender<crate::app::AppEvent>,
    egui_ctx:   egui::Context,
    executor:   Arc<Executor>,
    fetching:   AtomicBool,
    last_fetch: Mutex<Option<Instant>>,
}

impl NexradClient {
    pub fn new(
        tx:       mpsc::SyncSender<crate::app::AppEvent>,
        egui_ctx: egui::Context,
    ) -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent("satellite-viewer/0.1 (github.com/madjayq/crackhead-tools)")
            .timeout(std::time::Duration::from_secs(30))
            .build();

        // Each NexradClient gets a private single-thread worker so NEXRAD
        // fetches never starve OSM tile requests.
        let executor = Executor::new(1);

        Self {
            inner: Arc::new(NexradClientInner {
                agent,
                tx,
                egui_ctx,
                executor,
                fetching:   AtomicBool::new(false),
                last_fetch: Mutex::new(None),
            }),
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.inner.fetching.load(Ordering::Relaxed)
    }

    pub fn needs_refresh(&self) -> bool {
        if self.inner.fetching.load(Ordering::Relaxed) { return false; }
        let guard = self.inner.last_fetch.lock().unwrap();
        match *guard {
            None    => true,
            Some(t) => t.elapsed().as_secs() >= 60,
        }
    }

    /// Spawn a thread-pool job that fetches all frames in `range`.
    pub fn fetch_frames(&self, range: TimeRange) {
        let inner = Arc::clone(&self.inner);
        inner.fetching.store(true, Ordering::Relaxed);

        self.inner.executor.spawn(move || {
            let timestamps = range.frames();
            log::info!(
                "NEXRAD: fetching {} frames ({} → {})",
                timestamps.len(), range.start, range.end,
            );

            for ts in timestamps {
                match fetch_frame(&inner.agent, ts) {
                    Ok(image) => {
                        let _ = inner.tx.send(crate::app::AppEvent::Nexrad(
                            NexradEvent::FrameReady { timestamp: ts, image },
                        ));
                    }
                    Err(e) => {
                        log::debug!("NEXRAD frame {ts}: {e}");
                        let _ = inner.tx.send(crate::app::AppEvent::Nexrad(
                            NexradEvent::FrameError { timestamp: ts, error: e.to_string() },
                        ));
                    }
                }
                // Be polite to the server.
                std::thread::sleep(std::time::Duration::from_millis(50));
                inner.egui_ctx.request_repaint();
            }

            let _ = inner.tx.send(crate::app::AppEvent::Nexrad(NexradEvent::FetchComplete));
            inner.fetching.store(false, Ordering::Relaxed);
            *inner.last_fetch.lock().unwrap() = Some(Instant::now());
            inner.egui_ctx.request_repaint();
        });
    }
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

fn fetch_frame(agent: &ureq::Agent, ts: DateTime<Utc>) -> Result<GeoImage> {
    let url = iem_url(ts);
    log::trace!("GET {url}");

    let resp = agent.get(&url).call().map_err(|e| match e {
        ureq::Error::Status(code, _)  => anyhow::anyhow!("HTTP {code}"),
        ureq::Error::Transport(t)     => anyhow::anyhow!("transport: {t}"),
    })?;

    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .context("reading response body")?;

    let img = image::load_from_memory(&bytes).context("decoding PNG")?;
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(GeoImage {
        bounds: NEXRAD_CONUS_BOUNDS,
        pixels: rgba.into_raw(),
        width,
        height,
    })
}

fn iem_url(ts: DateTime<Utc>) -> String {
    format!(
        "https://mesonet.agron.iastate.edu/archive/data/{}/{:02}/{:02}/\
         GIS/uscomp/n0r_{}{:02}{:02}{:02}{:02}.png",
        ts.format("%Y"),
        ts.format("%m").to_string().parse::<u32>().unwrap_or(1),
        ts.format("%d").to_string().parse::<u32>().unwrap_or(1),
        ts.format("%Y"),
        ts.format("%m").to_string().parse::<u32>().unwrap_or(1),
        ts.format("%d").to_string().parse::<u32>().unwrap_or(1),
        ts.format("%H").to_string().parse::<u32>().unwrap_or(0),
        ts.format("%M").to_string().parse::<u32>().unwrap_or(0),
    )
}

// ── Frame store ───────────────────────────────────────────────────────────────

pub struct RadarFrameStore {
    pub frames:        Vec<RadarFrame>,
    pub current_index: usize,
}

pub struct RadarFrame {
    pub timestamp: DateTime<Utc>,
    pub image:     GeoImage,
}

impl Default for RadarFrameStore {
    fn default() -> Self {
        Self { frames: Vec::new(), current_index: 0 }
    }
}

impl RadarFrameStore {
    pub fn insert(&mut self, timestamp: DateTime<Utc>, image: GeoImage) {
        if let Some(pos) = self.frames.iter().position(|f| f.timestamp == timestamp) {
            self.frames[pos].image = image;
        } else {
            let pos = self.frames.partition_point(|f| f.timestamp < timestamp);
            self.frames.insert(pos, RadarFrame { timestamp, image });
        }
    }

    pub fn len(&self)      -> usize  { self.frames.len() }
    pub fn is_empty(&self) -> bool   { self.frames.is_empty() }

    pub fn current(&self) -> Option<&RadarFrame> {
        self.frames.get(self.current_index)
    }
}
