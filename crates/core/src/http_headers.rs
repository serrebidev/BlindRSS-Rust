//! Per-channel media headers, matching Python `core/http_headers.py`.

use serde_json::Value;

fn insert(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        || value.is_empty()
        || value.bytes().any(|b| b < 32 || b == 127)
        || matches!(
            name.as_str(),
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "proxy-authorization"
                | "range"
                | "accept-encoding"
        )
    {
        return;
    }
    headers.retain(|(key, _)| key != &name);
    headers.push((name, value.to_owned()));
}

/// Copy channel metadata aliases and additional `Name: value` header lines.
pub fn channel_http_headers(channel: &Value) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for (keys, target) in [
        (&["http-user-agent"][..], "user-agent"),
        (&["http-referrer", "http-referer"][..], "referer"),
        (&["http-origin"][..], "origin"),
        (&["http-cookie"][..], "cookie"),
        (&["http-authorization"][..], "authorization"),
        (&["http-accept"][..], "accept"),
    ] {
        if let Some(value) = keys.iter().find_map(|key| {
            channel
                .get(key)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        }) {
            insert(&mut headers, target, value);
        }
    }
    if let Some(extra) = channel.get("http-headers").and_then(Value::as_array) {
        for line in extra.iter().filter_map(Value::as_str) {
            if let Some((name, value)) = line.split_once(':') {
                insert(&mut headers, name, value);
            }
        }
    }
    headers
}

/// Add browser identity and URL-scoped cookies without replacing explicit metadata.
pub fn playback_headers(
    channel: &Value,
    media_url: &str,
    article_url: &str,
    source_url: &str,
    cookies: &[crate::cookies::Cookie],
) -> Vec<(String, String)> {
    if !url::Url::parse(media_url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
        return Vec::new();
    }
    let mut headers = headers_for_target(&channel_http_headers(channel), source_url, media_url);
    let mut defaults = vec![
        ("user-agent", crate::http::user_agent()),
        ("accept", "*/*".to_owned()),
    ];
    if let Ok(mut article) = url::Url::parse(article_url)
        && matches!(article.scheme(), "http" | "https")
    {
        let _ = article.set_username("");
        let _ = article.set_password(None);
        article.set_fragment(None);
        defaults.push(("origin", article.origin().ascii_serialization()));
        let same_origin =
            url::Url::parse(media_url).is_ok_and(|media| media.origin() == article.origin());
        let referer = if same_origin {
            article.to_string()
        } else {
            format!("{}/", article.origin().ascii_serialization())
        };
        defaults.push(("referer", referer));
    }
    defaults.push((
        "cookie",
        crate::cookies::cookie_header_for(cookies, media_url),
    ));
    for (key, value) in defaults {
        if !headers.iter().any(|(name, _)| name == key) {
            insert(&mut headers, key, &value);
        }
    }
    headers
}

/// Relay protected media through the existing cache; secrets stay out of player/cast URLs.
pub fn protected_media_url(url: &str, headers: &[(String, String)]) -> String {
    if headers.is_empty()
        || !url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
    {
        return url.to_owned();
    }
    crate::rangecache::proxify(url, headers, false)
}

/// Credentials and site-specific headers belong only to the original origin.
pub(crate) fn headers_for_target(
    headers: &[(String, String)],
    original: &str,
    target: &str,
) -> Vec<(String, String)> {
    let same_origin = url::Url::parse(original)
        .ok()
        .zip(url::Url::parse(target).ok())
        .is_some_and(|(a, b)| a.origin() == b.origin());
    let mut out = Vec::new();
    for (name, value) in headers {
        if same_origin
            || matches!(
                name.to_ascii_lowercase().as_str(),
                "user-agent" | "accept" | "accept-encoding" | "range"
            )
        {
            insert(&mut out, name, value);
            // Range and identity encoding are controlled by the cache, not metadata.
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "range" | "accept-encoding"
            ) {
                out.push((name.clone(), value.clone()));
            }
        }
    }
    out
}

/// Follow media redirects explicitly so arbitrary provider headers cannot leak to another origin.
pub(crate) fn media_get(
    original: &str,
    target: &str,
    headers: &[(String, String)],
) -> Result<ureq::http::Response<ureq::Body>, ()> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .user_agent(crate::http::user_agent())
            .http_status_as_error(false)
            .max_redirects(0)
            .build(),
    );
    let mut target = url::Url::parse(target).map_err(|_| ())?;
    for _ in 0..10 {
        if !matches!(target.scheme(), "http" | "https") {
            return Err(());
        }
        let mut request = agent.get(target.as_str());
        for (name, value) in headers_for_target(headers, original, target.as_str()) {
            request = request.header(&name, &value);
        }
        let response = request.call().map_err(|_| ())?;
        if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or(())?;
        target = target.join(location).map_err(|_| ())?;
        if !target.username().is_empty() || target.password().is_some() {
            return Err(());
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn channel_aliases_and_extra_headers_are_collected() {
        let headers = channel_http_headers(
            &json!({"http-user-agent":"Reader", "http-referrer":"https://first.example/", "http-referer":"https://second.example/", "http-origin":"https://first.example", "http-cookie":"session=value", "http-authorization":"Bearer value", "http-accept":"audio/*", "http-headers":["X-Token: abc:def", "Accept: */*", null]}),
        );
        assert!(headers.contains(&("referer".into(), "https://first.example/".into())));
        assert!(headers.contains(&("authorization".into(), "Bearer value".into())));
        assert!(headers.contains(&("x-token".into(), "abc:def".into())));
        assert_eq!(headers.iter().filter(|(k, _)| k == "accept").count(), 1);
        assert!(headers.contains(&("accept".into(), "*/*".into())));
        assert!(channel_http_headers(&Value::Null).is_empty());
    }

    #[test]
    fn malformed_and_transport_headers_are_rejected() {
        let headers = channel_http_headers(
            &json!({"http-cookie":"value\r\nInjected: yes", "http-headers":["Host: attacker", "Content-Length: 1", "Range: bytes=0-1", "Bad Name: value", "X-Test: safe", "X-Nul: a\u{0000}b"]}),
        );
        assert_eq!(headers, vec![("x-test".into(), "safe".into())]);
    }

    #[test]
    fn browser_origin_has_scheme_host_and_port_without_path_or_credentials() {
        let headers = playback_headers(
            &json!({}),
            "https://cdn.example/media",
            "https://user:pass@news.example:8443/story?q=1#chapter",
            "https://cdn.example/media",
            &[],
        );
        assert!(headers.contains(&("origin".into(), "https://news.example:8443".into())));
        assert!(headers.contains(&("referer".into(), "https://news.example:8443/".into())));
        assert!(
            playback_headers(
                &json!({"http-cookie":"secret"}),
                "C:/audio.mp3",
                "https://news.example/",
                "C:/audio.mp3",
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn explicit_metadata_wins_and_cookies_match_media_host() {
        let jar = crate::cookies::parse_netscape_jar(
            "media.example\tFALSE\t/\tTRUE\t0\tsession\tvalue\n",
        );
        let headers = playback_headers(
            &json!({"http-origin":"https://override.example", "http-user-agent":"Custom"}),
            "https://media.example/a",
            "https://news.example/a",
            "https://media.example/a",
            &jar,
        );
        assert!(headers.contains(&("origin".into(), "https://override.example".into())));
        assert!(headers.contains(&("user-agent".into(), "Custom".into())));
        assert!(headers.contains(&("cookie".into(), "session=value".into())));
        assert!(
            !playback_headers(
                &Value::Null,
                "https://other.example/a",
                "",
                "https://other.example/a",
                &jar
            )
            .iter()
            .any(|(k, _)| k == "cookie")
        );
        let resolved = playback_headers(
            &json!({"http-cookie":"source-secret", "http-authorization":"Bearer source-secret"}),
            "https://media.example/a",
            "https://news.example/a",
            "https://other.example/source",
            &jar,
        );
        assert!(
            !resolved
                .iter()
                .any(|(k, v)| k == "authorization" || v.contains("source-secret"))
        );
        assert!(resolved.contains(&("cookie".into(), "session=value".into())));
    }

    #[test]
    fn cross_origin_redirects_never_receive_site_headers() {
        let headers = channel_http_headers(
            &json!({"http-cookie":"secret", "http-authorization":"Bearer secret", "http-origin":"https://site.example", "http-headers":["X-Token: secret"], "http-user-agent":"Reader"}),
        );
        assert_eq!(
            headers_for_target(
                &headers,
                "https://site.example/a",
                "https://other.example/b"
            ),
            vec![("user-agent".into(), "Reader".into())]
        );
        assert_eq!(
            headers_for_target(&headers, "https://site.example/a", "https://site.example/b"),
            headers
        );
        assert_eq!(
            headers_for_target(&headers, "https://site.example/a", "http://site.example/a").len(),
            1
        );
    }

    #[test]
    fn redirected_requests_strip_credentials_before_contacting_other_origin() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        let original = format!("http://{}/media", origin.local_addr().unwrap());
        let target = format!("http://{}/media", destination.local_addr().unwrap());
        let first = std::thread::spawn(move || {
            let (mut socket, _) = origin.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer secret"));
            write!(socket, "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let second = std::thread::spawn(move || {
            let (mut socket, _) = destination.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 8192];
            let n = socket.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(!request.contains("secret"));
            assert!(!request.contains("authorization:"));
            assert!(!request.contains("cookie:"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let headers = channel_http_headers(
            &json!({"http-authorization":"Bearer secret", "http-cookie":"session=secret", "http-headers":["X-Token: secret"]}),
        );
        assert_eq!(
            media_get(&original, &original, &headers)
                .unwrap()
                .status()
                .as_u16(),
            200
        );
        first.join().unwrap();
        second.join().unwrap();
    }
}
