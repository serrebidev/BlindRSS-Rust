//! Netscape jar + matching tests.

use blindrss_core::cookies::{cookie_header_for, parse_netscape_jar};

const JAR: &str = "# Netscape HTTP Cookie File\n\
.example.com\tTRUE\t/\tFALSE\t2000000000\tsession\tabc\n\
pay.site.com\tFALSE\t/\tTRUE\t2000000000\ttoken\txyz\n\
#HttpOnly_.example.com\tTRUE\t/\tFALSE\t2000000000\thidden\th1\n\
expired.com\tTRUE\t/\tFALSE\t1000\told\tgone\n";

#[test]
fn path_matching_requires_a_slash_boundary() {
    // Audit 8 B07 (Python site_cookies.cookies_for): a cookie
    // scoped to /private matches /private and /private/child but
    // must not leak to the sibling /private-other.
    let jar = parse_netscape_jar(
        "example.com\tFALSE\t/private\tFALSE\t2000000000\tsecret\ts1\n\
         example.com\tFALSE\t/private/\tFALSE\t2000000000\ttrail\tt1\n",
    );
    assert_eq!(
        cookie_header_for(&jar, "https://example.com/private"),
        "secret=s1"
    );
    assert_eq!(
        cookie_header_for(&jar, "https://example.com/private/child"),
        "secret=s1; trail=t1"
    );
    assert_eq!(
        cookie_header_for(&jar, "https://example.com/private-other"),
        ""
    );
    assert_eq!(
        cookie_header_for(&jar, "https://example.com/private-other/x"),
        ""
    );
    // A root-scoped cookie still matches everywhere.
    let jar = parse_netscape_jar("example.com\tFALSE\t/\tFALSE\t2000000000\troot\tr1\n");
    assert_eq!(
        cookie_header_for(&jar, "https://example.com/anywhere/deep"),
        "root=r1"
    );
}

#[test]
fn budget_keeps_clearance_and_small_cookies_past_an_oversize_first_entry() {
    // Audit 8 B06 (Python _trim_cookies_to_budget): selection
    // happens before rendering — clearance first, then smallest —
    // so a huge first cookie cannot starve the rest, and the
    // rendered header stays within the 8,000-byte budget while
    // preserving the jar's relative order among the kept cookies.
    let huge = "x".repeat(7_980);
    let jar = parse_netscape_jar(&format!(
        "example.com\tFALSE\t/\tFALSE\t2000000000\tbig_one\t{huge}\n\
         example.com\tFALSE\t/\tFALSE\t2000000000\tcf_clearance\tclear123\n\
         example.com\tFALSE\t/\tFALSE\t2000000000\tsmall\ts\n"
    ));
    let header = cookie_header_for(&jar, "https://example.com/");
    assert!(header.len() <= 8000, "header is {} bytes", header.len());
    assert!(header.contains("cf_clearance=clear123"), "{header}");
    assert!(header.contains("small=s"), "{header}");
    assert!(
        !header.contains("big_one"),
        "oversize cookie kept: {header}"
    );
}

#[test]
fn duplicate_names_resolve_to_the_last_value() {
    // Python cookies_for builds a name->value map in jar order:
    // a later cookie with the same name replaces the earlier one.
    let jar = parse_netscape_jar(
        "example.com\tFALSE\t/\tFALSE\t2000000000\tsession\told\n\
         example.com\tFALSE\t/\tFALSE\t2000000000\tsession\tnew\n",
    );
    assert_eq!(
        cookie_header_for(&jar, "https://example.com/"),
        "session=new"
    );
}

#[test]
fn leading_dot_domain_implies_subdomains() {
    // Python _parse_jar: include_subdomains is the leading dot OR
    // the TRUE flag.
    let jar = parse_netscape_jar(".example.com\tFALSE\t/\tFALSE\t2000000000\tdotted\td1\n");
    assert_eq!(
        cookie_header_for(&jar, "https://news.example.com/"),
        "dotted=d1"
    );
}

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
