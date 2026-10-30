//! Netscape jar + matching tests.

use blindrss_core::cookies::{cookie_header_for, parse_netscape_jar};

const JAR: &str = "# Netscape HTTP Cookie File\n\
.example.com\tTRUE\t/\tFALSE\t2000000000\tsession\tabc\n\
pay.site.com\tFALSE\t/\tTRUE\t2000000000\ttoken\txyz\n\
#HttpOnly_.example.com\tTRUE\t/\tFALSE\t2000000000\thidden\th1\n\
expired.com\tTRUE\t/\tFALSE\t1000\told\tgone\n";

#[test]
fn jar_parse_and_match() {
    let jar = parse_netscape_jar(JAR);
    assert_eq!(jar.len(), 4);
    // Subdomain of example.com over http: session + hidden, not the
    // secure pay.site one.
    let header = cookie_header_for(&jar, "http://news.example.com/story");
    assert!(header.contains("session=abc"));
    assert!(header.contains("hidden=h1"));
    assert!(!header.contains("token"));
    // Secure cookie over https on its own host.
    let header = cookie_header_for(&jar, "https://pay.site.com/checkout");
    assert_eq!(header, "token=xyz");
    // Expired never sent.
    let header = cookie_header_for(&jar, "https://expired.com/");
    assert_eq!(header, "");
}
