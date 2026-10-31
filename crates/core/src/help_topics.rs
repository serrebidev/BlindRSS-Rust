//! F1 help-topic registry, ported from Python core/help_topics.py.
//! Maps commands, dialogs, notebook pages, menus and context-menu
//! labels to user-guide section ids. Generated from the Python
//! module so the tables cannot drift apart silently.

pub const DEFAULT_TOPIC: &str = "user-guide";

/// (id, English title) of every guide section, in document order.
pub const TOPICS: &[(&str, &str)] = &[
    ("user-guide", "BlindRSS User Guide"),
    ("getting-started", "Getting Started"),
    ("main-window", "The Main Window"),
    ("feed-tree", "Feeds and Folders List"),
    ("article-list", "Article List"),
    ("reading-pane", "Reading Pane"),
    ("article-window", "Article Window"),
    ("search-field", "Search Field"),
    ("status-bar", "Status Bar"),
    ("context-menus", "Context Menus"),
    ("adding-feeds", "Adding a Feed"),
    ("detect-feeds", "Detecting Feeds on a Page"),
    ("find-podcast", "Finding Podcasts and RSS Feeds"),
    ("podcast-directories", "Podcast and Feed Directories"),
    ("subscribing", "Subscribing to a Search Result"),
    ("podcast-archive", "Podcast Archive"),
    ("video-search", "Video Search"),
    ("open-article-url", "Opening an Article by URL"),
    ("open-media-url", "Opening a Media URL"),
    ("removing-feeds", "Removing a Feed"),
    ("feed-properties", "Feed Properties"),
    ("categories", "Categories and Subcategories"),
    ("category-properties", "Category Properties"),
    ("refreshing", "Refreshing Feeds"),
    ("feed-errors", "Feeds with Errors"),
    ("import-opml", "Importing OPML"),
    ("export-opml", "Exporting OPML"),
    (
        "import-youtube-takeout",
        "Importing a YouTube Takeout Archive",
    ),
    ("persistent-search", "Persistent Searches"),
    ("smart-folders", "Smart Folders"),
    ("filter-rules", "Filter Rules"),
    ("article-filter", "Article Filter"),
    ("sorting", "Sorting Articles"),
    ("list-headers", "Article List Columns"),
    ("opening-articles", "Opening Articles"),
    ("read-status", "Read and Unread Articles"),
    ("favorites", "Favorites"),
    ("deleted-articles", "Deleted Articles"),
    ("full-text", "Full-Text Article Recovery"),
    ("rich-view", "Rich Full-Text View"),
    ("accessible-browser", "Accessible Browser"),
    ("youtube", "YouTube Videos"),
    ("forums", "Forum and Discussion Threads"),
    ("clipboard", "Cut, Copy, and Paste"),
    ("player", "The Built-in Player"),
    ("player-controls", "Player Controls"),
    ("player-shortcuts", "Player Keyboard Shortcuts"),
    ("playback-speed", "Playback Speed"),
    ("equalizer", "Equalizer"),
    ("chapters", "Chapters"),
    ("play-queue", "Play Queue"),
    ("casting", "Casting to Other Devices"),
    ("silence-skipping", "Skip Silence"),
    ("downloads", "Downloading Media"),
    ("settings", "Settings"),
    ("settings-general", "Settings: General"),
    ("settings-feeds", "Settings: Feeds and Articles"),
    ("settings-youtube", "Settings: YouTube"),
    ("settings-media-player", "Settings: Media Player"),
    ("settings-provider", "Settings: Provider"),
    ("settings-notifications", "Settings: Notifications"),
    ("settings-translate", "Settings: Translate"),
    ("settings-list-headers", "Settings: List Headers"),
    ("settings-advanced", "Settings: Advanced"),
    ("settings-captcha", "Settings: CAPTCHA Solving"),
    ("providers", "Online Accounts and Providers"),
    ("notifications", "Notifications"),
    ("translation", "Article Translation"),
    ("site-cookies", "Importing Site Cookies"),
    ("keyboard-shortcuts", "Keyboard Shortcuts"),
    ("shortcuts-reference", "Default Keyboard Shortcuts"),
    ("language", "Interface Language"),
    ("tray", "Tray Icon and Media Keys"),
    ("desktop-shortcuts", "Adding Desktop Shortcuts"),
    ("updates", "Checking for Updates"),
    ("version", "Announcing the Version"),
    ("about", "About BlindRSS"),
    ("help-window", "Using This Help Window"),
    ("troubleshooting", "Troubleshooting"),
    ("support", "Support and Community"),
];

pub const COMMAND_TOPIC_PAIRS: &[(&str, &str)] = &[
    ("article.copy_link", "clipboard"),
    ("article.copy_media_link", "clipboard"),
    ("article.copy_text", "clipboard"),
    ("article.delete", "deleted-articles"),
    ("article.download", "downloads"),
    ("article.download_as", "downloads"),
    ("article.open_browser", "opening-articles"),
    ("article.open_url", "open-article-url"),
    ("article.toggle_favorite", "favorites"),
    ("article.toggle_queue", "play-queue"),
    ("article.toggle_read", "read-status"),
    ("article.view_description", "feed-properties"),
    ("feeds.add", "adding-feeds"),
    ("feeds.add_category", "categories"),
    ("feeds.copy_url", "clipboard"),
    ("feeds.detect_page", "detect-feeds"),
    ("feeds.edit_selected", "feed-properties"),
    ("feeds.export_opml", "export-opml"),
    ("feeds.find_podcast", "find-podcast"),
    ("feeds.import_opml", "import-opml"),
    ("feeds.import_youtube_takeout", "import-youtube-takeout"),
    ("feeds.mark_all_read", "read-status"),
    ("feeds.podcast_archive", "podcast-archive"),
    ("feeds.refresh_all", "refreshing"),
    ("feeds.refresh_selected", "refreshing"),
    ("feeds.remove", "removing-feeds"),
    ("feeds.remove_category", "categories"),
    ("feeds.stop_refresh", "refreshing"),
    ("feeds.video_search", "video-search"),
    ("feeds.view_errors", "feed-errors"),
    ("filter.media_all", "article-filter"),
    ("filter.media_with", "article-filter"),
    ("filter.media_without", "article-filter"),
    ("filter.read_all", "article-filter"),
    ("filter.read_read", "article-filter"),
    ("filter.read_unread", "article-filter"),
    ("help.user_guide", "user-guide"),
    ("media.open_url", "open-media-url"),
    ("player.chapters", "chapters"),
    ("player.equalizer", "equalizer"),
    ("player.play_pause", "player-controls"),
    ("player.show_hide", "player"),
    ("player.stop", "player-controls"),
    ("queue.next", "play-queue"),
    ("queue.open", "play-queue"),
    ("queue.prev", "play-queue"),
    ("sort.ascending", "sorting"),
    ("sort.author", "sorting"),
    ("sort.date", "sorting"),
    ("sort.description", "sorting"),
    ("sort.feed", "sorting"),
    ("sort.name", "sorting"),
    ("sort.status", "sorting"),
    ("speed.down", "playback-speed"),
    ("speed.reset", "playback-speed"),
    ("speed.up", "playback-speed"),
    ("tools.announce_version", "version"),
    ("tools.backup_data", "export-opml"),
    ("tools.check_updates", "updates"),
    ("tools.filter_rules", "filter-rules"),
    ("tools.import_site_cookies", "site-cookies"),
    ("tools.keyboard_shortcuts", "keyboard-shortcuts"),
    ("tools.persistent_search", "persistent-search"),
    ("tools.restore_data", "export-opml"),
    ("tools.settings", "settings"),
    ("tools.youtube_account", "import-youtube-takeout"),
    ("view.accessible_browser", "accessible-browser"),
    ("view.focus_search", "search-field"),
    ("view.rich_view", "rich-view"),
    ("view.toggle_search", "search-field"),
];

pub const DIALOG_TOPIC_PAIRS: &[(&str, &str)] = &[
    ("AboutDialog", "about"),
    ("AccessibleBrowserFrame", "accessible-browser"),
    ("AddFeedDialog", "adding-feeds"),
    ("AddShortcutsDialog", "desktop-shortcuts"),
    ("ArticleWindow", "article-window"),
    ("CastDialog", "casting"),
    ("CategoryPropertiesDialog", "category-properties"),
    ("ColumnLayoutPanel", "list-headers"),
    ("EqualizerDialog", "equalizer"),
    ("ExcludeNotificationFeedsDialog", "notifications"),
    ("FeedErrorsDialog", "feed-errors"),
    ("FeedPropertiesDialog", "feed-properties"),
    ("FeedSearchDialog", "find-podcast"),
    ("FilterRuleEditorDialog", "filter-rules"),
    ("FilterRulesDialog", "filter-rules"),
    ("HelpWindow", "help-window"),
    ("ImportSiteCookiesDialog", "site-cookies"),
    ("KeyboardShortcutsDialog", "keyboard-shortcuts"),
    ("MainFrame", "main-window"),
    ("OpenArticleDialog", "open-article-url"),
    ("OpenMediaUrlDialog", "open-media-url"),
    ("PersistentSearchDialog", "persistent-search"),
    ("PlayerFrame", "player"),
    ("PodcastArchiveDialog", "podcast-archive"),
    ("QueueDialog", "play-queue"),
    ("SettingsDialog", "settings"),
    ("ShortcutCaptureDialog", "keyboard-shortcuts"),
    ("SmartFolderDialog", "smart-folders"),
    ("TakeoutImportSelectionDialog", "import-youtube-takeout"),
    ("YouTubeAccountDialog", "import-youtube-takeout"),
    ("YtdlpGlobalSearchDialog", "video-search"),
];

/// (dialog class, English tab title, topic id).
pub const NOTEBOOK_PAGE_TOPICS: &[(&str, &str, &str)] = &[
    ("FeedPropertiesDialog", "General", "feed-properties"),
    ("FeedPropertiesDialog", "List Headers", "list-headers"),
    ("SettingsDialog", "Advanced", "settings-advanced"),
    ("SettingsDialog", "CAPTCHA Solving", "settings-captcha"),
    ("SettingsDialog", "Feeds && Articles", "settings-feeds"),
    ("SettingsDialog", "General", "settings-general"),
    ("SettingsDialog", "List Headers", "settings-list-headers"),
    ("SettingsDialog", "Media Player", "settings-media-player"),
    ("SettingsDialog", "Notifications", "settings-notifications"),
    ("SettingsDialog", "Provider", "settings-provider"),
    ("SettingsDialog", "Translate", "settings-translate"),
    ("SettingsDialog", "YouTube", "settings-youtube"),
];

pub const MENU_TITLE_TOPIC_PAIRS: &[(&str, &str)] = &[
    ("&Edit", "clipboard"),
    ("&File", "feed-tree"),
    ("&Help", "help-window"),
    ("&Player", "player"),
    ("&Tools", "settings"),
    ("&View", "main-window"),
];

pub const CONTEXT_MENU_TOPIC_PAIRS: &[(&str, &str)] = &[
    ("Add Subcategory", "categories"),
    ("Add to Favorites", "favorites"),
    ("Add to Play Queue", "play-queue"),
    ("Add {count} to Play Queue", "play-queue"),
    ("Always show image alt text", "reading-pane"),
    ("Chapter Links", "chapters"),
    ("Copy Feed URL", "clipboard"),
    ("Copy Image Link", "clipboard"),
    ("Copy Link", "clipboard"),
    ("Copy Links", "clipboard"),
    ("Copy Media Link", "clipboard"),
    ("Copy Text", "clipboard"),
    ("Copy Text ({count} articles)", "clipboard"),
    ("Delete Article", "deleted-articles"),
    ("Delete Article Permanently", "deleted-articles"),
    ("Delete Category and Feeds", "categories"),
    ("Delete Smart Folder", "smart-folders"),
    ("Delete {count} Articles", "deleted-articles"),
    ("Delete {count} Articles Permanently", "deleted-articles"),
    ("Detect Audio", "player"),
    ("Download", "downloads"),
    ("Download As...", "downloads"),
    ("Edit Category", "category-properties"),
    ("Edit Feed...", "feed-properties"),
    ("Edit Smart Folder...", "smart-folders"),
    ("Export Category to OPML...", "export-opml"),
    ("Image Alt Text", "reading-pane"),
    ("Import OPML Here...", "import-opml"),
    ("Mark All Items as Read", "read-status"),
    ("Mark as &Read", "read-status"),
    ("Mark as &Unread", "read-status"),
    ("Mark {count} as &Read", "read-status"),
    ("Mark {count} as &Unread", "read-status"),
    ("Never show image alt text", "reading-pane"),
    ("New Smart Folder...", "smart-folders"),
    ("Notifications for This Feed", "notifications"),
    ("Open Article", "opening-articles"),
    ("Open Play Queue...", "play-queue"),
    ("Open in Default Browser", "opening-articles"),
    ("Refresh Category", "refreshing"),
    ("Refresh Feed", "refreshing"),
    ("Remove Category", "categories"),
    ("Remove Feed", "removing-feeds"),
    ("Remove from Favorites", "favorites"),
    ("Remove from Play Queue", "play-queue"),
    ("Remove {count} from Play Queue", "play-queue"),
    ("Reset Title to Feed Default", "feed-properties"),
    ("Restore Article", "deleted-articles"),
    ("Restore {count} Articles", "deleted-articles"),
    ("Use default setting", "reading-pane"),
    ("View Feed Description...", "feed-properties"),
    ("View History...", "podcast-archive"),
];

fn lookup(pairs: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// True when `topic_id` names a real guide section.
pub fn is_known_topic(topic_id: &str) -> bool {
    TOPICS.iter().any(|(id, _)| *id == topic_id)
}

/// The section id if known, else DEFAULT_TOPIC.
pub fn normalize(topic_id: &str) -> &'static str {
    TOPICS
        .iter()
        .find(|(id, _)| *id == topic_id)
        .map(|(id, _)| *id)
        .unwrap_or(DEFAULT_TOPIC)
}

/// English title of a section ("" when unknown).
pub fn topic_title(topic_id: &str) -> &'static str {
    TOPICS
        .iter()
        .find(|(id, _)| *id == topic_id)
        .map(|(_, t)| *t)
        .unwrap_or("")
}

/// Topic documenting a shortcut command id.
pub fn topic_for_command(command_id: &str) -> &'static str {
    lookup(COMMAND_TOPIC_PAIRS, command_id).unwrap_or(DEFAULT_TOPIC)
}

/// Topic documenting a dialog/window kind, if mapped.
pub fn topic_for_dialog(kind: &str) -> Option<&'static str> {
    lookup(DIALOG_TOPIC_PAIRS, kind)
}

/// Topic documenting a notebook page of a dialog, if mapped.
pub fn topic_for_notebook_page(dialog: &str, page_title: &str) -> Option<&'static str> {
    NOTEBOOK_PAGE_TOPICS
        .iter()
        .find(|(d, p, _)| *d == dialog && *p == page_title)
        .map(|(_, _, t)| *t)
}

/// Topic documenting a menu-bar title, if mapped.
pub fn topic_for_menu_title(title: &str) -> Option<&'static str> {
    lookup(MENU_TITLE_TOPIC_PAIRS, title)
}

/// Topic documenting a context-menu label, if mapped.
pub fn topic_for_context_label(label: &str) -> Option<&'static str> {
    lookup(CONTEXT_MENU_TOPIC_PAIRS, label)
}
