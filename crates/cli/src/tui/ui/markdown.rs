//! Markdown rendering, split by area: inline spans, block layout, syntax
//! highlighting, and tables. Split out of `ui`; logic unchanged.

use super::*;

mod block;
mod highlight;
mod inline;
mod table;

pub(crate) use block::*;
pub(crate) use highlight::*;
pub(crate) use inline::*;
pub(crate) use table::*;
