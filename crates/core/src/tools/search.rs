//! File search tools: `grep` (regex) and `glob` (path patterns), each in its
//! own submodule with shared subprocess/output helpers in `shared`. Split for
//! file size; the public `Grep`/`Glob` tools and their behavior are unchanged.

mod glob;
mod grep;
mod shared;

#[cfg(test)]
mod test_support;

pub use glob::Glob;
pub use grep::Grep;
