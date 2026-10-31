//! Custom article-open command parsing (Python
//! `core/utils.py::build_open_command`, issue #31).
//!
//! `%1` is replaced by the URL; if the template contains no
//! `%1`, the URL is appended as the final argument. Splitting
//! follows Python's shlex: POSIX rules off Windows; on Windows
//! the non-POSIX mode keeps backslashes in paths intact and the
//! surrounding quotes on tokens are stripped afterwards.

/// Split a command template into argv tokens (Python
/// `shlex.split` in the given mode). Errors on an unterminated
/// quote, like shlex's ValueError.
pub fn split_command(template: &str, posix: bool) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has_token = false;
    let mut chars = template.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                    if !posix {
                        cur.push(c); // non-POSIX keeps the quote char
                    }
                } else if posix && q == '"' && c == '\\' {
                    // Inside POSIX double quotes, backslash escapes
                    // only $ ` " \ and newline.
                    match chars.peek() {
                        Some(&n) if matches!(n, '$' | '`' | '"' | '\\' | '\n') => {
                            chars.next();
                            if n != '\n' {
                                cur.push(n);
                            }
                        }
                        _ => cur.push(c),
                    }
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c.is_whitespace() {
                    if has_token {
                        out.push(std::mem::take(&mut cur));
                        has_token = false;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                    has_token = true;
                    if !posix {
                        cur.push(c);
                    }
                } else if posix && c == '\\' {
                    has_token = true;
                    match chars.next() {
                        Some(n) if n != '\n' => cur.push(n),
                        Some(_) => {}
                        None => cur.push('\\'),
                    }
                } else {
                    has_token = true;
                    cur.push(c);
                }
            }
        }
    }
    if quote.is_some() {
        return Err("No closing quotation".to_string());
    }
    if has_token {
        out.push(cur);
    }
    Ok(out)
}

/// Parse a custom open-article command template into an argv
/// list (Python `build_open_command`). Errors for an empty or
/// unparseable template.
pub fn build_open_command(template: &str, url: &str) -> Result<Vec<String>, String> {
    let template = template.trim();
    if template.is_empty() {
        return Err("The command is empty.".to_string());
    }
    let posix = !cfg!(windows);
    let parts =
        split_command(template, posix).map_err(|e| format!("Could not parse the command: {e}"))?;
    let mut cleaned: Vec<String> = Vec::new();
    for mut part in parts {
        if !posix && part.len() >= 2 && part.starts_with('"') && part.ends_with('"') {
            part = part[1..part.len() - 1].to_string();
        }
        cleaned.push(part);
    }
    if cleaned.is_empty() {
        return Err("The command is empty.".to_string());
    }
    let mut argv = Vec::new();
    let mut substituted = false;
    for part in cleaned {
        if part.contains("%1") {
            argv.push(part.replace("%1", url));
            substituted = true;
        } else {
            argv.push(part);
        }
    }
    if !substituted {
        argv.push(url.to_string());
    }
    Ok(argv)
}
