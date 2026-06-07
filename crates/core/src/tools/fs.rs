//! Filesystem tools: `read_file`, `write_file`, `edit_file`, `multi_edit`,
//! `list_dir`, and `undo_last_edit`, with shared path/atomic-write helpers in
//! `common`. Each tool lives in its own submodule; the public tools and the
//! `resolve`/`snapshot_meta`/`ensure_not_stale`/`atomic_write_preserving_permissions`
//! helpers are unchanged.

mod common;
mod edit;
mod list;
mod read;
mod undo;
mod write;

#[cfg(test)]
mod edit_undo_tests;
#[cfg(test)]
mod read_write_tests;
#[cfg(test)]
mod test_support;

pub use edit::{EditFile, MultiEdit};
pub use list::ListDir;
pub use read::ReadFile;
pub use undo::UndoLastEdit;
pub use write::WriteFile;

pub(crate) use common::{
    atomic_write_preserving_permissions, ensure_not_stale, resolve, snapshot_meta,
};
