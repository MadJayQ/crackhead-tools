/// Sentinel-2 L2A data layer.
///
/// Pipeline:
///   1. POST to Element84 STAC API — find least-cloudy scene for the viewport.
///   2. Enumerate the XYZ tiles that cover the viewport at the current zoom.
///   3. GET each tile from the public TiTiler instance (COG → PNG on the fly).
///
/// All HTTP via `ureq` (synchronous); work runs on worker threads supplied by
/// our `Executor` thread pool.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Instant;
use std::io::Read;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{GeoBounds, GeoImage};
use crate::executor::Executor;
use crate::ui::map_view::MapViewport;

const TITILER_BASE: &str = "https://titiler.xyz";

// ── STAC types ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StacSearchResponse { features: Vec<StacFeature> }

#[derive(Debug, Deserialize)]
struct StacFeature {
    id:         String,
    properties: StacProperties,
    bbox:       [f64; 4],
    assets:     StacAssets,
}

#[derive(Debug, Deserialize)]
struct StacProperties {
    #[serde(rename = "eo:cloud_cover")]
    cloud_cover: Option<f64>,
    datetime:    Option<String>,
}

#[derive(Debug, Deserialize)]
struct StacAssets {
    visual: Option<StacAsset>,
    #[serde(rename = "B04")]
    b04: Option<StacAsset>,
}

#[derive(Debug, Deserialize)]
struct StacAsset { href: String }

// ── Events ────────────────────────────────────────────────────────────────────

pub enum SentinelEvent {
    TileReady { z: u8, x: u32, y: u32, image: GeoImage },
    SceneFound {
        scene_id:    String,
        bounds:      GeoBounds,
        cloud_cover: f64,
        datetime:    String,
        cog_url:     String,
    },
    Error(String),
}

// ── Client ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SentinelClient {
    inner: Arc<SentinelClientInner>,
}

struct SentinelClientInner {
    agent:          ureq::Agent,
    tx:             mpsc::SyncSender<crate::app::AppEvent>,
    egui_ctx:       egui::Context,
    executor:       Arc<Executor>,
    fetching:       AtomicBool,
    last_viewport:  Mutex<Option<MapViewport>>,
    last_fetch:     Mutex<Option<Instant>>,
}

impl SentinelClient {
    pub fn new(
        tx:       mpsc::SyncSender<crate::app::AppEvent>,
        egui_ctx: egui::Context,
    ) -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent("satellite-viewer/0.1 (github.com/madjayq/crackhead-tools)")
            .timeout(std::time::Duration::from_secs(60))
            .build();

        let executor = Executor::new(4); // 4 concurrent tile downloads

        Self {
            inner: Arc::new(SentinelClientInner {
                agent,
                tx,
                egui_ctx,
                executor,
                fetching:      AtomicBool::new(false),
                last_viewport: Mutex::new(None),
                last_fetch:    Mutex::new(None),
            }),
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.inner.fetching.load(Ordering::Relaxed)
    }

    pub fn needs_refresh(&self, viewport: &MapViewport) -> bool {
        if self.inner.fetching.load(Ordering::Relaxed) { return false; }
        {
            let guard = self.inner.last_fetch.lock().unwrap();
            if let Some(t) = *guard {
                if t.elapsed().as_secs() < 5 { return false; }
            }
        }
        let guard = self.inner.last_viewport.lock().unwrap();
        match &*guard {
            None       => true,
            Some(prev) => !prev.is_close_to(viewport),
        }
    }

    /// Spawn a worker that searches STAC and then fetches map tiles.
    pub fn fetch_scene(&self, viewport: MapViewport) {
        let inner = Arc::clone(&self.inner);
        inner.fetching.store(true, Ordering::Relaxed);
        *inner.last_fetch.lock().unwrap() = Some(Instant::now());

        self.inner.executor.spawn(move || {
            if let Err(e) = do_fetch(&inner, &viewport) {
                log::warn!("Sentinel fetch: {e}");
                let _ = inner.tx.send(crate::app::AppEvent::Sentinel(
                    SentinelEvent::Error(e.to_string()),
                ));
            }
            inner.fetching.store(false, Ordering::Relaxed);
            *inner.last_viewport.lock().unwrap() = Some(viewport);
            inner.egui_ctx.request_repaint();
        });
    }
}

// ── Fetch logic ───────────────────────────────────────────────────────────────

fn do_fetch(inner: &SentinelClientInner, vp: &MapViewport) -> Result<()> {
    // ── 1. STAC search ────────────────────────────────────────────────────────
    let bbox = &vp.bounds;
    let body = serde_json::json!({
        "collections": ["sentinel-2-l2a"],
        "bbox": [bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat],
        "limit": 10,
        "query": { "eo:cloud_cover": { "lt": 30 } },
        "sortby": [{ "field": "properties.eo:cloud_cover", "direction": "asc" }]
    });

    let resp = inner.agent
        .post("https://earth-search.aws.element84.com/v1/search")
        .send_json(&body)
        .map_err(ureq_err)?;

    let result: StacSearchResponse = resp.into_json().context("parsing STAC response")?;

    let Some(feature) = result.features.into_iter().next() else {
        log::info!("Sentinel: no scenes found for viewport");
        return Ok(());
    };

    let cog_url = feature.assets.visual
        .as_ref()
        .or(feature.assets.b04.as_ref())
        .map(|a| a.href.clone())
        .context("no usable asset in scene")?;

    let cloud_cover  = feature.properties.cloud_cover.unwrap_or(0.0);
    let datetime     = feature.properties.datetime.unwrap_or_else(|| "unknown".into());
    let scene_bounds = GeoBounds::new(
        feature.bbox[0], feature.bbox[1], feature.bbox[2], feature.bbox[3],
    );

    log::info!("Sentinel scene {} — cloud: {:.1}% — {}", feature.id, cloud_cover, datetime);

    let _ = inner.tx.send(crate::app::AppEvent::Sentinel(SentinelEvent::SceneFound {
        scene_id:    feature.id,
        bounds:      scene_bounds,
        cloud_cover,
        datetime,
        cog_url:     cog_url.clone(),
    }));

    // ── 2. Fetch XYZ tiles ────────────────────────────────────────────────────
    let zoom   = vp.zoom_level();
    let coords = tiles_for_bounds(&vp.bounds, zoom);
    log::info!("Sentinel: fetching {} tiles at z={zoom}", coords.len());

    for (x, y) in coords {
        let url = titiler_tile_url(&cog_url, zoom, x, y);
        match fetch_tile(&inner.agent, &url, zoom, x, y) {
            Ok(image) => {
                let _ = inner.tx.send(crate::app::AppEvent::Sentinel(
                    SentinelEvent::TileReady { z: zoom, x, y, image },
                ));
                inner.egui_ctx.request_repaint();
            }
            Err(e) => log::debug!("Sentinel tile z={zoom} x={x} y={y}: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    Ok(())
}

fn fetch_tile(agent: &ureq::Agent, url: &str, z: u8, x: u32, y: u32) -> Result<GeoImage> {
    let resp = agent.get(url).call().map_err(ureq_err)?;
    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes).context("tile body")?;

    let img = image::load_from_memory(&bytes).context("decoding tile PNG")?;
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(GeoImage {
        bounds: tile_bounds(z, x, y),
        pixels: rgba.into_raw(),
        width,
        height,
    })
}

// ── Tile math ─────────────────────────────────────────────────────────────────

fn titiler_tile_url(cog_url: &str, z: u8, x: u32, y: u32) -> String {
    let enc = percent_encode(cog_url);
    format!(
        "{TITILER_BASE}/cog/tiles/WebMercatorQuad/{z}/{x}/{y}.png\
         ?url={enc}&bidx=1&bidx=2&bidx=3&rescale=0,3000"
    )
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                vec![b as char]
            } else {
                format!("%{b:02X}").chars().collect::<Vec<_>>()
            }
        })
        .collect()
}

fn tile_bounds(z: u8, x: u32, y: u32) -> GeoBounds {
    let n = 2u32.pow(z as u32) as f64;
    let min_lon = x as f64 / n * 360.0 - 180.0;
    let max_lon = (x as f64 + 1.0) / n * 360.0 - 180.0;
    let max_lat = lat_from_y(y as f64, n);
    let min_lat = lat_from_y(y as f64 + 1.0, n);
    GeoBounds { min_lon, min_lat, max_lon, max_lat }
}

fn lat_from_y(y: f64, n: f64) -> f64 {
    (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees()
}

fn tiles_for_bounds(bounds: &GeoBounds, z: u8) -> Vec<(u32, u32)> {
    let (x0, y0) = lon_lat_to_tile(bounds.min_lon, bounds.max_lat, z);
    let (x1, y1) = lon_lat_to_tile(bounds.max_lon, bounds.min_lat, z);
    (x0..=x1).flat_map(|x| (y0..=y1).map(move |y| (x, y))).collect()
}

fn lon_lat_to_tile(lon: f64, lat: f64, z: u8) -> (u32, u32) {
    use std::f64::consts::PI;
    let n   = 2u32.pow(z as u32) as f64;
    let x   = ((lon + 180.0) / 360.0 * n).floor() as u32;
    let lat_r = lat.to_radians();
    let y   = ((1.0 - lat_r.tan().asinh() / PI) / 2.0 * n).floor() as u32;
    (x.min(n as u32 - 1), y.min(n as u32 - 1))
}

fn ureq_err(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, _) => anyhow::anyhow!("HTTP {code}"),
        ureq::Error::Transport(t)    => anyhow::anyhow!("transport: {t}"),
    }
}
