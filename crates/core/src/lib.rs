//! BlindRSS core library (Rust port).
//!
//! Stage 1-3 slice: data layer over the existing Python `rss.db`, config
//! reader, feed parsing, and the hybrid extraction cascade. The Python
//! application remains the shipping product; this crate is developed in
//! parallel and must stay able to read any database the Python app writes.

pub mod announce;
pub mod archive;
pub mod cast;
pub mod castsession;
pub mod castwire;
pub mod config;
pub mod cookies;
pub mod db;
pub mod discovery;
pub mod download;
pub mod extract;
pub mod feeds;
pub mod help;
pub mod i18n;
pub mod maintenance;
pub mod media;
pub mod oauth;
pub mod opml;
pub mod providers;
pub mod refresh;
pub mod rules;
pub mod shortcuts;
pub mod shortcuts_data;
pub mod takeout;

pub use db::{Article, Category, Chapter, Db, Feed, PlaybackState};
pub use extract::{ExtractOutcome, extract_article};
pub use feeds::{ParsedEntry, ParsedFeed, parse_feed};
pub use refresh::{refresh_all, refresh_feed, store_parsed};
