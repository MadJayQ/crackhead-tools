/// OSM tile HTTP fetcher + wgpu texture cache.
///
/// Fetches run synchronously on worker threads from our `Executor`.
/// Decoded pixels are sent back via `std::sync::mpsc` and uploaded to wgpu
/// textures in `flush()`, which is called once per frame from the GPU thread.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{Arc, mpsc};

use anyhow::Result;

use super::{context::GpuContext, map_pipeline::MapPipeline};
use crate::executor::Executor;

// ── Tile ID ───────────────────────────────────────────────────────────────────

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub struct TileId { pub z: u8, pub x: u32, pub y: u32 }

// ── Channel events ────────────────────────────────────────────────────────────

enum TileEvent {
    Ready  { id: TileId, pixels: Vec<u8>, width: u32, height: u32 },
    Error  { id: TileId, error: String },
}

// ── Per-tile GPU state ────────────────────────────────────────────────────────

struct GpuTile {
    #[allow(dead_code)]
    texture:    wgpu::Texture,
    texture_bg: wgpu::BindGroup,
}

// ── TileCache ─────────────────────────────────────────────────────────────────

pub struct TileCache {
    agent:    ureq::Agent,
    executor: Arc<Executor>,
    tx:       mpsc::SyncSender<TileEvent>,
    rx:       mpsc::Receiver<TileEvent>,
    pending:  HashSet<TileId>,
    tiles:    HashMap<TileId, GpuTile>,
    repaint:  Arc<dyn Fn() + Send + Sync>,
}

impl TileCache {
    pub fn new(
        executor: Arc<Executor>,
        repaint:  Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel(256);
        let agent = ureq::AgentBuilder::new()
            .user_agent("satellite-viewer/0.1")
            .timeout(std::time::Duration::from_secs(15))
            .build();

        Self { agent, executor, tx, rx, pending: HashSet::new(), tiles: HashMap::new(), repaint }
    }

    /// Request `id` — no-op if already loaded or in flight.
    pub fn request(&mut self, id: TileId) {
        if self.tiles.contains_key(&id) || self.pending.contains(&id) { return; }
        self.pending.insert(id);

        let agent   = self.agent.clone();
        let tx      = self.tx.clone();
        let repaint = Arc::clone(&self.repaint);

        self.executor.spawn(move || {
            match fetch_tile(&agent, id) {
                Ok((pixels, w, h)) => {
                    let _ = tx.send(TileEvent::Ready { id, pixels, width: w, height: h });
                }
                Err(e) => {
                    let _ = tx.send(TileEvent::Error { id, error: e.to_string() });
                }
            }
            repaint();
        });
    }

    /// Upload pending tiles to the GPU.  Call once per frame from the render thread.
    pub fn flush(&mut self, gpu: &GpuContext, pipeline: &MapPipeline) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                TileEvent::Ready { id, pixels, width, height } => {
                    self.pending.remove(&id);
                    let label   = format!("osm_{}_{}_{}", id.z, id.x, id.y);
                    let texture = pipeline.upload_texture(gpu, &label, &pixels, width, height);
                    let view    = texture.create_view(&Default::default());
                    let bg      = pipeline.texture_bind_group(gpu, &view);
                    self.tiles.insert(id, GpuTile { texture, texture_bg: bg });
                }
                TileEvent::Error { id, error } => {
                    self.pending.remove(&id);
                    log::debug!("tile {id:?}: {error}");
                }
            }
        }
    }

    pub fn get(&self, id: &TileId) -> Option<&wgpu::BindGroup> {
        self.tiles.get(id).map(|t| &t.texture_bg)
    }

    pub fn evict(&mut self, keep: &HashSet<TileId>) {
        self.tiles.retain(|id, _| keep.contains(id));
    }
}

// ── HTTP fetch ────────────────────────────────────────────────────────────────

fn fetch_tile(agent: &ureq::Agent, id: TileId) -> Result<(Vec<u8>, u32, u32)> {
    let sub = match (id.x + id.y) % 3 { 0 => 'a', 1 => 'b', _ => 'c' };
    let url = format!(
        "https://{sub}.tile.openstreetmap.org/{}/{}/{}.png",
        id.z, id.x, id.y
    );
    let resp = agent.get(&url).call().map_err(|e| match e {
        ureq::Error::Status(c, _) => anyhow::anyhow!("HTTP {c}"),
        ureq::Error::Transport(t) => anyhow::anyhow!("{t}"),
    })?;

    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;

    let img = image::load_from_memory(&bytes)?.into_rgba8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w, h))
}
