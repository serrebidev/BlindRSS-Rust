use blindrss_core::{Article, Db, Feed, config::Config, provider};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blindrss-provider-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("rss.db")
}

fn config(provider: &str, url: &str, account: &str) -> Config {
    let mut cfg = Config::default();
    cfg.set_path("active_provider", json!(provider));
    cfg.set_path(&format!("providers.{provider}.url"), json!(url));
    cfg.set_path(&format!("providers.{provider}.api_key"), json!(account));
    cfg.set_path(&format!("providers.{provider}.email"), json!(account));
    cfg
}

fn article() -> Article {
    Article {
        id: "miniflux-entry:1".into(),
        feed_id: "miniflux:1".into(),
        title: "Private".into(),
        url: "https://example.com/private".into(),
        content: Some("Keep me".into()),
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    }
}

fn feed() -> Feed {
    Feed {
        id: "miniflux:1".into(),
        url: "https://example.com/feed".into(),
        title: "Private feed".into(),
        title_is_custom: false,
        category: Some("Old".into()),
        icon_url: None,
    }
}

#[test]
fn account_cache_paths_are_distinct_private_and_idempotent() {
    let base = scratch();
    let first = config("miniflux", "https://reader.example/v1/", "secret-one");
    let same = config("miniflux", "https://reader.example/", "secret-one");
    let other = config("miniflux", "https://reader.example/", "secret-two");
    let a = provider::active_db_path(&first, &base);
    assert_ne!(a, base);
    assert_eq!(a, provider::active_db_path(&same, &base));
    assert_eq!(a, provider::active_db_path(&first, &a));
    assert_ne!(a, provider::active_db_path(&other, &base));
    assert!(!a.to_string_lossy().contains("secret-one"));
    assert_eq!(provider::active_db_path(&Config::default(), &base), base);
    let mut email = config("bazqux", "", "Person@Example.com");
    let before = provider::active_db_path(&email, &base);
    email.set_path("providers.bazqux.email", json!("person@example.com"));
    email.set_path("providers.bazqux.password", json!("replacement-password"));
    assert_eq!(before, provider::active_db_path(&email, &base));
    std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
}

#[test]
fn account_switch_does_not_reuse_legacy_or_other_account_rows() {
    let base = scratch();
    let legacy = Db::open_or_create(&base).unwrap();
    legacy
        .add_feed("https://local.example/feed", "Local", None)
        .unwrap();
    let first = config("miniflux", "https://reader.example", "first");
    let second = config("miniflux", "https://reader.example", "second");
    let one = Db::open_or_create(&provider::active_db_path(&first, &base)).unwrap();
    one.insert_feed_if_missing(&feed()).unwrap();
    one.insert_article_if_new(&article()).unwrap();
    let p1 = provider::get_provider(&first, base.clone());
    assert_eq!(p1.feeds().unwrap().len(), 1);
    let p2 = provider::get_provider(&second, base.clone());
    assert!(p2.feeds().unwrap().is_empty());
    assert!(p2.articles(None, 10).unwrap().is_empty());
    assert_eq!(legacy.feeds().unwrap()[0].title, "Local");
    assert_eq!(
        one.articles(None, 10).unwrap()[0].content.as_deref(),
        Some("Keep me")
    );
    drop((legacy, one, p1, p2));
    std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
}

fn server(replies: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = [0; 16_384];
            let count = stream.read(&mut bytes).unwrap();
            requests.push(String::from_utf8_lossy(&bytes[..count]).into_owned());
            write!(stream, "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (url, worker)
}

#[test]
fn failed_hosted_mutations_leave_cached_state_unchanged() {
    let (url, server) = server(vec![(500, "{}"), (500, "{}"), (500, "{}"), (500, "{}")]);
    let base = scratch();
    let cfg = config("miniflux", &url, "secret");
    let db = Db::open_or_create(&provider::active_db_path(&cfg, &base)).unwrap();
    db.insert_feed_if_missing(&feed()).unwrap();
    db.insert_article_if_new(&article()).unwrap();
    let p = provider::get_provider(&cfg, base.clone());
    assert!(p.mark_read(&article(), true).is_err());
    assert!(p.set_favorite(&article(), true).is_err());
    assert!(p.update_feed(&feed(), Some("Wrong"), None, None).is_err());
    assert!(p.remove_feed(&feed()).is_err());
    let old = db.article_by_id(&article().id).unwrap().unwrap();
    assert!(!old.is_read && !old.is_favorite);
    assert_eq!(db.feeds().unwrap()[0].title, "Private feed");
    assert_eq!(server.join().unwrap().len(), 4);
    drop((db, p));
    std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
}

#[test]
fn mark_all_read_uses_the_remote_account_endpoint_before_cache_update() {
    let (url, server) = server(vec![(200, r#"{"id":9,"username":"person"}"#), (204, "")]);
    let base = scratch();
    let cfg = config("miniflux", &url, "secret");
    let db = Db::open_or_create(&provider::active_db_path(&cfg, &base)).unwrap();
    db.insert_feed_if_missing(&feed()).unwrap();
    db.insert_article_if_new(&article()).unwrap();
    let p = provider::get_provider(&cfg, base.clone());
    assert_eq!(p.mark_all_read(None).unwrap(), 1);
    assert!(db.article_by_id(&article().id).unwrap().unwrap().is_read);
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /v1/me "));
    assert!(requests[1].starts_with("PUT /v1/users/9/mark-all-as-read "));
    drop((db, p));
    std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
}
