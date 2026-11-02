//! BlindRSS core library (Rust port).
//!
//! Stage 1-3 slice: data layer over the existing Python `rss.db`, config
//! reader, feed parsing, and the hybrid extraction cascade. The Python
//! application remains the shipping product; this crate is developed in
//! parallel and must stay able to read any database the Python app writes.

pub mod announce;
pub mod archive;
pub mod archive_store;
pub mod article_columns;
pub mod article_html;
pub mod article_lang;
pub mod browser;
pub mod captcha;
pub mod cast;
pub mod castsession;
pub mod castupnp;
pub mod castwire;
pub mod chromium;
pub mod config;
pub mod cookies;
pub mod cookies_import;
pub mod db;
pub mod dependency;
pub mod discovery;
pub mod download;
pub mod enrich;
pub mod equalizer;
pub mod extract;
pub mod feeds;
pub mod feedsearch;
pub mod forum_sources;
pub mod help;
pub mod help_topics;
pub mod http;
pub mod http_headers;
pub mod i18n;
pub mod maintenance;
pub mod media;
pub mod media_url;
pub mod oauth;
pub mod opencmd;
pub mod opml;
pub mod play_cache;
pub mod playback;
pub mod postprocess;
pub mod provider;
pub mod providers;
pub mod proxy;
pub mod rangecache;
pub mod raop;
pub mod reader_cache;
pub mod refresh;
pub mod retention;
pub mod rules;
pub mod shortcuts;
pub mod shortcuts_data;
pub mod silence;
pub mod single_instance;
pub mod source_feeds;
pub mod sources;
pub mod sponsorblock;
pub mod takeout;
pub mod threads;
pub mod translate;
pub mod translation_updates;
pub mod user_agents;
pub mod videosearch;
pub mod youtube_account;
pub mod ytfull;

pub use db::{Article, Category, Chapter, Db, Feed, PlaybackState};
pub use extract::{ExtractOutcome, extract_article};
pub use feeds::{ParsedEntry, ParsedFeed, parse_feed};
pub use refresh::{refresh_all, refresh_feed, store_parsed};
