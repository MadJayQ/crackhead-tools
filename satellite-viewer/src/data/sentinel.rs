/// Sentinel-2 L2A data layer.
///
/// Imagery pipeline:
///   1. Search for the best available scene via the Element84 STAC API
///      (earth-search.aws.element84.com/v1).
///   2. Pick the least-cloudy scene whose footprint intersects the viewport.
///   3. Fetch XYZ map tiles rendered from the scene's Cloud-Optimised GeoTIFF
///      via the public TiTiler instance (titiler.xyz).
///      Band combination: 4, 3, 2 (Red, Green, Blue) → true-colour.
///      Rescale: 0–3 000 (typical L2A surface-reflectance range × 10 000).
///
/// All heavy work runs in the Tokio thread pool; results come back through
/// `AppEvent::Sentinel` on the shared `mpsc` channel.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{GeoBounds, GeoImage};
use crate::ui::map_view::MapViewport;

// ── TiTiler endpoint ──────────────────────────────────────────────────────────

const TITILER_BASE: &str = "https://titiler.xyz";

// ── STAC API types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StacSearchResponse {
    features: Vec<StacFeature>,
}

#[derive(Debug, Deserialize)]
struct StacFeature {
    id: String,
    properties: StacProperties,
    bbox: [f64; 4], // [min_lon, min_lat, max_lon, max_lat]
    assets: StacAssets,
}

#[derive(Debug, Deserialize)]
struct StacProperties {
    #[serde(rename = "eo:cloud_cover")]
    cloud_cover: Option<f64>,
    datetime: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StacAssets {
    /// True-colour visual (B04/B03/B02 composite) — not always present.
    visual: Option<StacAsset>,
    /// Raw B04 (Red) — always present; used when "visual" is absent.
    #[serde(rename = "B04")]
    b04: Option<StacAsset>,
}

#[derive(Debug, Deserialize, Clone)]
struct StacAsset {
    href: String,
}

// ── Events ────────────────────────────────────────────────────────────────────

pub enum SentinelEvent {
    /// A tile for a specific (z, x, y) arrived.
    TileReady {
        z: u8,
        x: u32,
        y: u32,
        image: GeoImage,
    },
    /// The STAC search found a scene (metadata only — tiles haven't loaded yet).
    SceneFound {
        scene_id: String,
        bounds: GeoBounds,
        cloud_cover: f64,
        datetime: String,
        cog_url: String,
    },
    /// A non-fatal error (logged but not shown as a dialog).
    Error(String),
}

// ── Client ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SentinelClient {
    inner: Arc<SentinelClientInner>,
}

struct SentinelClientInner {
    http: reqwest::Client,
    tx: mpsc::SyncSender<crate::app::AppEvent>,
    egui_ctx: egui::Context,
    fetching: AtomicBool,
    last_viewport: std::sync::Mutex<Option<MapViewport>>,
    last_fetch: std::sync::Mutex<Option<Instant>>,
}

impl SentinelClient {
    pub fn new(
        tx: mpsc::SyncSender<crate::app::AppEvent>,
        egui_ctx: egui::Context,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("satellite-viewer/0.1 (github.com/madjayq/crackhead-tools)")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("failed to build HTTP client");

        Self {
            inner: Arc::new(SentinelClientInner {
                http,
                tx,
                egui_ctx,
                fetching: AtomicBool::new(false),
                last_viewport: std::sync::Mutex::new(None),
                last_fetch: std::sync::Mutex::new(None),
            }),
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.inner.fetching.load(Ordering::Relaxed)
    }

    /// Returns true when the viewport has changed enough to warrant a new fetch.
    pub fn needs_refresh(&self, viewport: &MapViewport) -> bool {
        if self.inner.fetching.load(Ordering::Relaxed) {
            return false;
        }
        // Rate-limit to once every 5 seconds minimum.
        {
            let guard = self.inner.last_fetch.lock().unwrap();
            if let Some(t) = *guard {
                if t.elapsed().as_secs() < 5 {
                    return false;
                }
            }
        }
        // Check if the viewport has moved meaningfully.
        let guard = self.inner.last_viewport.lock().unwrap();
        match &*guard {
            None => true,
            Some(prev) => !prev.is_close_to(viewport),
        }
    }

    /// Search STAC for the best scene, then fetch its map tiles.
    pub async fn fetch_scene(&self, viewport: MapViewport) -> Result<()> {
        self.inner.fetching.store(true, Ordering::Relaxed);
        *self.inner.last_fetch.lock().unwrap() = Some(Instant::now());

        let result = self.do_fetch_scene(&viewport).await;

        self.inner.fetching.store(false, Ordering::Relaxed);
        *self.inner.last_viewport.lock().unwrap() = Some(viewport);
        self.inner.egui_ctx.request_repaint();
        result
    }

    async fn do_fetch_scene(&self, viewport: &MapViewport) -> Result<()> {
        // ── 1. STAC search ────────────────────────────────────────────────────
        let bbox = &viewport.bounds;
        let body = serde_json::json!({
            "collections": ["sentinel-2-l2a"],
            "bbox": [bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat],
            "limit": 10,
            "query": {
                "eo:cloud_cover": { "lt": 30 }
            },
            "sortby": [
                { "field": "properties.eo:cloud_cover", "direction": "asc" }
            ]
        });

        let response = self
            .inner
            .http
            .post("https://earth-search.aws.element84.com/v1/search")
            .json(&body)
            .send()
            .await
            .context("STAC search request")?;

        let status = response.status();
        anyhow::ensure!(status.is_success(), "STAC search HTTP {status}");

        let search_result: StacSearchResponse =
            response.json().await.context("parsing STAC response")?;

        let Some(feature) = search_result.features.into_iter().next() else {
            log::info!("Sentinel: no scenes found for viewport");
            return Ok(());
        };

        // Prefer the pre-rendered "visual" asset; fall back to B04 (we'll
        // ask TiTiler to composite B04/B03/B02 on the fly).
        let cog_url = feature
            .assets
            .visual
            .as_ref()
            .or(feature.assets.b04.as_ref())
            .map(|a| a.href.clone())
            .context("no usable asset in scene")?;

        let cloud_cover = feature.properties.cloud_cover.unwrap_or(0.0);
        let datetime = feature
            .properties
            .datetime
            .unwrap_or_else(|| "unknown".into());
        let scene_bounds = GeoBounds::new(
            feature.bbox[0],
            feature.bbox[1],
            feature.bbox[2],
            feature.bbox[3],
        );

        log::info!(
            "Sentinel scene {} — cloud: {:.1}% — {}",
            feature.id,
            cloud_cover,
            datetime
        );

        let _ = self
            .inner
            .tx
            .send(crate::app::AppEvent::Sentinel(SentinelEvent::SceneFound {
                scene_id: feature.id,
                bounds: scene_bounds,
                cloud_cover,
                datetime,
                cog_url: cog_url.clone(),
            }));

        // ── 2. Fetch XYZ tiles ────────────────────────────────────────────────
        let zoom = viewport.zoom_level();
        let tile_coords = tiles_for_bounds(&viewport.bounds, zoom);
        log::info!("Sentinel: fetching {} tiles at z={zoom}", tile_coords.len());

        for (x, y) in tile_coords {
            let tile_url = titiler_tile_url(&cog_url, zoom, x, y);
            match self.fetch_tile(&tile_url, zoom, x, y).await {
                Ok(image) => {
                    let _ = self.inner.tx.send(crate::app::AppEvent::Sentinel(
                        SentinelEvent::TileReady { z: zoom, x, y, image },
                    ));
                    self.inner.egui_ctx.request_repaint();
                }
                Err(e) => {
                    log::debug!("Sentinel tile z={zoom} x={x} y={y}: {e}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }

        Ok(())
    }

    async fn fetch_tile(
        &self,
        url: &str,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<GeoImage> {
        let response = self
            .inner
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("tile not found");
        }
        anyhow::ensure!(status.is_success(), "HTTP {status}");

        let bytes = response.bytes().await.context("tile body")?;
        let img = image::load_from_memory(&bytes).context("decoding tile PNG")?;
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        let bounds = tile_bounds(z, x, y);

        Ok(GeoImage {
            bounds,
            pixels: rgba.into_raw(),
            width,
            height,
        })
    }
}

// ── Tile math ─────────────────────────────────────────────────────────────────

/// Build the TiTiler URL for a single XYZ tile rendered from a COG.
fn titiler_tile_url(cog_url: &str, z: u8, x: u32, y: u32) -> String {
    // Use bands 4/3/2 (R/G/B) in true-colour; rescale from L2A surface
    // reflectance (0–10 000) to display range.
    let encoded = urlencoding_encode(cog_url);
    format!(
        "{TITILER_BASE}/cog/tiles/WebMercatorQuad/{z}/{x}/{y}.png\
         ?url={encoded}&bidx=1&bidx=2&bidx=3&rescale=0,3000",
    )
}

/// Minimal URL encoder for the `url` query parameter (encodes `:`, `/`, etc.).
fn urlencoding_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                let b = c as u32;
                format!("%{b:02X}").chars().collect::<Vec<_>>()
            }
        })
        .collect()
}

/// Returns the geographic bounding box of a Web Mercator XYZ tile.
fn tile_bounds(z: u8, x: u32, y: u32) -> GeoBounds {
    let n = 2u32.pow(z as u32) as f64;
    let min_lon = (x as f64) / n * 360.0 - 180.0;
    let max_lon = (x as f64 + 1.0) / n * 360.0 - 180.0;
    let max_lat = lat_from_tile_y(y as f64, n);
    let min_lat = lat_from_tile_y(y as f64 + 1.0, n);
    GeoBounds { min_lon, min_lat, max_lon, max_lat }
}

fn lat_from_tile_y(y: f64, n: f64) -> f64 {
    use std::f64::consts::PI;
    let lat_rad = (PI * (1.0 - 2.0 * y / n)).sinh().atan();
    lat_rad.to_degrees()
}

/// Enumerate all XYZ tile indices that overlap `bounds` at zoom `z`.
fn tiles_for_bounds(bounds: &GeoBounds, z: u8) -> Vec<(u32, u32)> {
    let (x_min, y_min) = lon_lat_to_tile(bounds.min_lon, bounds.max_lat, z);
    let (x_max, y_max) = lon_lat_to_tile(bounds.max_lon, bounds.min_lat, z);

    let mut out = Vec::new();
    for x in x_min..=x_max {
        for y in y_min..=y_max {
            out.push((x, y));
        }
    }
    out
}

fn lon_lat_to_tile(lon: f64, lat: f64, z: u8) -> (u32, u32) {
    use std::f64::consts::PI;
    let n = 2u32.pow(z as u32) as f64;
    let x = ((lon + 180.0) / 360.0 * n).floor() as u32;
    let lat_rad = lat.to_radians();
    let y = ((1.0 - lat_rad.tan().asinh() / PI) / 2.0 * n).floor() as u32;
    (x.min(n as u32 - 1), y.min(n as u32 - 1))
}
