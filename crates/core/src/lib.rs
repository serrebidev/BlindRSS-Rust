//! BlindRSS core library (Rust port).
//!
//! Stage 1-3 slice: data layer over the existing Python `rss.db`, config
//! reader, feed parsing, and the hybrid extraction cascade. The Python
//! application remains the shipping product; this crate is developed in
//! parallel and must stay able to read any database the Python app writes.

pub mod config;
pub mod db;
pub mod extract;
pub mod feeds;
pub mod media;
pub mod opml;
pub mod providers;
pub mod refresh;

pub use db::{Article, Category, Chapter, Db, Feed, PlaybackState};
pub use extract::{ExtractOutcome, extract_article};
pub use feeds::{ParsedEntry, ParsedFeed, parse_feed};
pub use refresh::{refresh_all, refresh_feed, store_parsed};
