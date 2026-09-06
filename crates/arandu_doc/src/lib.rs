//! Pure documentation rendering engine for the Arandu programming language.
//!
//! Provides zero-dependency, pure transformations of [`arandu_middle::docs::DocModule`]
//! into JSON, GitHub-Flavored Markdown, and standalone static HTML.

pub mod html;
pub mod json;
pub mod markdown;

pub use html::render_html;
pub use json::render_json;
pub use markdown::render_markdown;
