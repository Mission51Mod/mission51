//! foxpipe: what every pipeline stage needs besides its algorithm (docs/release/RUST_PORTING.md).
//!   paths   repo root discovery (no hardcoded drive paths), TEMP on the repo's work/tmp, game-data roots
//!   polite  below-normal / idle priority, "is a game running" check
//!   project the location project spec (projects/<code>/project.toml): model, validation, datasets, Loc, facets
//!   guard   FOX_WRITE_ROOTS write guard (editor workspaces)
//!   trace   foxbuild I/O trace of Rust stage code (reads / writes / lists -> FOXBUILD_TRACE_DIR/<pid>.json)
pub mod guard;
pub mod paths;
pub mod polite;
pub mod project;
pub mod trace;
