//! The Previews tab: terrain (3-D on the GPU, or a hillshade map), the navmesh over it, textures and models of the
//! open project. Loading runs on workers; the 3-D view renders only when something changed and pauses while a
//! game runs (the map view needs no GPU work beyond showing an image).
use crate::preview::gpu::{self, Camera, LineVertex, Look, TerrainGpu};
use crate::preview::nav::{self, NavOverlay};
use crate::preview::density::{self, DensityGrid};
use crate::preview::navdiff::{self, Comparison};
use crate::preview_layers::{LayerAction, LayerState, LayerStatus, PreviewLayers};
use crate::preview::terrain::{self, Terrain};
use crate::preview::texture::{self, Channels, Image, Texture};
use crate::preview::model;
use crate::preview::worldtex;
use crate::projects::Project;
use crate::settings::Settings;
use crate::tasks::{Task, TaskCtx};
use crate::theme::{self, pal};
use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Vec2};
use eframe::egui_wgpu;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sub {
    Terrain,
    Textures,
    Models,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerrainMode {
    View3d,
    Map,
}

pub struct Env<'a> {
    pub project: Option<&'a Project>,
    pub wgpu: Option<&'a egui_wgpu::RenderState>,
    pub game_running: Option<String>,
}

/// what the terrain worker produces
pub struct TerrainData {
    pub terrain: Terrain,
    pub hill: Image,
    pub verts: Vec<gpu::TerrainVertex>,
    pub indices: Vec<u32>,
    pub path: PathBuf,
    /// world rectangle (x0, z0, x1, z1) worth framing first
    pub focus: (f32, f32, f32, f32),
    /// the project's world texture as one mosaic (row 0 = smallest z) and the map shaded with it
    pub world: Option<Image>,
    pub hill_world: Option<Image>,
    /// tiles that could not be read, or why there is no world texture
    pub world_notes: Vec<String>,
}

// One serial worker per resource. Keep obsolete workers until Done, cancel their
// work, and coalesce pending requests. Neither an old success nor an old error
// may become visible after cancellation, replacement, or a same-path reload.
type Work<T> = Box<dyn FnOnce(&TaskCtx<T>, &AtomicBool) -> Result<T, String> + Send>;
struct PendingWork<T> {
    generation: u64,
    label: String,
    ctx: egui::Context,
    work: Work<T>,
}
struct SerialWorker<T> {
    generation: u64,
    active: Option<(u64, Task<T>, Arc<AtomicBool>)>,
    pending: Option<PendingWork<T>>,
}
impl<T> Default for SerialWorker<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            active: None,
            pending: None,
        }
    }
}
impl<T: Send + 'static> SerialWorker<T> {
    fn request(
        &mut self,
        ctx: &egui::Context,
        label: &str,
        work: impl FnOnce(&TaskCtx<T>, &AtomicBool) -> Result<T, String> + Send + 'static,
    ) {
        self.cancel();
        self.pending = Some(PendingWork {
            generation: self.generation,
            label: label.into(),
            ctx: ctx.clone(),
            work: Box::new(work),
        });
        self.start_pending();
    }
    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        if let Some((_, _, cancel)) = &self.active {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    fn start_pending(&mut self) {
        if self.active.is_none()
            && let Some(p) = self.pending.take()
        {
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = cancel.clone();
            let task = Task::spawn_serial(p.label, &p.ctx, move |t| (p.work)(t, &worker_cancel));
            self.active = Some((p.generation, task, cancel));
        }
    }
    fn poll(&mut self) -> Option<Result<T, String>> {
        let mut result = None;
        if let Some((generation, task, _)) = self.active.as_mut()
            && task.poll()
        {
            if *generation == self.generation {
                result = task.take_result();
            }
            self.active = None;
        }
        self.start_pending();
        result
    }
    fn busy(&self) -> bool {
        self.active.is_some() || self.pending.is_some()
    }
    fn loading(&self) -> bool {
        self.pending.is_some()
            || self
                .active
                .as_ref()
                .is_some_and(|(generation, _, _)| *generation == self.generation)
    }
    fn progress(&self) -> f32 {
        self.active
            .as_ref()
            .filter(|(g, _, _)| *g == self.generation)
            .map_or(-1.0, |(_, t, _)| t.progress)
    }
    fn phase(&self) -> &str {
        if self.pending.is_some() {
            "waiting for the previous task to stop"
        } else {
            self.active
                .as_ref()
                .map_or("", |(_, t, _)| t.phase.as_str())
        }
    }
}
impl<T> Drop for SerialWorker<T> {
    fn drop(&mut self) {
        if let Some((_, _, c)) = &self.active {
            c.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DensityStyle {
    ceiling_bits: u64,
    opacity: u8,
}
impl DensityStyle {
    fn from_controls(layers: &PreviewLayers) -> Self {
        Self {
            ceiling_bits: layers.ceiling_per_ha.to_bits(),
            opacity: layers.opacity,
        }
    }
    fn ceiling(self) -> f64 {
        f64::from_bits(self.ceiling_bits)
    }
}
struct DensityMap {
    path: PathBuf,
    grid: Arc<DensityGrid>,
    image: Image,
}
/// One nav texel stays close to a displayed physical pixel. All layers still
/// cover the same world bounds and use the same UVs, regardless of resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NavRasterSize {
    width: usize,
    height: usize,
}
impl NavRasterSize {
    const MAX_SIDE: usize = 2048;

    fn for_scene_layer(
        ctx: &egui::Context,
        layer: egui::LayerId,
        content_size: Vec2,
        previous: Option<Self>,
    ) -> Option<Self> {
        // egui removes identity transforms from its layer map.
        let scale = ctx
            .layer_transform_to_global(layer)
            .map_or(1.0, |transform| transform.scaling);
        Self::for_display(content_size * scale, ctx.pixels_per_point(), previous)
    }

    fn for_display(
        span_points: Vec2,
        pixels_per_point: f32,
        previous: Option<Self>,
    ) -> Option<Self> {
        let span = span_points * pixels_per_point;
        if !span.x.is_finite() || !span.y.is_finite() || span.x <= 0.0 || span.y <= 0.0 {
            return None;
        }
        let longest = span.max_elem();
        let quantum = if longest < 64.0 { 4.0 } else { 64.0 };
        if let Some(size) = previous {
            let side = size.width.max(size.height) as f32;
            // Keep tiny layout changes from repeatedly cancelling useful work.
            // Minification remains bounded: at most about 1.12 texels/pixel.
            let width = (side * (span.x / longest)).round().clamp(1.0, side) as usize;
            let height = (side * (span.y / longest)).round().clamp(1.0, side) as usize;
            let same_aspect = size.width.abs_diff(width) <= 1 && size.height.abs_diff(height) <= 1;
            if same_aspect
                && longest >= side * 0.9
                && (side == Self::MAX_SIDE as f32 || longest < (side + quantum) * 1.1)
            {
                return Some(size);
            }
        }
        let side = ((longest / quantum).floor() * quantum).clamp(1.0, Self::MAX_SIDE as f32);
        Some(Self {
            width: (side * (span.x / longest)).round().clamp(1.0, side) as usize,
            height: (side * (span.y / longest)).round().clamp(1.0, side) as usize,
        })
    }
}

fn map_size(t: &Terrain) -> Result<(usize, usize), String> {
    let b = density::MapBounds::from_terrain(t)?;
    let (dx, dz) = (b.x1 - b.x0, b.z1 - b.z0);
    let side = 2048.0;
    Ok(if dx >= dz {
        (2048, (side * dz / dx).round().max(1.0) as usize)
    } else {
        ((side * dx / dz).round().max(1.0) as usize, 2048)
    })
}

fn image_texture(ctx: &egui::Context, name: &str, image: &Image) -> egui::TextureHandle {
    let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba);
    ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelRequest {
    generation: u64,
    path: PathBuf,
    options: model::LoadOptions,
}

pub struct PreviewView {
    pub sub: Sub,
    pub mode: TerrainMode,
    terrain_key: String,
    terrain_task: SerialWorker<Arc<TerrainData>>,
    pub terrain: Option<Result<Arc<TerrainData>, String>>,
    hill_tex: Option<(String, egui::TextureHandle)>,
    nav_key: String,
    nav_task: SerialWorker<Arc<NavOverlay>>,
    pub nav: Option<Result<Arc<NavOverlay>, String>>,
    nav_tex: Option<(String, egui::TextureHandle)>,
    dataset_key: String,
    layer_root: Option<PathBuf>,
    preview_ctx: Option<egui::Context>,
    pub layers: PreviewLayers,
    nav_raster_task: SerialWorker<Image>,
    nav_raster_key: String,
    nav_raster: Option<Result<Image, String>>,
    nav_raster_size: Option<NavRasterSize>,
    density_task: SerialWorker<DensityMap>,
    density_data: Option<Result<DensityMap, String>>,
    density_style: Option<DensityStyle>,
    density_reading: bool,
    density_tex: Option<(u64, egui::TextureHandle)>,
    comparison_task: SerialWorker<Arc<Comparison>>,
    comparison_data: Option<Result<Arc<Comparison>, String>>,
    comparison_raster_task: SerialWorker<Image>,
    comparison_raster_key: String,
    comparison_raster: Option<Result<Image, String>>,
    comparison_tex: Option<(String, egui::TextureHandle)>,
    gpu: Option<TerrainGpu>,
    gpu_error: Option<String>,
    gpu_mesh_for: String,
    gpu_lines_for: String,
    pub cam: Option<Camera>,
    pub exaggeration: f32,
    pub show_nav: bool,
    pub show_grid: bool,
    /// drape the project's world texture (when it has one)
    pub show_world: bool,
    map_rect: egui::Rect,
    // textures
    tex_list_key: String,
    tex_list_task: Option<Task<Vec<PathBuf>>>,
    pub tex_list: Vec<PathBuf>,
    tex_filter: String,
    tex_task: Option<Task<Texture>>,
    pub texture: Option<Result<Texture, String>>,
    tex_handle: Option<(String, egui::TextureHandle)>,
    pub tex_level: usize,
    pub tex_channels: Channels,
    tex_zoom: f32,
    // models
    model_list_key: String,
    model_list_task: Option<Task<Vec<PathBuf>>>,
    pub model_list: Vec<PathBuf>,
    model_filter: String,
    model_task: Option<(ModelRequest, Task<Arc<model::Model>>)>,
    model_request: Option<ModelRequest>,
    model_options: model::LoadOptions,
    model_generation: u64,
    model_ctx: Option<egui::Context>,
    model_gpu_error: Option<String>,
    model_lines_for: String,
    pub model: Option<Result<Arc<model::Model>, String>>,
    model_gpu: Option<TerrainGpu>,
    model_gpu_for: String,
    model_cam: Option<Camera>,
    model_rect: egui::Rect,
    pub model_wire: bool,
    pub model_textures: bool,
    pub model_normals: bool,
    pub model_bones: bool,
    pub model_inspection: u8,
    pub model_mesh: Option<usize>,
    pub model_bone: Option<usize>,
}

impl Default for PreviewView {
    fn default() -> Self {
        PreviewView {
            sub: Sub::Terrain,
            mode: TerrainMode::View3d,
            terrain_key: String::new(),
            terrain_task: SerialWorker::default(),
            terrain: None,
            hill_tex: None,
            nav_key: String::new(),
            nav_task: SerialWorker::default(),
            nav: None,
            nav_tex: None,
            dataset_key: String::new(),
            layer_root: None,
            preview_ctx: None,
            layers: PreviewLayers::default(),
            nav_raster_task: SerialWorker::default(),
            nav_raster_key: String::new(),
            nav_raster: None,
            nav_raster_size: None,
            density_task: SerialWorker::default(),
            density_data: None,
            density_style: None,
            density_reading: false,
            density_tex: None,
            comparison_task: SerialWorker::default(),
            comparison_data: None,
            comparison_raster_task: SerialWorker::default(),
            comparison_raster_key: String::new(),
            comparison_raster: None,
            comparison_tex: None,
            gpu: None,
            gpu_error: None,
            gpu_mesh_for: String::new(),
            gpu_lines_for: String::new(),
            cam: None,
            exaggeration: 1.0,
            show_nav: true,
            show_grid: false,
            show_world: true,
            map_rect: egui::Rect::ZERO,
            tex_list_key: String::new(),
            tex_list_task: None,
            tex_list: vec![],
            tex_filter: String::new(),
            tex_task: None,
            texture: None,
            tex_handle: None,
            tex_level: 0,
            tex_channels: Channels::Rgba,
            tex_zoom: 0.0,
            model_list_key: String::new(),
            model_list_task: None,
            model_list: vec![],
            model_filter: String::new(),
            model_task: None,
            model_request: None,
            model_options: model::LoadOptions::default(),
            model_generation: 0,
            model_ctx: None,
            model_gpu_error: None,
            model_lines_for: String::new(),
            model: None,
            model_gpu: None,
            model_gpu_for: String::new(),
            model_cam: None,
            model_rect: egui::Rect::ZERO,
            model_wire: false,
            model_textures: true,
            model_normals: false,
            model_bones: false,
            model_inspection: 0,
            model_mesh: None,
            model_bone: None,
        }
    }
}

/// files with these extensions under the paths (bounded walk, sorted)
pub fn list_files(paths: &[PathBuf], exts: &[&str], max: usize) -> Vec<PathBuf> {
    let mut out = vec![];
    fn walk(d: &Path, exts: &[&str], out: &mut Vec<PathBuf>, max: usize, depth: usize) {
        if depth > 24 || out.len() >= max {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut es: Vec<_> = rd.flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&p, exts, out, max, depth + 1);
            } else if p.extension().is_some_and(|x| exts.iter().any(|e| x.eq_ignore_ascii_case(e))) {
                out.push(p);
                if out.len() >= max {
                    return;
                }
            }
        }
    }
    for p in paths {
        if p.is_file() {
            out.push(p.clone());
        } else {
            walk(p, exts, &mut out, max, 0);
        }
    }
    out
}

impl PreviewView {
    pub fn busy(&self) -> bool {
        self.terrain_task.busy()
            || self.nav_task.busy()
            || self.nav_raster_task.busy()
            || self.density_task.busy()
            || self.comparison_task.busy()
            || self.comparison_raster_task.busy()
            || self.tex_task.is_some()
            || self.model_task.is_some()
    }

    pub fn poll(&mut self) {
        if let Some(result) = self.terrain_task.poll() {
            self.terrain = Some(result);
        }
        if let Some(result) = self.nav_task.poll() {
            self.nav = Some(result);
        }
        if let Some(result) = self.nav_raster_task.poll() {
            self.nav_raster = Some(result);
            self.nav_tex = None;
        }
        if let Some(result) = self.density_task.poll() {
            self.density_data = Some(result);
            self.density_tex = None;
            self.density_reading = false;
        }
        if let Some(result) = self.comparison_task.poll() {
            self.comparison_data = Some(result);
            self.invalidate_comparison_raster();
        }
        if let Some(result) = self.comparison_raster_task.poll() {
            self.comparison_raster = Some(result);
            self.comparison_tex = None;
        }
        if let Some(ctx) = self.preview_ctx.clone() {
            self.ensure_map_raster(&ctx);
            self.ensure_comparison_raster(&ctx);
        }
        if let Some(t) = self.tex_task.as_mut()
            && t.poll()
        {
            self.texture = t.take_result();
            self.tex_task = None;
            self.tex_handle = None;
            self.tex_level = 0;
            self.tex_zoom = 0.0;
        }
        if let Some(t) = self.tex_list_task.as_mut()
            && t.poll()
        {
            self.tex_list = t.take_result().and_then(|r| r.ok()).unwrap_or_default();
            self.tex_list_task = None;
        }
        let mut restart_model = false;
        if let Some((request, t)) = self.model_task.as_mut()
            && t.poll()
        {
            if self.model_request.as_ref() == Some(request) {
                self.model = t.take_result();
            } else {
                restart_model = self.model_request.is_some();
            }
            self.model_task = None;
        }
        if restart_model
            && let Some(request) = self.model_request.clone()
            && let Some(ctx) = self.model_ctx.clone()
        {
            // Coalesce obsolete model workers, preserving P's existing target contract.
            self.start_model(request, &ctx);
        }
        if let Some(t) = self.model_list_task.as_mut()
            && t.poll()
        {
            self.model_list = t.take_result().and_then(|r| r.ok()).unwrap_or_default();
            self.model_list_task = None;
        }
    }

    /// Call before poll even when Previews is hidden. Paths are captured from the
    /// selected project, never resolved against the process working directory.
    pub fn synchronize_dataset(&mut self, tools: &Path, project: Option<&Project>) {
        let key = format!(
            "{:?}|{:?}",
            crate::projects::clean(tools),
            project.map(|p| (&p.file, &p.root, &p.spec.preview))
        );
        if self.dataset_key != key {
            self.dataset_key = key;
            self.layer_root = project.map(|p| p.root.clone());
            self.invalidate_terrain();
            self.layers.reset();
        }
    }

    fn invalidate_comparison_raster(&mut self) {
        self.comparison_raster_task.cancel();
        self.comparison_raster_key.clear();
        self.comparison_raster = None;
        self.comparison_tex = None;
    }

    fn invalidate_layers(&mut self) {
        self.density_task.cancel();
        self.comparison_task.cancel();
        self.density_data = None;
        self.comparison_data = None;
        self.density_tex = None;
        self.invalidate_comparison_raster();
        self.density_style = None;
        self.density_reading = false;
    }

    fn invalidate_terrain(&mut self) {
        self.terrain_task.cancel();
        self.nav_task.cancel();
        self.nav_raster_task.cancel();
        self.terrain = None;
        self.nav = None;
        self.nav_raster = None;
        self.nav_raster_size = None;
        self.terrain_key.clear();
        self.nav_key.clear();
        self.nav_raster_key.clear();
        self.hill_tex = None;
        self.nav_tex = None;
        self.cam = None;
        self.map_rect = egui::Rect::ZERO;
        self.gpu_error = None;
        self.gpu_mesh_for.clear();
        self.gpu_lines_for.clear();
        self.invalidate_layers();
    }

    /// Explicit same-path reload invalidates every dependent map and comparison.
    pub fn reload_terrain(&mut self, ctx: &egui::Context, project: &Project) {
        self.invalidate_terrain();
        self.ensure_terrain(ctx, project);
    }

    /// (re)load the project's terrain and navmesh when their paths change
    fn ensure_terrain(&mut self, ctx: &egui::Context, pr: &Project) {
        self.preview_ctx = Some(ctx.clone());
        let Some(h) = pr.heights() else { return };
        let cell = pr.spec.preview.cell_m.unwrap_or(2.0);
        let origin = pr.spec.preview.origin.map(|o| (o[0], o[1]));
        let water = pr.spec.preview.water_y;
        let wt_dir = pr.world_texture();
        let key = format!("{}|{cell}|{origin:?}|{water:?}|{wt_dir:?}", h.display());
        if key != self.terrain_key {
            self.terrain_key = key;
            self.terrain = None;
            self.hill_tex = None;
            self.nav_tex = None;
            self.cam = None;
            let path = h.clone();
            self.terrain_task.request(ctx, "Loading terrain", move |t, cancel| {
                density::check_cancel(cancel)?;
                if !cell.is_finite() || cell <= 0.0 || origin.is_some_and(|(x, z)| !x.is_finite() || !z.is_finite())
                    || water.is_some_and(|y| !y.is_finite()) {
                    return Err("terrain cell size, origin and water height must be finite; cell size must be positive".into());
                }
                t.progress(-1.0, "reading heights");
                let terrain = terrain::load(&path, cell, origin)?;
                density::MapBounds::from_terrain(&terrain)?;
                density::check_cancel(cancel)?;
                t.progress(-1.0, "shading");
                let hill = terrain::hillshade(&terrain, 1024, water, 1.0);
                density::check_cancel(cancel)?;
                t.progress(-1.0, "building the mesh");
                let (verts, indices) = terrain::mesh(&terrain, 1024);
                density::check_cancel(cancel)?;
                let focus = terrain::focus(&terrain, water);
                let (mut world, mut hill_world, mut world_notes) = (None, None, vec![]);
                if let Some(d) = wt_dir {
                    t.progress(-1.0, "assembling the world texture");
                    let tiles = worldtex::find_tiles(&d);
                    let n = tiles.iter().map(|x| x.0.max(x.1) + 1).max().unwrap_or(1);
                    match worldtex::mosaic(&tiles, (2048 / n).clamp(64, 1024)) {
                        Ok((img, notes)) => {
                            hill_world = Some(terrain::hillshade_with(&terrain, 1024, water, 1.0, Some(&img)));
                            world = Some(img);
                            world_notes = notes;
                        }
                        Err(e) => world_notes.push(format!("{}: {e}", d.display())),
                    }
                }
                density::check_cancel(cancel)?;
                Ok(Arc::new(TerrainData { terrain, hill, verts, indices, path, focus, world, hill_world, world_notes }))
            });
        }
        let navs = pr.nav();
        let nkey = format!("{navs:?}");
        if nkey != self.nav_key {
            self.nav_key = nkey;
            self.nav = None;
            self.nav_tex = None;
            self.nav_task.cancel();
            if !navs.is_empty() {
                self.nav_task
                    .request(ctx, "Loading navmesh", move |t, cancel| {
                        density::check_cancel(cancel)?;
                        let o = nav::load(&navs, cancel, |i, n| {
                            t.progress(
                                i as f32 / n.max(1) as f32,
                                format!("{i} of {n} navmesh files"),
                            )
                        });
                        density::check_cancel(cancel)?;
                        if o.files == 0 && o.errors.is_empty() {
                            return Err(
                                "No .nav2 files found in the project's navmesh paths.".into()
                            );
                        }
                        Ok(Arc::new(o))
                    });
            }
        }
    }

    fn terrain_lines_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.terrain_task.generation,
            self.nav_task.generation,
            matches!(self.nav, Some(Ok(_))),
            self.show_nav,
            self.show_grid
        )
    }

    fn ensure_map_raster(&mut self, ctx: &egui::Context) {
        let (Some(Ok(terrain)), Some(Ok(nav)), Some(size)) =
            (&self.terrain, &self.nav, self.nav_raster_size)
        else {
            return;
        };
        let key = format!(
            "{}|{}|{}x{}",
            self.terrain_task.generation, self.nav_task.generation, size.width, size.height
        );
        if self.nav_raster_key == key {
            return;
        }
        self.nav_raster_key = key;
        self.nav_tex = None;
        self.nav_raster = None;
        let (terrain, nav) = (terrain.clone(), nav.clone());
        self.nav_raster_task
            .request(ctx, "Drawing navmesh map", move |t, cancel| {
                t.progress(-1.0, "drawing the current navmesh");
                navdiff::raster_overlay(&terrain.terrain, &nav, size.width, size.height, cancel)
            });
    }

    fn ensure_comparison_raster(&mut self, ctx: &egui::Context) {
        let (Some(Ok(terrain)), Some(Ok(comparison)), Some(size)) =
            (&self.terrain, &self.comparison_data, self.nav_raster_size)
        else {
            return;
        };
        if !matches!(comparison.as_ref(), Comparison::Ready(_)) {
            return;
        }
        let key = format!(
            "{}|{}|{}x{}",
            self.terrain_task.generation, self.comparison_task.generation, size.width, size.height
        );
        if self.comparison_raster_key == key {
            return;
        }
        self.comparison_raster_key = key;
        self.comparison_tex = None;
        self.comparison_raster = None;
        let (terrain, comparison) = (terrain.clone(), comparison.clone());
        self.comparison_raster_task
            .request(ctx, "Drawing navmesh difference", move |t, cancel| {
                t.progress(-1.0, "drawing the cached navmesh difference");
                let Comparison::Ready(diff) = comparison.as_ref() else {
                    return Err("Navmesh difference is unavailable.".into());
                };
                diff.raster(&terrain.terrain, size.width, size.height, cancel)
            });
    }

    pub fn layer_status(&self) -> LayerStatus<'_> {
        LayerStatus {
            terrain_available: matches!(self.terrain, Some(Ok(_))),
            nav_available: matches!(self.nav, Some(Ok(_))),
            density: if self.density_reading && self.density_task.loading() {
                LayerState::Loading {
                    fraction: self.density_task.progress(),
                }
            } else {
                match &self.density_data {
                    Some(Ok(data)) => LayerState::Ready(&data.grid),
                    Some(Err(e)) => LayerState::Error(e),
                    None => LayerState::Idle,
                }
            },
            comparison: if self.comparison_task.loading() {
                LayerState::Loading {
                    fraction: self.comparison_task.progress(),
                }
            } else if self.comparison_raster_task.loading() {
                LayerState::Loading {
                    fraction: self.comparison_raster_task.progress(),
                }
            } else {
                match &self.comparison_data {
                    Some(Ok(data)) => match &self.comparison_raster {
                        Some(Err(error)) => LayerState::Error(error),
                        _ => LayerState::Ready(data.as_ref()),
                    },
                    Some(Err(e)) => LayerState::Error(e),
                    None => LayerState::Idle,
                }
            },
        }
    }

    /// Dispatch the same typed actions emitted by the visible layer controls.
    /// Relative paths use the selected project's captured root.
    pub fn apply_layer_action(&mut self, ctx: &egui::Context, action: LayerAction) {
        match action {
            LayerAction::CancelDensity => {
                self.density_task.cancel();
                self.density_reading = false;
                self.density_data = Some(Err("Density loading cancelled.".into()));
                self.density_tex = None;
            }
            LayerAction::CancelNav => {
                self.comparison_task.cancel();
                self.comparison_data = Some(Err("Navmesh comparison cancelled.".into()));
                self.invalidate_comparison_raster();
            }
            LayerAction::LoadDensity { path, options } => {
                self.density_task.cancel();
                self.density_data = None;
                self.density_tex = None;
                let (Some(root), Some(Ok(terrain))) = (&self.layer_root, &self.terrain) else {
                    self.density_data = Some(Err(
                        "Load the selected project's terrain before measuring density.".into(),
                    ));
                    return;
                };
                let path = if path.is_absolute() {
                    path
                } else {
                    crate::projects::clean(&root.join(path))
                };
                let terrain = terrain.clone();
                let style = DensityStyle::from_controls(&self.layers);
                self.density_style = Some(style);
                self.density_reading = true;
                self.mode = TerrainMode::Map;
                self.density_task
                    .request(ctx, "Loading density map", move |t, cancel| {
                        let grid = Arc::new(density::load(
                            &path,
                            &terrain.terrain,
                            options,
                            cancel,
                            |f, p| t.progress(f, p),
                        )?);
                        t.progress(-1.0, "drawing density");
                        let (w, h) = map_size(&terrain.terrain)?;
                        let image = grid.raster(w, h, style.ceiling(), style.opacity, cancel)?;
                        Ok(DensityMap { path, grid, image })
                    });
            }
            LayerAction::CompareNav { paths, options } => {
                self.comparison_task.cancel();
                self.comparison_data = None;
                self.invalidate_comparison_raster();
                let (Some(root), Some(Ok(_)), Some(Ok(nav))) =
                    (&self.layer_root, &self.terrain, &self.nav)
                else {
                    self.comparison_data = Some(Err(
                        "Load the selected project's terrain and current navmesh before comparing."
                            .into(),
                    ));
                    return;
                };
                let paths: Vec<_> = paths
                    .into_iter()
                    .map(|p| {
                        if p.is_absolute() {
                            p
                        } else {
                            crate::projects::clean(&root.join(p))
                        }
                    })
                    .collect();
                let nav = nav.clone();
                self.mode = TerrainMode::Map;
                self.comparison_task
                    .request(ctx, "Comparing navmesh", move |t, cancel| {
                        let comparison =
                            navdiff::compare_selected(&nav, &paths, options, cancel, |i, n| {
                                t.progress(
                                    i as f32 / n.max(1) as f32,
                                    format!("{i} of {n} baseline files"),
                                )
                            })?;
                        Ok(Arc::new(comparison))
                    });
            }
        }
    }

    fn ensure_density_style(&mut self, ctx: &egui::Context) {
        let style = DensityStyle::from_controls(&self.layers);
        if self.density_style == Some(style) || self.density_reading {
            return;
        }
        let (Some(Ok(data)), Some(Ok(terrain))) = (&self.density_data, &self.terrain) else {
            return;
        };
        let (grid, path, terrain) = (data.grid.clone(), data.path.clone(), terrain.clone());
        self.density_style = Some(style);
        self.density_tex = None;
        self.density_task
            .request(ctx, "Drawing density map", move |t, cancel| {
                t.progress(-1.0, "drawing density with the selected colours");
                let (w, h) = map_size(&terrain.terrain)?;
                let image = grid.raster(w, h, style.ceiling(), style.opacity, cancel)?;
                Ok(DensityMap { path, grid, image })
            });
    }

    fn layer_controls(&mut self, ui: &mut egui::Ui) {
        // Raster completion must not move controls between pointer-down and pointer-up.
        let status_height = ui.spacing().interact_size.y;
        ui.allocate_ui(Vec2::new(ui.available_width(), status_height), |ui| {
            ui.set_min_height(status_height);
            if self.nav_raster_task.loading() {
                ui.label(RichText::new("Drawing navmesh map…").color(pal(ui).muted));
            }
        });
        let mut controls = std::mem::take(&mut self.layers);
        let actions = egui::CollapsingHeader::new("Density and navmesh comparison").default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("preview_layer_controls").max_height(380.0).show(ui, |ui| {
                ui.label("Map overlays use the selected project's world bounds; relative paths start at its root.");
                let drawing = self.comparison_raster_task.loading();
                let cached = self.comparison_data.as_ref().and_then(|result| result.as_ref().ok());
                let ready = cached.is_some_and(|data| matches!(data.as_ref(), Comparison::Ready(_)));
                let mut status = self.layer_status();
                if drawing && let Some(data) = cached {
                    // Only raster work reuses this legend. A new baseline request
                    // clears comparison_data, so prior geometry is never shown.
                    status.comparison = LayerState::Ready(data.as_ref());
                }
                let mut actions = controls.show(ui, status);
                if ready {
                    // Keep one task row even when idle: progress/cancellation must
                    // not resize the map and trigger a second, opposing redraw.
                    let row_height = ui.spacing().interact_size.y;
                    ui.allocate_ui(Vec2::new(ui.available_width(), row_height), |ui| {
                        ui.set_min_height(row_height);
                        if drawing {
                            ui.horizontal(|ui| {
                                let fraction = self.comparison_raster_task.progress();
                                if fraction.is_finite() && fraction >= 0.0 {
                                    ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                                        .desired_width(160.0).text("Drawing navmesh difference"));
                                } else {
                                    ui.spinner();
                                    ui.label("Drawing navmesh difference");
                                }
                                if ui.button("Cancel nav comparison").clicked() {
                                    actions.push(LayerAction::CancelNav);
                                }
                            });
                        }
                    });
                }
                actions
            }).inner
        }).body_returned.unwrap_or_default();
        self.layers = controls;
        let ctx = ui.ctx().clone();
        for action in actions {
            self.apply_layer_action(&ctx, action);
        }
        self.ensure_density_style(&ctx);
        if self.density_task.loading() && !self.density_reading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Updating density colours…");
            });
        }
        if let Some(Ok(data)) = &self.density_data {
            ui.label(RichText::new(data.path.display().to_string()).small());
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        self.synchronize_dataset(&s.resolved_root(), env.project);
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.sub, Sub::Terrain, "Terrain + navmesh");
            ui.selectable_value(&mut self.sub, Sub::Textures, "Textures");
            ui.selectable_value(&mut self.sub, Sub::Models, "Models");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                match env.project {
                    Some(pr) => {
                        ui.label(RichText::new(pr.name()).strong());
                        ui.label(RichText::new("project").color(p.muted));
                    }
                    None => {
                        ui.label(RichText::new("no project open").color(p.muted));
                    }
                }
            });
        });
        ui.add_space(4.0);
        match self.sub {
            Sub::Terrain => self.terrain_ui(ui, env),
            Sub::Textures => self.textures_ui(ui, s, env),
            Sub::Models => self.models_ui(ui, s, env),
        }
    }

    fn terrain_ui(&mut self, ui: &mut egui::Ui, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let Some(pr) = env.project else {
            ui.label(RichText::new("Open a project (Project tab) to preview its terrain.").color(p.muted));
            return;
        };
        if pr.heights().is_none() {
            ui.label(RichText::new("This project names no height grid ([preview] heights in foxproject.toml).").color(p.muted));
            return;
        }
        self.ensure_terrain(&ctx, pr);
        let gpu_ok = env.wgpu.is_some() && env.game_running.is_none() && self.gpu_error.is_none();
        // toolbar
        ui.horizontal(|ui| {
            ui.add_enabled_ui(env.wgpu.is_some(), |ui| {
                ui.selectable_value(&mut self.mode, TerrainMode::View3d, "3-D");
            });
            ui.selectable_value(&mut self.mode, TerrainMode::Map, "Map");
            ui.separator();
            ui.checkbox(&mut self.show_nav, "Navmesh");
            let has_world = self.terrain.as_ref().is_some_and(|t| t.as_ref().is_ok_and(|d| d.world.is_some()));
            ui.add_enabled_ui(has_world, |ui| {
                ui.checkbox(&mut self.show_world, "World texture").on_hover_text("The project's world-texture tiles ([preview] world_texture)");
            });
            if self.mode == TerrainMode::View3d {
                ui.checkbox(&mut self.show_grid, "Grid 128 m");
                ui.label("Height ×");
                ui.add(egui::Slider::new(&mut self.exaggeration, 0.5..=4.0).step_by(0.25).fixed_decimals(2));
                if ui.button("Reset view").clicked() {
                    self.cam = None;
                }
            } else if ui.button("Fit").clicked() {
                self.map_rect = egui::Rect::ZERO;
            }
            if ui.button("Reload").on_hover_text("Reload terrain and navmesh, invalidating density and comparison results").clicked() {
                self.reload_terrain(&ctx, pr);
            }
            if self.terrain_task.busy() || self.nav_task.busy() {
                ui.spinner();
                let phase = if self.terrain_task.busy() { self.terrain_task.phase() } else { self.nav_task.phase() };
                ui.label(RichText::new(phase).color(p.muted));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(Ok(t)) = &self.terrain {
                    let tr = &t.terrain;
                    ui.label(RichText::new(format!("{} x {} · {} m cells · {:.1} .. {:.1} m", tr.cols, tr.rows, tr.cell, tr.min, tr.max)).color(p.muted));
                }
                if let Some(Ok(n)) = &self.nav {
                    ui.label(RichText::new(format!("{} navmesh files, {} polygons ·", n.files, n.polygons)).color(p.muted));
                }
            });
        });
        if self.mode == TerrainMode::View3d && env.wgpu.is_none() {
            self.mode = TerrainMode::Map;
        }
        if let Some(g) = &env.game_running && self.mode == TerrainMode::View3d {
            ui.colored_label(p.warn, format!("{g} is running: the 3-D view is paused (no GPU work); showing the map."));
        }
        if let Some(e) = &self.gpu_error {
            ui.colored_label(p.err, format!("3-D view unavailable: {e}"));
        }
        if let Some(Ok(d)) = &self.terrain && !d.world_notes.is_empty() {
            ui.colored_label(p.warn, format!("World texture: {}", d.world_notes[0])).on_hover_text(d.world_notes.join("\n"));
        }
        if let Some(Ok(n)) = &self.nav && !n.errors.is_empty() {
            ui.colored_label(p.warn, format!("{} navmesh file(s) unreadable: {}", n.errors.len(), n.errors[0]))
                .on_hover_text(n.errors.join("\n"));
        }
        if let Some(Err(e)) = &self.nav { ui.colored_label(p.err, format!("Could not load the navmesh: {e}")); }
        if let Some(Err(e)) = &self.nav_raster { ui.colored_label(p.err, format!("Navmesh map unavailable: {e}")); }
        self.layer_controls(ui);
        let data = match &self.terrain {
            Some(Ok(d)) => d.clone(),
            Some(Err(e)) => {
                ui.colored_label(p.err, format!("Could not load the terrain: {e}"));
                return;
            }
            None => {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.label("Loading the terrain…");
                });
                return;
            }
        };
        let water = pr.spec.preview.water_y;
        if self.mode == TerrainMode::View3d && gpu_ok {
            self.view_3d(ui, env.wgpu.unwrap(), &data, water);
        } else {
            self.map(ui, &data);
        }
    }

    fn view_3d(&mut self, ui: &mut egui::Ui, rs: &egui_wgpu::RenderState, data: &Arc<TerrainData>, water: Option<f32>) {
        let p = pal(ui);
        if self.gpu.is_none() {
            // pipeline creation can fail on exotic adapters: report instead of panicking
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| TerrainGpu::new(rs))) {
                Ok(g) => self.gpu = Some(g),
                Err(_) => {
                    self.gpu_error = Some("could not create the terrain pipeline on this GPU".into());
                    return;
                }
            }
        }
        let t = &data.terrain;
        let lines_key = self.terrain_lines_key();
        let g = self.gpu.as_mut().unwrap();
        let mesh_key = self.terrain_key.clone();
        if self.gpu_mesh_for != mesh_key {
            if g.is_software() {
                // a software rasteriser (WARP, llvmpipe): 256 x 256 cells instead of 1024 x 1024 keeps it interactive
                let (v, i) = terrain::mesh(t, 256);
                g.set_mesh(&v, &i);
            } else {
                g.set_mesh(&data.verts, &data.indices);
            }
            let (x0, z0, x1, z1) = t.extent();
            g.set_world_texture(data.world.as_ref(), (x0, z0, x1, z1));
            self.gpu_mesh_for = mesh_key;
            self.gpu_lines_for.clear();
        }
        let nav = self.nav.as_ref().and_then(|n| n.as_ref().ok()).cloned();

        if self.gpu_lines_for != lines_key {
            let mut lines: Vec<LineVertex> = vec![];
            if self.show_grid {
                lines.extend(terrain::grid_lines(t, 128.0, [1.0, 1.0, 1.0, 0.25]));
            }
            if self.show_nav && let Some(n) = &nav {
                lines.reserve(n.edges.len() * 2);
                for (a, b, bd) in &n.edges {
                    let c = if *bd { [1.0, 0.45, 0.08, 1.0] } else { [0.15, 0.75, 1.0, 0.55] };
                    lines.push(LineVertex { pos: [a[0], a[1] + 0.15, a[2]], color: c });
                    lines.push(LineVertex { pos: [b[0], b[1] + 0.15, b[2]], color: c });
                }
            }
            g.set_lines(&lines);
            self.gpu_lines_for = lines_key;
        }
        let (x0, z0, x1, z1) = t.extent();
        let span = (x1 - x0).max(z1 - z0);
        let f = data.focus;
        let cam = self.cam.get_or_insert_with(|| {
            let (cx, cz) = ((f.0 + f.2) / 2.0, (f.1 + f.3) / 2.0);
            Camera { target: [cx, t.height(cx, cz), cz], yaw: 0.6, pitch: 0.55, distance: (f.2 - f.0).max(f.3 - f.1) * 1.1, fov_y: 0.75 }
        });
        let avail = ui.available_size().max(Vec2::splat(64.0));
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        // orbit / pan / zoom
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            cam.yaw -= d.x * 0.006;
            cam.pitch = (cam.pitch + d.y * 0.006).clamp(0.05, 1.5);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            let k = cam.distance * 0.0016;
            let (cy, sy) = (cam.yaw.cos(), cam.yaw.sin());
            // grab-the-ground panning: right = (cos yaw, -sin yaw), forward = (-sin yaw, -cos yaw) on x / z
            cam.target[0] -= (d.x * cy + d.y * sy) * k;
            cam.target[2] -= (-d.x * sy + d.y * cy) * k;
            cam.target[0] = cam.target[0].clamp(x0, x1);
            cam.target[2] = cam.target[2].clamp(z0, z1);
            cam.target[1] = t.height(cam.target[0], cam.target[2]);
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                cam.distance = (cam.distance * (1.0 - scroll * 0.0015)).clamp(20.0, span * 3.0);
            }
        }
        let ppp = ui.ctx().pixels_per_point();
        let size = [(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32];
        let bg = ui.visuals().extreme_bg_color;
        let look = Look {
            hmin: t.min,
            hmax: t.max,
            water,
            exaggeration: self.exaggeration,
            background: [bg.r() as f32 / 255.0, bg.g() as f32 / 255.0, bg.b() as f32 / 255.0],
            show_lines: self.show_nav || self.show_grid,
            world_texture: self.show_world,
            model: false,
            model_textures: false,
            model_inspection: 0,
            sun_azimuth: 2.4,
            sun_elevation: 0.75,
        };
        // the shader scales heights by the exaggeration: so does the orbit centre
        let mut cam = *cam;
        cam.target[1] *= self.exaggeration;
        if let Some(id) = g.render(size, &cam, &look) {
            ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        ui.painter().text(rect.left_bottom() + Vec2::new(8.0, -8.0), egui::Align2::LEFT_BOTTOM,
                          format!("drag: orbit · right-drag: pan · wheel: zoom · {}", g.adapter_name()),
                          egui::FontId::proportional(11.0), p.muted);
    }

    fn map(&mut self, ui: &mut egui::Ui, data: &Arc<TerrainData>) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let t = &data.terrain;
        let use_world = self.show_world && data.hill_world.is_some();
        let hkey = format!("{}|{use_world}", self.terrain_key);
        if self.hill_tex.as_ref().map(|h| &h.0) != Some(&hkey) {
            let src = if use_world { data.hill_world.as_ref().unwrap() } else { &data.hill };
            let img = egui::ColorImage::from_rgba_unmultiplied([src.width, src.height], &src.rgba);
            self.hill_tex = Some((hkey, ctx.load_texture("hillshade", img, egui::TextureOptions::LINEAR)));
        }
        if let Some(Ok(image)) = &self.nav_raster
            && self.nav_tex.as_ref().map(|h| &h.0) != Some(&self.nav_raster_key) {
            self.nav_tex = Some((self.nav_raster_key.clone(), image_texture(&ctx, "nav_overlay", image)));
        }
        if !self.density_task.loading() && let Some(Ok(data)) = &self.density_data
            && self.density_tex.as_ref().map(|h| h.0) != Some(self.density_task.generation) {
            self.density_tex = Some((self.density_task.generation, image_texture(&ctx, "density_overlay", &data.image)));
        }
        if let Some(Ok(image)) = &self.comparison_raster
            && self.comparison_tex.as_ref().map(|h| &h.0) != Some(&self.comparison_raster_key) {
            self.comparison_tex = Some((self.comparison_raster_key.clone(), image_texture(&ctx, "nav_difference", image)));
        }
        let aspect = (t.cols - 1) as f32 / (t.rows - 1) as f32;
        let content = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1000.0 * aspect, 1000.0));
        if self.map_rect == egui::Rect::ZERO {
            // open on the land, not the whole grid
            let (x0, z0, x1, z1) = t.extent();
            let f = data.focus;
            let to = |x: f32, z: f32| egui::pos2((x - x0) / (x1 - x0) * content.width(), (z1 - z) / (z1 - z0) * content.height());
            self.map_rect = egui::Rect::from_two_pos(to(f.0, f.1), to(f.2, f.3));
        }
        let mut rect = self.map_rect;
        let hill = self.hill_tex.as_ref().map(|h| h.1.id());
        let navt = if self.show_nav { self.nav_tex.as_ref().map(|h| h.1.id()) } else { None };
        let density_tex = if self.layers.show_density { self.density_tex.as_ref().map(|h| h.1.id()) } else { None };
        let comparison_tex = if self.layers.show_nav_diff { self.comparison_tex.as_ref().map(|h| h.1.id()) } else { None };
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let mut hover_world: Option<(f32, f32)> = None;
        let previous_size = self.nav_raster_size;
        let mut projected_size = None;
        egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(6).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            egui::Scene::new().zoom_range(0.2..=40.0).show(ui, &mut rect, |ui| {
                // Measure the full terrain, not its clipped visible subset. The
                // Scene transform already includes this frame's pan/zoom input.
                projected_size = NavRasterSize::for_scene_layer(&ctx, ui.layer_id(), content.size(), previous_size);
                let resp = ui.allocate_rect(content, Sense::hover());
                if let Some(h) = hill {
                    ui.painter().image(h, content, uv, Color32::WHITE);
                }
                if let Some(d) = density_tex { ui.painter().image(d, content, uv, Color32::WHITE); }
                if let Some(n) = navt { ui.painter().image(n, content, uv, Color32::WHITE); }
                if let Some(d) = comparison_tex { ui.painter().image(d, content, uv, Color32::WHITE); }
                ui.painter().rect_stroke(content, 0, egui::Stroke::new(1.0, p.muted), egui::StrokeKind::Outside);
                if let Some(pos) = resp.hover_pos() {
                    let (x0, z0, x1, z1) = t.extent();
                    let fx = (pos.x - content.min.x) / content.width();
                    let fy = (pos.y - content.min.y) / content.height();
                    hover_world = Some((x0 + fx * (x1 - x0), z1 - fy * (z1 - z0)));
                }
            });
        });
        self.map_rect = rect;
        if let Some(size) = projected_size {
            self.nav_raster_size = Some(size);
            self.ensure_map_raster(&ctx);
            self.ensure_comparison_raster(&ctx);
        }
        if let Some((x, z)) = hover_world {
            let hgt = t.height(x, z);
            egui::Area::new(egui::Id::new("map_coords")).fixed_pos(ui.max_rect().left_bottom() + Vec2::new(10.0, -28.0)).show(&ctx, |ui| {
                theme::pill(ui, &format!("x {x:.0}  z {z:.0}  height {hgt:.1} m"), p.info);
            });
        }
    }

    fn textures_ui(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let dirs: Vec<PathBuf> = env.project.map(|pr| pr.textures()).unwrap_or_default();
        let key = format!("{dirs:?}");
        if key != self.tex_list_key {
            self.tex_list_key = key;
            self.tex_list.clear();
            let d2 = dirs.clone();
            self.tex_list_task = Some(Task::spawn("Listing textures", &ctx, move |_| Ok(list_files(&d2, &["ftex", "dds", "png"], 20_000))));
        }
        if self.texture.is_none() && self.tex_task.is_none() && !s.preview_texture.is_empty() {
            self.open_texture(&ctx, PathBuf::from(&s.preview_texture));
        }
        egui::Panel::left("tex_list").resizable(true).default_size(320.0).min_size(200.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.tex_filter).hint_text("Filter").desired_width(ui.available_width() - 80.0));
                if ui.button("Open…").clicked() && let Some(f) = rfd::FileDialog::new().add_filter("textures", &["ftex", "dds", "png"]).pick_file() {
                    s.preview_texture = f.display().to_string();
                    self.open_texture(&ctx, f);
                }
            });
            if self.tex_list_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Listing…");
                });
            } else if dirs.is_empty() {
                ui.label(RichText::new("The project lists no texture folders ([preview] textures). Open a file instead.").small().color(p.muted));
            }
            let f = self.tex_filter.to_lowercase();
            let shown: Vec<&PathBuf> = self.tex_list.iter().filter(|x| f.is_empty() || x.to_string_lossy().to_lowercase().contains(&f)).collect();
            ui.label(RichText::new(format!("{} of {} files", shown.len(), self.tex_list.len())).small().color(p.muted));
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            let mut pick: Option<PathBuf> = None;
            egui::ScrollArea::vertical().id_salt("tex_scroll").auto_shrink([false, false]).show_rows(ui, row_h, shown.len(), |ui, range| {
                for i in range {
                    let path = shown[i];
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    let sel = s.preview_texture == path.display().to_string();
                    if ui.selectable_label(sel, RichText::new(name).monospace().size(12.0)).on_hover_text(path.display().to_string()).clicked() {
                        pick = Some(path.clone());
                    }
                }
            });
            if let Some(f) = pick {
                s.preview_texture = f.display().to_string();
                self.open_texture(&ctx, f);
            }
        });
        egui::CentralPanel::no_frame().show(ui, |ui| self.texture_pane(ui));
    }

    pub fn open_texture(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.texture = None;
        self.tex_handle = None;
        self.tex_task = Some(Task::spawn("Decoding texture", ctx, move |_| texture::load(&path, 16)));
    }

    fn texture_pane(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        if self.tex_task.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Decoding…");
            });
            return;
        }
        let tex = match &self.texture {
            None => {
                ui.label(RichText::new("Pick a texture (.ftex with its .ftexs, .dds, .png).").color(p.muted));
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.err, e);
                return;
            }
            Some(Ok(t)) => t.clone(),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tex.path.file_name().unwrap_or_default().to_string_lossy()).monospace().strong());
            theme::pill(ui, tex.kind, p.info);
            theme::pill(ui, &tex.format, p.accent);
            theme::pill(ui, &format!("{} x {}", tex.width, tex.height), p.muted);
            theme::pill(ui, &format!("{} level(s)", tex.levels.len()), p.muted);
        });
        for n in &tex.notes {
            ui.label(RichText::new(n).small().color(p.warn));
        }
        ui.horizontal(|ui| {
            ui.label("Channels");
            for (c, l) in [(Channels::Rgba, "RGBA"), (Channels::Rgb, "RGB"), (Channels::R, "R"), (Channels::G, "G"), (Channels::B, "B"), (Channels::A, "A")] {
                if ui.selectable_label(self.tex_channels == c, l).clicked() {
                    self.tex_channels = c;
                    self.tex_handle = None;
                }
            }
            ui.separator();
            if tex.levels.len() > 1 {
                ui.label("Mip");
                let mut l = self.tex_level as i32;
                if ui.add(egui::Slider::new(&mut l, 0..=(tex.levels.len() as i32 - 1))).changed() {
                    self.tex_level = l as usize;
                    self.tex_handle = None;
                }
            }
            ui.separator();
            if ui.button("Fit").clicked() {
                self.tex_zoom = 0.0;
            }
            if ui.button("1:1").clicked() {
                self.tex_zoom = 1.0;
            }
        });
        let lvl = self.tex_level.min(tex.levels.len() - 1);
        let img = &tex.levels[lvl];
        let key = format!("{}|{lvl}|{:?}", tex.path.display(), self.tex_channels);
        if self.tex_handle.as_ref().map(|h| &h.0) != Some(&key) {
            let px = texture::view(img, self.tex_channels);
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &px);
            self.tex_handle = Some((key, ctx.load_texture("preview_texture", ci, egui::TextureOptions::NEAREST)));
        }
        let h = self.tex_handle.as_ref().unwrap().1.clone();
        let avail = ui.available_size();
        let fit = (avail.x / img.width as f32).min(avail.y / img.height as f32).max(0.01);
        let z = if self.tex_zoom <= 0.0 { fit } else { self.tex_zoom };
        egui::ScrollArea::both().id_salt("tex_view").auto_shrink([false, false]).show(ui, |ui| {
            let r = ui.add(egui::Image::new(&h).fit_to_exact_size(Vec2::new(img.width as f32 * z, img.height as f32 * z)).sense(Sense::hover()));
            if r.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 && ui.input(|i| i.modifiers.ctrl) {
                    self.tex_zoom = (z * (1.0 + scroll * 0.002)).clamp(0.05, 32.0);
                }
            }
        });
    }

    fn models_ui(&mut self, ui: &mut egui::Ui, s: &mut Settings, env: &Env) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        let dirs: Vec<PathBuf> = env.project.map(|pr| pr.models()).unwrap_or_default();
        let mut options = model::LoadOptions {
            texture_roots: env.project.map(|pr| pr.textures()).unwrap_or_default(),
            name_files: vec![],
        };
        if !s.cache_dir.trim().is_empty() {
            options.texture_roots.push(PathBuf::from(s.cache_dir.trim()));
        }
        if !s.dict_file.trim().is_empty() {
            options.name_files.push(PathBuf::from(s.dict_file.trim()));
        }
        for rel in [
            "tools/mgsv-lookup-strings/fmdl/Dictionaries/fmdl_bonename_dictionary.txt",
            "tools/mgsv-lookup-strings/FmdlTool/fmdl_dictionary.txt",
        ] {
            let file = Path::new(&s.repo_root).join(rel);
            if file.is_file() {
                options.name_files.push(file);
            }
        }
        if options != self.model_options {
            self.model_options = options.clone();
            if let Some(path) = self.model_request.as_ref().map(|r| r.path.clone()) {
                self.open_model_with(&ctx, path, options);
            }
        }
        let key = format!("{dirs:?}");
        if key != self.model_list_key {
            self.model_list_key = key;
            self.model_list.clear();
            let d2 = dirs.clone();
            self.model_list_task = Some(Task::spawn_serial("Listing models", &ctx, move |_| {
                Ok(list_files(&d2, &["fmdl"], 20_000))
            }));
        }
        if self.model.is_none() && self.model_task.is_none() && !s.preview_model.is_empty() {
            self.open_model(&ctx, PathBuf::from(&s.preview_model));
        }
        egui::Panel::left("model_list")
            .resizable(true)
            .default_size(320.0)
            .min_size(200.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model_filter)
                            .hint_text("Filter")
                            .desired_width(ui.available_width() - 80.0),
                    );
                    if ui.button("Open…").clicked() && let Some(f) = rfd::FileDialog::new().add_filter("Fox model", &["fmdl"]).pick_file() {
                        s.preview_model = f.display().to_string();
                        self.open_model(&ctx, f);
                    }
                });
                if self.model_list_task.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Listing…");
                    });
                } else if dirs.is_empty() {
                    ui.label(
                        RichText::new("The project lists no model folders ([preview] models). Open a file instead.")
                            .small()
                            .color(p.muted),
                    );
                }
                let f = self.model_filter.to_lowercase();
                let shown: Vec<&PathBuf> = self
                    .model_list
                    .iter()
                    .filter(|x| f.is_empty() || x.to_string_lossy().to_lowercase().contains(&f))
                    .collect();
                ui.label(
                    RichText::new(format!("{} of {} files", shown.len(), self.model_list.len()))
                        .small()
                        .color(p.muted),
                );
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                let mut pick: Option<PathBuf> = None;
                egui::ScrollArea::vertical()
                    .id_salt("model_scroll")
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, shown.len(), |ui, range| {
                        for i in range {
                            let path = shown[i];
                            let name = path.file_name().unwrap_or_default().to_string_lossy();
                            let sel = s.preview_model == path.display().to_string();
                            if ui
                                .selectable_label(sel, RichText::new(name).monospace().size(12.0))
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                pick = Some(path.clone());
                            }
                        }
                    });
                if let Some(f) = pick {
                    s.preview_model = f.display().to_string();
                    self.open_model(&ctx, f);
                }
            });
        egui::CentralPanel::no_frame().show(ui, |ui| self.model_pane(ui, env));
    }

    /// Immediately remove displayed data, then coalesce repeated opens behind the one serial CPU worker.
    pub fn open_model(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.open_model_with(ctx, path, self.model_options.clone());
    }

    pub fn open_model_with(&mut self, ctx: &egui::Context, path: PathBuf, options: model::LoadOptions) {
        self.clear_model_display();
        self.model_request = Some(ModelRequest {
            generation: self.model_generation,
            path,
            options,
        });
        self.model_ctx = Some(ctx.clone());
        if self.model_task.is_none() {
            self.start_model(self.model_request.clone().unwrap(), ctx);
        }
    }

    fn start_model(&mut self, request: ModelRequest, ctx: &egui::Context) {
        let load = request.clone();
        self.model_task = Some((
            request,
            Task::spawn_serial("Reading model and textures", ctx, move |_| {
                model::load_with(&load.path, &load.options).map(Arc::new)
            }),
        ));
    }

    fn clear_model_display(&mut self) {
        self.model = None;
        self.model_cam = None;
        self.model_rect = egui::Rect::ZERO;
        self.model_mesh = None;
        self.model_bone = None;
        self.model_gpu_error = None;
        self.model_gpu_for.clear();
        self.model_lines_for.clear();
        self.model_generation = self.model_generation.wrapping_add(1);
    }

    /// Current rendered model camera/viewport, for inspection and coordinate-aware proof.
    pub fn model_view(&self) -> Option<(Camera, egui::Rect)> {
        self.model_cam
            .filter(|_| self.model_rect.is_positive())
            .map(|c| (c, self.model_rect))
    }

    /// Project switches invalidate the selected model even while Previews is hidden.
    pub fn reset_models(&mut self) {
        self.model_request = None;
        self.model_options = model::LoadOptions::default();
        self.model_list_key.clear();
        self.model_list.clear();
        self.model_list_task = None;
        self.clear_model_display();
        self.model_gpu = None;
    }

    fn model_pane(&mut self, ui: &mut egui::Ui, env: &Env) {
        let p = pal(ui);
        if self.model_task.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading model and textures…");
            });
            if let Some(r) = &self.model_request {
                ui.label(RichText::new(r.path.display().to_string()).monospace().small());
            }
            return;
        }
        let m = match &self.model {
            None => {
                ui.label(RichText::new("Pick a model (.fmdl).").color(p.muted));
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(p.err, e);
                return;
            }
            Some(Ok(m)) => m.clone(),
        };
        let i = &m.info;
        let size = std::array::from_fn::<_, 3, _>(|k| i.bbox.1[k] - i.bbox.0[k]);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(Path::new(&i.path).file_name().unwrap_or_default().to_string_lossy())
                    .monospace()
                    .strong(),
            )
            .on_hover_text(&i.path);
            theme::pill(ui, &format!("FMDL {:.2}", i.version), p.info);
            theme::pill(ui, &format!("{} meshes · {} bones", i.meshes, m.bones.len()), p.accent);
            theme::pill(
                ui,
                &format!("{} vertices · {} triangles", i.vertices, i.triangles),
                p.muted,
            );
            theme::pill(
                ui,
                &format!("{:.2} x {:.2} x {:.2} m", size[0], size[1], size[2]),
                p.muted,
            );
        });
        ui.label(RichText::new("Pose: Bind pose (no animation loaded)").strong().color(p.warn))
            .on_hover_text("Raw FMDL vertices and stored bind-position fields. Skinning, actor MOVE and cutscene animation are not applied.");
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.model_textures, "Albedo").on_hover_text("Source base colour only; Fox normal/specular/emissive shaders are not reproduced.");
            ui.checkbox(&mut self.model_wire, "Wireframe");
            ui.checkbox(&mut self.model_normals, "Normal vectors").on_hover_text("At most 4,000 vectors; source normals where supported, explicitly labelled geometry fallback otherwise.");
            ui.add_enabled_ui(!m.bones.is_empty(), |ui| {
                ui.checkbox(&mut self.model_bones, "Bind bones").on_hover_text("Source world bind positions, shown through the mesh. No animation or IK.");
            });
            egui::ComboBox::from_id_salt("model_surface").selected_text(match self.model_inspection { 1 => "UV checker", 2 => "Normal colours", _ => "Shaded" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.model_inspection, 0, "Shaded");
                ui.selectable_value(&mut self.model_inspection, 1, "UV checker");
                ui.selectable_value(&mut self.model_inspection, 2, "Normal colours");
            });
            if ui.button("Frame model").clicked() { self.model_cam = None; }
            for (label, yaw, pitch) in [("Front", 0.0, 0.0), ("Side", std::f32::consts::FRAC_PI_2, 0.0), ("Top", 0.0, 1.48)] {
                if ui.button(label).clicked() {
                    let cam = self.model_cam.get_or_insert(framed_camera(&m, self.model_mesh));
                    cam.yaw = yaw; cam.pitch = pitch;
                }
            }
        });
        egui::CollapsingHeader::new("Mesh, material and bone inspection")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("model_mesh")
                        .selected_text(self.model_mesh.map_or("All meshes".into(), |id| format!("Mesh {id}")))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.model_mesh, None, "All meshes");
                            for mesh in &m.meshes {
                                ui.selectable_value(
                                    &mut self.model_mesh,
                                    Some(mesh.id),
                                    format!("Mesh {} · material {}", mesh.id, mesh.material),
                                );
                            }
                        });
                    if ui.button("Frame selection").clicked() {
                        self.model_cam = Some(framed_camera(&m, self.model_mesh));
                    }
                    if self.model_bones {
                        if ui.button("Frame bind bones").clicked() {
                            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                            for bone in &m.bones {
                                for k in 0..3 {
                                    lo[k] = lo[k].min(bone.world[k]);
                                    hi[k] = hi[k].max(bone.world[k]);
                                }
                            }
                            let diag = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f32>().sqrt().max(0.01);
                            self.model_cam = Some(Camera {
                                target: std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0),
                                yaw: 0.7,
                                pitch: 0.35,
                                distance: diag * 1.6,
                                fov_y: 0.75,
                            });
                        }
                        egui::ComboBox::from_id_salt("model_bone")
                            .selected_text(self.model_bone.map_or("All bones".into(), |id| format!("Bone {id}")))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.model_bone, None, "All bones");
                                for bone in &m.bones {
                                    ui.selectable_value(
                                        &mut self.model_bone,
                                        Some(bone.id),
                                        format!("{} · {}", bone.id, bone.name),
                                    );
                                }
                            });
                    }
                });
                if let Some(mesh) = m.meshes.iter().find(|x| Some(x.id) == self.model_mesh) {
                    ui.label(format!(
                        "Mesh {} · {} triangles · UV0 {} · {} normals · source alpha {:#04x}",
                        mesh.id,
                        (mesh.indices.end - mesh.indices.start) / 3,
                        if mesh.has_uv {
                            "available"
                        } else {
                            "missing/unsupported"
                        },
                        if mesh.source_normals {
                            "source"
                        } else {
                            "geometry fallback"
                        },
                        mesh.alpha_mode
                    ));
                    if let Some(mat) = m.materials.get(mesh.material) {
                        ui.label(format!("Material {} · {}", mat.id, mat.status));
                        if let Some(path) = &mat.path {
                            ui.add(
                                egui::Label::new(RichText::new(path.display().to_string()).monospace().size(12.0))
                                    .selectable(true)
                                    .wrap(),
                            );
                        }
                    }
                } else {
                    let loaded = m.materials.iter().filter(|m| m.image.is_some()).count();
                    ui.label(format!(
                        "{loaded}/{} materials have resolved albedo. Unresolved surfaces use neutral shading.",
                        m.materials.len()
                    ));
                }
                if let Some(bone) = self.model_bone.and_then(|id| m.bones.get(id)) {
                    ui.label(format!(
                        "Bone {} · {} · parent {}",
                        bone.id,
                        bone.name,
                        bone.parent.map_or("root".into(), |p| p.to_string())
                    ));
                    ui.monospace(format!("Local {:.3?} · world bind {:.3?}", bone.local, bone.world));
                }
                if !m.notes.is_empty() {
                    egui::CollapsingHeader::new(format!("{} inspection notes", m.notes.len())).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            for note in &m.notes {
                                ui.colored_label(p.warn, note);
                            }
                        });
                    });
                }
            });
        ui.label(
            RichText::new(
                "Read-only albedo inspection · source bind pose · Fox lighting and animation are not reproduced",
            )
            .small()
            .color(p.muted),
        );
        let Some(rs) = env.wgpu else {
            ui.label(
                RichText::new(
                    "No GPU device in this session: model data and controls are available; the 3-D view needs wgpu.",
                )
                .color(p.muted),
            );
            return;
        };
        if let Some(game) = &env.game_running {
            ui.colored_label(
                p.warn,
                format!("{game} is running: the 3-D view is paused (no GPU work)."),
            );
            return;
        }
        if let Some(e) = &self.model_gpu_error {
            ui.colored_label(p.err, e);
            return;
        }
        if self.model_gpu.is_none() {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| TerrainGpu::new(rs))) {
                Ok(g) => self.model_gpu = Some(g),
                Err(_) => {
                    self.model_gpu_error = Some("Could not create the model pipeline on this GPU.".into());
                    return;
                }
            }
        }
        let g = self.model_gpu.as_mut().unwrap();
        let key = self.model_generation.to_string();
        if self.model_gpu_for != key {
            g.set_model(&m);
            self.model_gpu_for = key;
        }
        g.set_visible_mesh(self.model_mesh);
        let key = format!(
            "{}|{}|{}|{}|{:?}|{:?}",
            self.model_generation,
            self.model_wire,
            self.model_normals,
            self.model_bones,
            self.model_mesh,
            self.model_bone
        );
        if self.model_lines_for != key {
            let lines = model_surface_lines(&m, self.model_mesh, self.model_wire, self.model_normals);
            g.set_lines(&lines);
            let bones = if self.model_bones {
                m.bone_lines(self.model_bone)
            } else {
                vec![]
            };
            g.set_bone_lines(&bones);
            self.model_lines_for = key;
        }
        let diag = size.iter().map(|x| x * x).sum::<f32>().sqrt().max(0.01);
        let cam = self.model_cam.get_or_insert(framed_camera(&m, None));
        let avail = ui.available_size().max(Vec2::splat(64.0));
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        self.model_rect = rect;
        let d = resp.drag_delta();
        if resp.dragged_by(egui::PointerButton::Primary) {
            cam.yaw -= d.x * 0.008;
            cam.pitch = (cam.pitch + d.y * 0.008).clamp(-1.5, 1.5);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let scale = 2.0 * cam.distance * (cam.fov_y / 2.0).tan() / rect.height();
            let right = [cam.yaw.cos(), 0.0, -cam.yaw.sin()];
            let up = [
                -cam.pitch.sin() * cam.yaw.sin(),
                cam.pitch.cos(),
                -cam.pitch.sin() * cam.yaw.cos(),
            ];
            for k in 0..3 {
                cam.target[k] += (-right[k] * d.x + up[k] * d.y) * scale;
            }
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                cam.distance = (cam.distance * (-scroll * 0.0015).exp()).clamp(diag * 0.02, diag * 20.0);
            }
        }
        let ppp = ui.ctx().pixels_per_point();
        let limit = if g.is_software() { 2048 } else { 4096 };
        let px = [
            (rect.width() * ppp).round() as u32,
            (rect.height() * ppp).round() as u32,
        ]
        .map(|n| n.min(limit));
        let bg = ui.visuals().extreme_bg_color;
        let look = Look {
            hmin: i.bbox.0[1],
            hmax: i.bbox.1[1],
            water: None,
            exaggeration: 1.0,
            background: [bg.r() as f32 / 255.0, bg.g() as f32 / 255.0, bg.b() as f32 / 255.0],
            show_lines: self.model_wire || self.model_normals,
            world_texture: false,
            model: true,
            model_textures: self.model_textures,
            model_inspection: self.model_inspection,
            sun_azimuth: 2.4,
            sun_elevation: 0.75,
        };
        if let Some(id) = g.render(px, cam, &look) {
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        ui.painter().text(
            rect.left_bottom() + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "left drag: orbit · right/middle drag: pan · wheel: zoom",
            egui::FontId::proportional(11.0),
            p.muted,
        );
    }

}

fn framed_camera(model: &model::Model, mesh: Option<usize>) -> Camera {
    let bbox = mesh
        .and_then(|id| model.meshes.iter().find(|x| x.id == id))
        .map(|mesh| {
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in &model.verts[mesh.vertices.clone()] {
                for k in 0..3 {
                    lo[k] = lo[k].min(v.pos[k]);
                    hi[k] = hi[k].max(v.pos[k]);
                }
            }
            (lo, hi)
        })
        .unwrap_or(model.info.bbox);
    let diag = (0..3)
        .map(|k| (bbox.1[k] - bbox.0[k]).powi(2))
        .sum::<f32>()
        .sqrt()
        .max(0.01);
    Camera {
        target: std::array::from_fn(|k| (bbox.0[k] + bbox.1[k]) / 2.0),
        yaw: 0.7,
        pitch: 0.35,
        distance: diag * 1.6,
        fov_y: 0.75,
    }
}

fn model_surface_lines(model: &model::Model, mesh: Option<usize>, wire: bool, normals: bool) -> Vec<LineVertex> {
    let mut lines = Vec::new();
    let selected = mesh.and_then(|id| model.meshes.iter().find(|x| x.id == id));
    if wire {
        if let Some(mesh) = selected {
            let mut seen = std::collections::HashSet::new();
            for tri in model.indices[mesh.indices.start as usize..mesh.indices.end as usize].chunks_exact(3) {
                for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                    if lines.len() < model::MAX_EDGES * 2 && seen.insert((a.min(b), a.max(b))) {
                        for id in [a, b] {
                            lines.push(LineVertex {
                                pos: model.verts[id as usize].pos,
                                color: [0.1, 0.65, 1.0, 0.55],
                            });
                        }
                    }
                }
            }
        } else {
            lines.extend_from_slice(&model.edges);
        }
    }
    if normals {
        if let Some(mesh) = selected {
            let verts = &model.verts[mesh.vertices.clone()];
            let diag = (0..3)
                .map(|k| (model.info.bbox.1[k] - model.info.bbox.0[k]).powi(2))
                .sum::<f32>()
                .sqrt()
                .max(0.01);
            for v in verts.iter().step_by(verts.len().div_ceil(model::MAX_NORMALS).max(1)) {
                for pos in [v.pos, std::array::from_fn(|k| v.pos[k] + v.nrm[k] * diag * 0.018)] {
                    lines.push(LineVertex {
                        pos,
                        color: [0.35, 1.0, 0.4, 0.9],
                    });
                }
            }
        } else {
            lines.extend_from_slice(&model.normals);
        }
    }
    lines
}

#[cfg(test)]
mod model_target_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn stub(path: &str) -> Arc<model::Model> {
        Arc::new(model::Model {
            info: model::ModelInfo {
                path: path.into(),
                bytes: 0,
                version: 2.04,
                meshes: 0,
                vertices: 0,
                triangles: 0,
                bbox: ([0.0; 3], [1.0; 3]),
            },
            verts: vec![],
            indices: vec![],
            meshes: vec![],
            materials: vec![],
            bones: vec![],
            edges: vec![],
            normals: vec![],
            notes: vec![],
        })
    }
    fn drain(v: &mut PreviewView) {
        let start = Instant::now();
        while v.model_task.is_some() {
            v.poll();
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn model_target_late_success_is_discarded_and_latest_selection_is_coalesced() {
        let ctx = egui::Context::default();
        let mut v = PreviewView::default();
        let old = ModelRequest {
            generation: 0,
            path: "old.fmdl".into(),
            options: Default::default(),
        };
        let (tx, rx) = mpsc::channel();
        v.model_request = Some(old.clone());
        v.model_task = Some((
            old,
            Task::spawn_serial("old model", &ctx, move |_| {
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(stub("old"))
            }),
        ));
        v.open_model(&ctx, "missing-middle.fmdl".into());
        v.open_model(&ctx, "missing-latest.fmdl".into());
        assert_eq!(v.model_task.as_ref().unwrap().0.path, PathBuf::from("old.fmdl"));
        tx.send(()).unwrap();
        drain(&mut v);
        let e = v.model.as_ref().unwrap().as_ref().unwrap_err();
        assert!(e.contains("missing-latest"), "{e}");
        assert!(!e.contains("middle"));
    }

    #[test]
    fn model_target_same_path_reload_rejects_earlier_snapshot() {
        let ctx = egui::Context::default();
        let mut v = PreviewView::default();
        let old = ModelRequest {
            generation: 0,
            path: "same-missing.fmdl".into(),
            options: Default::default(),
        };
        let (tx, rx) = mpsc::channel();
        v.model_request = Some(old.clone());
        v.model_task = Some((
            old,
            Task::spawn_serial("old snapshot", &ctx, move |_| {
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Ok(stub("obsolete same path"))
            }),
        ));
        v.open_model(&ctx, "same-missing.fmdl".into());
        tx.send(()).unwrap();
        drain(&mut v);
        assert!(
            v.model.as_ref().unwrap().is_err(),
            "old successful snapshot must not appear after reload"
        );
    }

    #[test]
    fn model_target_reset_while_loading_discards_late_error() {
        let ctx = egui::Context::default();
        let mut v = PreviewView::default();
        let old = ModelRequest {
            generation: 0,
            path: "old.fmdl".into(),
            options: Default::default(),
        };
        let (tx, rx) = mpsc::channel();
        v.model_request = Some(old.clone());
        v.model_task = Some((
            old,
            Task::spawn_serial("old error", &ctx, move |_| {
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
                Err("obsolete error".into())
            }),
        ));
        v.reset_models();
        tx.send(()).unwrap();
        drain(&mut v);
        assert!(v.model.is_none());
        assert!(v.model_request.is_none());
    }
}


#[cfg(test)]
mod layer_target_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn wait<T: Send + 'static>(worker: &mut SerialWorker<T>) -> Option<Result<T, String>> {
        let start = Instant::now();
        let mut result = None;
        while worker.busy() {
            if let Some(r) = worker.poll() {
                result = Some(r);
            }
            assert!(start.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(1));
        }
        result
    }

    #[test]
    fn serial_worker_coalesces_replacements_and_cancels_obsolete_work() {
        let ctx = egui::Context::default();
        let mut worker = SerialWorker::<usize>::default();
        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        worker.request(&ctx, "old", move |_, cancel| {
            start_tx.send(()).unwrap();
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(cancel.load(Ordering::Relaxed));
            Ok(1)
        });
        start_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (middle_tx, middle_rx) = mpsc::channel();
        worker.request(&ctx, "middle", move |_, _| {
            middle_tx.send(()).unwrap();
            Ok(2)
        });
        worker.request(&ctx, "latest", |_, _| Ok(3));
        // No replacement starts until the old worker actually ends.
        assert!(middle_rx.try_recv().is_err());
        assert!(worker.pending.is_some());
        done_tx.send(()).unwrap();
        assert_eq!(wait(&mut worker).unwrap().unwrap(), 3);
        assert!(middle_rx.try_recv().is_err());
    }

    #[test]
    fn serial_worker_cancel_discards_late_error_and_same_path_success() {
        for old in [Ok(1), Err("old error".to_string())] {
            let mut worker = SerialWorker::<usize>::default();
            let ctx = egui::Context::default();
            let (tx, rx) = mpsc::channel();
            worker.request(&ctx, "same path", move |_, _| {
                rx.recv().unwrap();
                old
            });
            worker.cancel();
            assert!(!worker.loading());
            tx.send(()).unwrap();
            assert!(wait(&mut worker).is_none());
        }
    }

    #[test]
    fn serial_worker_same_request_reloads_a_new_generation() {
        let mut worker = SerialWorker::<usize>::default();
        let ctx = egui::Context::default();
        let (tx, rx) = mpsc::channel();
        worker.request(&ctx, "same path", move |_, _| {
            rx.recv().unwrap();
            Ok(1)
        });
        let first = worker.generation;
        worker.request(&ctx, "same path", |_, _| Ok(2));
        assert_ne!(first, worker.generation);
        tx.send(()).unwrap();
        assert_eq!(wait(&mut worker).unwrap().unwrap(), 2);
    }

    fn seed(view: &mut PreviewView, ctx: &egui::Context) {
        let terrain = Terrain::new(2, 2, vec![0.0; 4], 10.0, 0.0, 0.0).unwrap();
        let hill = Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        let grid = Arc::new(
            density::from_positions(
                &terrain,
                &[[0.0; 3]],
                Default::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
        );
        view.terrain = Some(Ok(Arc::new(TerrainData {
            terrain,
            hill: hill.clone(),
            verts: vec![],
            indices: vec![],
            path: "heights.npy".into(),
            focus: (0.0, 0.0, 10.0, 10.0),
            world: None,
            hill_world: None,
            world_notes: vec![],
        })));
        view.nav = Some(Ok(Arc::new(NavOverlay::default())));
        view.density_data = Some(Ok(DensityMap {
            path: "array.npy".into(),
            grid,
            image: hill.clone(),
        }));
        view.density_style = Some(DensityStyle::from_controls(&view.layers));
        view.density_tex = Some((0, image_texture(ctx, "old", &hill)));
        ctx.tex_manager().write().take_delta().clear();
    }

    #[test]
    fn density_recolour_uses_cached_grid_and_hides_old_texture_immediately() {
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        let grid = match &view.density_data {
            Some(Ok(data)) => data.grid.clone(),
            _ => unreachable!(),
        };
        view.layers.opacity = 0;
        view.ensure_density_style(&ctx);
        assert!(view.density_tex.is_none());
        assert!(view.density_task.loading());
        let result = wait(&mut view.density_task).unwrap().unwrap();
        assert!(Arc::ptr_eq(&grid, &result.grid));
        assert!(result.image.rgba.iter().skip(3).step_by(4).all(|&a| a == 0));
        // array.npy does not exist: recolouring must not reread it.
        assert_eq!(result.path, PathBuf::from("array.npy"));
    }

    #[test]
    fn dataset_change_while_loading_discards_old_terrain_and_all_map_state() {
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        let old = view.terrain.take().unwrap().unwrap();
        let (tx, rx) = mpsc::channel();
        view.terrain_task
            .request(&ctx, "old terrain", move |_, cancel| {
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(cancel.load(Ordering::Relaxed));
                Ok(old)
            });
        view.layers.density_path = "old.npy".into();
        view.synchronize_dataset(Path::new("new-tool-root"), None);
        assert!(view.terrain.is_none() && view.nav.is_none());
        assert!(view.density_data.is_none() && view.comparison_data.is_none());
        assert!(view.density_tex.is_none() && view.nav_tex.is_none());
        assert!(view.layers.density_path.is_empty());
        tx.send(()).unwrap();
        assert!(wait(&mut view.terrain_task).is_none());
    }

    #[test]
    fn terrain_nav_lines_invalidate_when_a_pending_result_arrives() {
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        view.nav = None;
        let pending = view.terrain_lines_key();
        view.nav = Some(Ok(Arc::new(NavOverlay::default())));
        let ready = view.terrain_lines_key();
        assert_ne!(
            pending, ready,
            "request generation alone misses result arrival"
        );
        view.nav_task.cancel();
        assert_ne!(
            ready,
            view.terrain_lines_key(),
            "same-size reload needs a new key"
        );
    }

    #[test]
    fn nav_raster_follows_full_projected_span_dpi_and_zoom_with_a_bounded_aspect() {
        let size = |span, dpi| NavRasterSize::for_display(span, dpi, None).unwrap();
        assert_eq!(
            size(Vec2::splat(420.0), 1.0),
            NavRasterSize {
                width: 384,
                height: 384
            }
        );
        assert_eq!(
            size(Vec2::splat(420.0), 2.0),
            NavRasterSize {
                width: 832,
                height: 832
            }
        );
        // A zoomed full terrain can extend well beyond the visible pane.
        assert_eq!(
            size(Vec2::new(840.0, 420.0), 1.0),
            NavRasterSize {
                width: 832,
                height: 416
            }
        );
        assert_eq!(
            size(Vec2::new(10000.0, 2500.0), 2.0),
            NavRasterSize {
                width: 2048,
                height: 512
            }
        );
        assert_eq!(
            size(Vec2::splat(19.0), 1.0),
            NavRasterSize {
                width: 16,
                height: 16
            }
        );
    }

    #[test]
    fn nav_raster_hysteresis_avoids_bucket_jitter_and_bounds_minification() {
        let first = NavRasterSize::for_display(Vec2::splat(420.0), 1.0, None).unwrap();
        for side in [419.0, 420.5, 448.0, 451.5, 385.0, 346.0] {
            assert_eq!(
                NavRasterSize::for_display(Vec2::splat(side), 1.0, Some(first)),
                Some(first)
            );
            assert!(first.width as f32 / side < 1.12);
        }
        assert_eq!(
            NavRasterSize::for_display(Vec2::splat(345.0), 1.0, Some(first))
                .unwrap()
                .width,
            320
        );
        assert_eq!(
            NavRasterSize::for_display(Vec2::splat(493.0), 1.0, Some(first))
                .unwrap()
                .width,
            448
        );
        let cap = NavRasterSize {
            width: 2048,
            height: 2048,
        };
        assert_eq!(
            NavRasterSize::for_display(Vec2::splat(100000.0), 1.0, Some(cap)),
            Some(cap)
        );
    }

    #[test]
    fn hidden_or_degenerate_map_does_not_request_invalid_raster_sizes() {
        for span in [
            Vec2::ZERO,
            Vec2::new(1.0, 0.0),
            Vec2::splat(-1.0),
            Vec2::splat(f32::NAN),
            Vec2::splat(f32::INFINITY),
        ] {
            assert!(NavRasterSize::for_display(span, 1.0, None).is_none());
        }
        for dpi in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(NavRasterSize::for_display(Vec2::splat(420.0), dpi, None).is_none());
        }
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        view.ensure_map_raster(&ctx);
        assert!(!view.nav_raster_task.busy());
    }

    #[test]
    fn resize_cancels_old_base_and_diff_rasters_and_reuses_cached_geometry() {
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        let nav = Arc::new(NavOverlay {
            edges: vec![
                ([1.0, 0.0, 1.0], [9.0, 0.0, 1.0], true),
                ([9.0, 0.0, 1.0], [1.0, 0.0, 9.0], true),
                ([1.0, 0.0, 9.0], [1.0, 0.0, 1.0], true),
            ],
            ..Default::default()
        });
        let comparison = Arc::new(Comparison::Ready(
            navdiff::compare(
                &nav,
                &NavOverlay::default(),
                Default::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
        ));
        view.nav = Some(Ok(nav.clone()));
        view.comparison_data = Some(Ok(comparison.clone()));
        // There are no baseline files: resizing must only read the cached diff.
        view.layers.baseline_paths = "nonexistent-baseline.nav2".into();
        let density_grid = view
            .density_data
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .grid
            .clone();
        let density_image = view
            .density_data
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .image
            .rgba
            .clone();
        let (base_tx, base_rx) = mpsc::channel();
        let (diff_tx, diff_rx) = mpsc::channel();
        view.nav_raster_task
            .request(&ctx, "obsolete base size", move |_, cancel| {
                base_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(cancel.load(Ordering::Relaxed));
                Ok(Image {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                })
            });
        view.comparison_raster_task
            .request(&ctx, "obsolete diff size", move |_, cancel| {
                diff_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(cancel.load(Ordering::Relaxed));
                Err("obsolete resize failure".into())
            });
        view.nav_tex = Some((
            "obsolete".into(),
            image_texture(
                &ctx,
                "obsolete-base",
                &Image {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                },
            ),
        ));
        view.comparison_tex = Some((
            "obsolete".into(),
            image_texture(
                &ctx,
                "obsolete-diff",
                &Image {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                },
            ),
        ));
        view.nav_raster_size = Some(NavRasterSize {
            width: 384,
            height: 384,
        });
        view.ensure_map_raster(&ctx);
        view.ensure_comparison_raster(&ctx);
        assert!(view.nav_tex.is_none() && view.comparison_tex.is_none());
        let base_generation = view.nav_raster_task.generation;
        let diff_generation = view.comparison_raster_task.generation;
        let geometry_generation = view.comparison_task.generation;
        // A further resize coalesces the first pending raster, but not geometry.
        view.nav_raster_size = Some(NavRasterSize {
            width: 128,
            height: 128,
        });
        view.ensure_map_raster(&ctx);
        view.ensure_comparison_raster(&ctx);
        assert_ne!(view.nav_raster_task.generation, base_generation);
        assert_ne!(view.comparison_raster_task.generation, diff_generation);
        assert_eq!(view.comparison_task.generation, geometry_generation);
        base_tx.send(()).unwrap();
        diff_tx.send(()).unwrap();
        let base = wait(&mut view.nav_raster_task).unwrap().unwrap();
        let diff = wait(&mut view.comparison_raster_task).unwrap().unwrap();
        assert_eq!((base.width, base.height), (128, 128));
        assert_eq!((diff.width, diff.height), (128, 128));
        assert!(base.rgba.chunks_exact(4).any(|p| p[3] > 0));
        assert!(diff.rgba.chunks_exact(4).any(|p| p[3] > 0));
        assert!(Arc::ptr_eq(
            view.nav.as_ref().unwrap().as_ref().unwrap(),
            &nav
        ));
        assert!(Arc::ptr_eq(
            view.comparison_data.as_ref().unwrap().as_ref().unwrap(),
            &comparison
        ));
        let density = view.density_data.as_ref().unwrap().as_ref().unwrap();
        assert!(Arc::ptr_eq(&density.grid, &density_grid));
        assert_eq!(density.image.rgba, density_image);
        // Stable accepted dimensions produce no additional worker generations.
        let generations = (
            view.nav_raster_task.generation,
            view.comparison_raster_task.generation,
        );
        view.ensure_map_raster(&ctx);
        view.ensure_comparison_raster(&ctx);
        assert_eq!(
            (
                view.nav_raster_task.generation,
                view.comparison_raster_task.generation
            ),
            generations
        );
        view.invalidate_terrain();
        assert!(view.nav_raster_size.is_none() && view.comparison_raster.is_none());
        assert!(view.comparison_data.is_none() && view.comparison_tex.is_none());
    }

    #[test]
    fn comparison_redraw_stays_loading_and_cancel_discards_its_late_result() {
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        view.comparison_data = Some(Ok(Arc::new(Comparison::Ready(
            navdiff::compare(
                &NavOverlay::default(),
                &NavOverlay::default(),
                Default::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
        ))));
        let (tx, rx) = mpsc::channel();
        view.comparison_raster_task
            .request(&ctx, "comparison resize", move |task, cancel| {
                task.progress(0.25, "drawing cached edges");
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(cancel.load(Ordering::Relaxed));
                Ok(Image {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                })
            });
        assert!(matches!(
            view.layer_status().comparison,
            LayerState::Loading { .. }
        ));
        view.apply_layer_action(&ctx, LayerAction::CancelNav);
        assert!(
            matches!(view.layer_status().comparison, LayerState::Error(e) if e.contains("cancelled"))
        );
        tx.send(()).unwrap();
        assert!(wait(&mut view.comparison_raster_task).is_none());
        assert!(view.comparison_raster.is_none() && view.comparison_tex.is_none());
    }

    #[test]
    fn nav_raster_completion_during_pointer_click_keeps_density_button_stable() {
        use egui_kittest::{Harness, kittest::Queryable};

        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        view.density_data = None;
        view.density_tex = None;
        view.layers.density_path = "placements.npy".into();
        view.layer_root = Some(std::env::temp_dir().join(format!(
            "foxstudio_nav_click_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        assert!(!view.layer_root.as_ref().unwrap().exists());
        let mut harness = Harness::builder()
            .with_size([1400.0, 1000.0])
            .build_ui_state(
                |ui, view: &mut PreviewView| {
                    view.poll();
                    view.layer_controls(ui);
                },
                view,
            );
        harness.get_by_label("Density and navmesh comparison").click();
        harness.run_steps(3);

        let (release, held) = mpsc::channel();
        harness
            .state_mut()
            .nav_raster_task
            .request(&ctx, "controlled nav raster", move |_, _| {
                held.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(Image {
                    width: 1,
                    height: 1,
                    rgba: vec![0; 4],
                })
            });
        harness.step();
        let before = harness.get_by_label("Load density").rect();
        let center = before.center();
        let generation = harness.state().density_task.generation;
        harness.event(egui::Event::PointerMoved(center));
        harness.step();
        harness.event(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();

        // Complete the worker while the button is held. Keep its result pending
        // in SerialWorker so the release frame adopts it before drawing controls.
        release.send(()).unwrap();
        let started = Instant::now();
        while !harness
            .state_mut()
            .nav_raster_task
            .active
            .as_mut()
            .unwrap()
            .1
            .poll()
        {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "Nav raster worker stalled"
            );
            std::thread::yield_now();
        }
        assert!(harness.state().nav_raster_task.loading());
        harness.event(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();

        assert_eq!(
            before,
            harness.get_by_label("Load density").rect(),
            "Nav raster completion moved a control during a pointer click"
        );
        assert!(
            harness.state().density_task.generation > generation,
            "Density click was not dispatched"
        );
        assert!(harness.state().density_reading);
        assert!(matches!(
            harness.state().layer_status().density,
            LayerState::Loading { .. }
        ));
        // Only dispatch is under test; the unique nonexistent input cannot read
        // a user's assets. Drain the resulting file error before dropping state.
        let _ = wait(&mut harness.state_mut().density_task);
    }

    #[test]
    fn redraw_controls_keep_cached_legend_footprint_and_real_cancel_button() {
        use egui_kittest::{Harness, kittest::Queryable};
        let ctx = egui::Context::default();
        let mut view = PreviewView::default();
        seed(&mut view, &ctx);
        view.density_data = None;
        view.density_tex = None;
        view.comparison_data = Some(Ok(Arc::new(Comparison::Ready(
            navdiff::compare(
                &NavOverlay::default(),
                &NavOverlay::default(),
                Default::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
        ))));
        let mut harness = Harness::builder()
            .with_size([1000.0, 1000.0])
            .build_ui_state(
                |ui, state: &mut (PreviewView, f32)| {
                    let start = ui.cursor().top();
                    state.0.layer_controls(ui);
                    state.1 = ui.cursor().top() - start;
                },
                (view, 0.0),
            );
        harness
            .get_by_label("Density and navmesh comparison")
            .click();
        harness.run_steps(6);
        let ready_height = harness.state().1;
        let (tx, rx) = mpsc::channel();
        harness
            .state_mut()
            .0
            .comparison_raster_task
            .request(&ctx, "resize", move |_, cancel| {
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
                assert!(cancel.load(Ordering::Relaxed));
                Ok(Image {
                    width: 1,
                    height: 1,
                    rgba: vec![255; 4],
                })
            });
        harness.run_steps(3);
        assert!(matches!(
            harness.state().0.layer_status().comparison,
            LayerState::Loading { .. }
        ));
        harness.get_by_label(
            "0 added \u{00b7} 0 removed \u{00b7} 0 boundary changes \u{00b7} 0 unchanged",
        );
        assert!((harness.state().1 - ready_height).abs() < 0.25);
        harness.get_by_label("Cancel nav comparison").scroll_to_me();
        harness.run_steps(3);
        harness.get_by_label("Cancel nav comparison").click();
        harness.run_steps(2);
        assert!(
            matches!(harness.state().0.layer_status().comparison, LayerState::Error(e) if e.contains("cancelled"))
        );
        tx.send(()).unwrap();
        assert!(wait(&mut harness.state_mut().0.comparison_raster_task).is_none());
    }

    #[test]
    fn scene_identity_transform_and_changed_aspect_cannot_suppress_or_distort_nav() {
        let ctx = egui::Context::default();
        let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("nav-scene"));
        ctx.set_transform_layer(layer, egui::emath::TSTransform::IDENTITY);
        assert!(ctx.layer_transform_to_global(layer).is_none());
        let size = NavRasterSize::for_scene_layer(&ctx, layer, Vec2::splat(420.0), None).unwrap();
        assert_eq!(
            size,
            NavRasterSize {
                width: 384,
                height: 384
            }
        );
        ctx.set_transform_layer(layer, egui::emath::TSTransform::from_scaling(2.0));
        assert_eq!(
            NavRasterSize::for_scene_layer(&ctx, layer, Vec2::splat(420.0), Some(size)).unwrap(),
            NavRasterSize {
                width: 832,
                height: 832
            }
        );
        assert_eq!(
            NavRasterSize::for_display(Vec2::new(420.0, 210.0), 1.0, Some(size)).unwrap(),
            NavRasterSize {
                width: 384,
                height: 192
            }
        );
    }

    #[test]
    fn raster_aspect_uses_sample_intervals_and_clamps_narrow_maps() {
        let terrain = Terrain::new(2, 4, vec![0.0; 8], 10.0, 0.0, 0.0).unwrap();
        assert_eq!(map_size(&terrain).unwrap(), (2048, 683));
        let terrain = Terrain::new(2, 10000, vec![0.0; 20000], 1.0, 0.0, 0.0).unwrap();
        assert_eq!(map_size(&terrain).unwrap(), (2048, 1));
    }
}
