//! Moving from another monitoring tool (ADR-0146): readers of other
//! tools' configuration, as export files or through their API, into a
//! neutral [`model::Inventory`]. The server previews it, and creates the
//! matching servers, checks, thresholds, silences and notification
//! channels once a person confirms.
//!
//! To add a reader: a module that implements [`source::Source`], listed
//! in [`source::sources`], its notes in [`notes::NOTES`] and in the web
//! interface's catalog, and fixtures under `tests/fixtures/<id>/`.

pub mod http;
pub mod model;
pub mod notes;
pub mod prometheus;
pub mod source;
pub mod text;
pub mod xml;

pub use model::Inventory;
pub use notes::Note;
pub use source::{Context, Input, ReadError, Source, SourceInfo};
