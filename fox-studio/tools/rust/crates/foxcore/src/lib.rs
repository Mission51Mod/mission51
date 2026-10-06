//! foxcore: Fox Engine (MGSV:TPP) formats and compute kernels.
//!
//! Rules for everything in this crate (docs/release/RUST_DEV.md):
//! * format kernels operate on bytes / slices; no global state or file-system policy;
//! * runtime_data reads caller-selected local archives and writes only an explicit profile cache;
//! * every port reproduces its Python reference byte for byte (tools/build/regress.py proves it);
//! * deterministic: no unordered iteration feeding outputs, no float reassociation (no fast-math, no FMA
//!   contraction is done by rustc by default), parallel code must reduce in a fixed order.

pub mod block_texture;
pub mod codecs;
pub mod containers;
pub mod dict;
#[cfg(feature = "proof-compat")]
pub mod dxt;
pub mod dxt2;
pub mod fdes;
pub mod fmdl;
pub mod fox2;
pub mod foxdata;
pub mod fpk;
pub mod frdv;
pub mod frt;
pub mod fsm;
pub mod fstb;
pub mod ftex;
pub mod geom;
pub mod grxla;
pub mod hash;
pub mod lba;
pub mod lng2;
pub mod lpsh;
pub mod mtar;
pub mod nav2;
pub mod npy;
pub mod obr;
#[cfg(feature = "internal-game-data")]
pub mod packorder_rules;
pub mod qar;
pub mod sand;
pub mod subp;
pub mod terrain;
pub mod twpf;
pub mod vfxlb;
pub mod wwise;

pub mod runtime_data;
#[cfg(feature = "internal-game-data")]
mod internal_qar_data;
