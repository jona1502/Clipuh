use std::path::Path;

fn is_sep(b: u8) -> bool {
    b == b'\\' || b == b'/'
}

/// Length of match of `pat` at the start of `text`. Separators are
/// interchangeable; a separator in `pat` matches a run of 1+ separators in
/// `text` (covers Debug-escaped `\\`). ASCII case-insensitive.
fn match_home(pat: &[u8], text: &[u8]) -> Option<usize> {
    let (mut pi, mut ti) = (0, 0);
    while pi < pat.len() {
        if ti >= text.len() {
            return None;
        }
        if is_sep(pat[pi]) {
            if !is_sep(text[ti]) {
                return None;
            }
            while ti < text.len() && is_sep(text[ti]) {
                ti += 1;
            }
            while pi < pat.len() && is_sep(pat[pi]) {
                pi += 1;
            }
        } else {
            if !pat[pi].eq_ignore_ascii_case(&text[ti]) {
                return None;
            }
            pi += 1;
            ti += 1;
        }
    }
    Some(ti)
}

fn replace_home(text: &str, home: &str) -> String {
    let pat = home.trim_end_matches(['\\', '/']).as_bytes();
    if pat.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last) = (0, 0);
    while i < bytes.len() {
        if text.is_char_boundary(i)
            && let Some(len) = match_home(pat, &bytes[i..])
        {
            // Do not swallow a longer path component (e.g. "C:\Users\bob2").
            let next = bytes.get(i + len).copied();
            let ok = next.is_none_or(|b| is_sep(b) || !(b.is_ascii_alphanumeric() || b >= 0x80));
            if ok {
                out.push_str(&text[last..i]);
                out.push_str("%USERPROFILE%");
                i += len;
                last = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

fn replace_user(text: &str, name: &str) -> String {
    let pat = name.as_bytes();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last) = (0, 0);
    while i + pat.len() <= bytes.len() {
        if text.is_char_boundary(i)
            && text.is_char_boundary(i + pat.len())
            && bytes[i..i + pat.len()].eq_ignore_ascii_case(pat)
        {
            let before = text[..i].chars().next_back();
            let after = text[i + pat.len()..].chars().next();
            if !before.is_some_and(char::is_alphanumeric)
                && !after.is_some_and(char::is_alphanumeric)
            {
                out.push_str(&text[last..i]);
                out.push_str("<user>");
                i += pat.len();
                last = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[last..]);
    out
}

/// Redacts home directory path and user name from `text`. Pure.
pub fn sanitize(text: &str, home: Option<&Path>, user_name: Option<&str>) -> String {
    let mut s = match home.and_then(Path::to_str) {
        Some(h) if !h.trim().is_empty() => replace_home(text, h.trim()),
        _ => text.to_string(),
    };
    if let Some(name) = user_name.map(str::trim)
        && name.chars().count() >= 3
    {
        s = replace_user(&s, name);
    }
    s
}
