//! Preview back ends (data loading and rendering); the Previews tab is preview_view.rs.
//!   terrain  height grids (.npy / .htre), CPU hillshade, mesh
//!   gpu      the 3-D terrain view on eframe's wgpu device (offscreen target shown as an egui image)
//!   nav      .nav2 polygon edges (foxcore::nav2)
//!   texture  .ftex / .dds / .png decoding (preview-only stopgap until foxcore::ftex)
//!   model    read-only FMDL geometry, UV/material bindings, normals and bind bones
//!   worldtex the location's world-texture tiles as one mosaic (drape for the terrain views)
pub mod density;
pub mod navdiff;
pub mod gpu;
pub mod model;
pub mod nav;
pub mod terrain;
pub mod texture;
pub mod worldtex;
