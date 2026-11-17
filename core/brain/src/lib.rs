//! Rust brain — the Python brain migrates into this crate one module at a
//! time (see "Rust migration rules" in CLAUDE.md). Each migrated module keeps
//! the Python implementation's behavior: its Python tests are ported here
//! first, and the Python code is deleted only after these tests pass.
//!
//! Until the migration completes, the hosts (gateway, GUI) drive both sides:
//! migrated modules run in-process, Python-only stages are reached through
//! `nerv_shared::brain_client` JSON-RPC calls.

pub mod handle;
pub mod llm;
pub mod memory;
pub mod router;
pub mod skills;
pub mod tools;
pub mod turn;
pub mod workflows;
