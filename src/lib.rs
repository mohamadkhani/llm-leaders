//! Library crate: everything except the CLI. The binary (`main.rs`) and the
//! integration tests (`tests/`) both consume this, so the matcher can be
//! tested against fixtures without re-declaring the module tree.

pub mod arena;
pub mod benchmark_list;
pub mod benchlm;
pub mod identity;
pub mod matcher;
pub mod models_list;
pub mod openrouter;
pub mod or_bench;
pub mod or_catalog;
