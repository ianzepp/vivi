#![deny(clippy::pedantic)]

//! The vivi mail formats: the letter (TOML front matter plus Markdown body),
//! the role reply schemas, and EML composition and rendering.

pub mod letter;
pub mod message;
