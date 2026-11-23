//! Feature extraction: turn one file/folder into a sparse vector.
//!
//! Every feature is a short readable string ("name:cache", "in:slack",
//! "age:2y+") hashed into one of `DIM` slots (the "hashing trick"), so the
//! vocabulary never has to be stored and unseen words still get a slot.
//! The strings are kept alongside so the model can explain itself.

use crate::tree::Node;
use std::collections::HashSet;
use std::path::Path;

/// Number of hashed feature slots.
pub const DIM: usize = 1 << 16;

#[derive(Debug, Clone)]
pub struct Feature {
    pub name: String,
    pub idx: u32,
    pub val: f32,
}

pub struct Ctx<'a> {
    pub home: &'a Path,
    pub now: u64,
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Split a file name into lowercase word tokens.
/// "GPUCache" → gpu, cache · "com.spotify.client" → com, spotify, client
/// · "node_modules" → node, modules.
/// Hash slot for a feature name.
pub fn slot(name: &str) -> u32 {
    (fnv1a(name) % DIM as u64) as u32
}

pub fn tokens(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.chars().count() >= 2 && !cur.chars().all(|c| c.is_ascii_digit()) && !out.contains(cur) {
            out.push(cur.clone());
        }
        cur.clear();
    };
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            flush(&mut cur, &mut out);
            continue;
        }
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase() && c.is_uppercase() && next_lower)
                || (prev.is_alphabetic() != c.is_alphabetic());
            if boundary {
                flush(&mut cur, &mut out);
            }
        }
        cur.extend(c.to_lowercase());
    }
    flush(&mut cur, &mut out);
    out
}

pub fn age_bucket(days: u64) -> &'static str {
    match days {
        0..=6 => "<1w",
        7..=29 => "<1m",
        30..=89 => "<3m",
        90..=364 => "<1y",
        365..=729 => "<2y",
        _ => "2y+",
    }
}

pub fn size_bucket(bytes: u64) -> &'static str {
    match bytes {
        0..=999_999 => "<1MB",
        1_000_000..=9_999_999 => "1-10MB",
        10_000_000..=99_999_999 => "10-100MB",
        100_000_000..=999_999_999 => "100MB-1GB",
        _ => ">1GB",
    }
}

pub fn extract(path: &Path, node: &Node, ctx: &Ctx) -> Vec<Feature> {
    let mut seen = HashSet::new();
    let mut f = Vec::with_capacity(32);
    let mut add = |name: String, val: f32| {
        if seen.insert(name.clone()) {
            let idx = slot(&name);
            f.push(Feature { name, idx, val });
        }
    };

    add("bias".into(), 1.0);
    let is_dir = node.is_dir();
    add(if is_dir { "kind:dir" } else { "kind:file" }.into(), 1.0);

    // What it's called
    let name = &*node.name;
    if name.starts_with('.') {
        add("name:hidden".into(), 1.0);
    }
    for t in tokens(name) {
        add(format!("name:{t}"), 1.0);
    }
    if let Some(ext) = Path::new(name).extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        if ext.len() <= 8 {
            add(format!("ext:{ext}"), 1.0);
        }
    }

    // Where it lives
    match path.strip_prefix(ctx.home) {
        Ok(rel) => {
            let comps: Vec<String> = rel
                .parent()
                .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect())
                .unwrap_or_default();
            add(format!("top:{}", comps.first().map(String::as_str).unwrap_or("~")), 1.0);
            if comps.len() >= 2 {
                add(format!("top2:{}/{}", comps[0], comps[1]), 1.0);
            }
            if comps.iter().any(|c| c.starts_with('.')) {
                add("in:hidden".into(), 1.0);
            }
            for c in &comps {
                for t in tokens(c) {
                    add(format!("in:{t}"), 1.0);
                }
            }
            add("depth".into(), (comps.len() as f32 / 10.0).min(1.0));
        }
        Err(_) => {
            add("outside-home".into(), 1.0);
            if let Some(first) = path.components().nth(1) {
                add(format!("top:/{}", first.as_os_str().to_string_lossy().to_lowercase()), 1.0);
            }
        }
    }

    // How big, how old, how many files
    add(format!("size:{}", size_bucket(node.size)), 1.0);
    if node.modified > 0 {
        let days = ctx.now.saturating_sub(node.modified) / 86_400;
        add(format!("age:{}", age_bucket(days)), 1.0);
    }
    f
}

/// Human wording for a feature name, used in explanations.
pub fn describe(name: &str) -> Option<String> {
    let (kind, v) = name.split_once(':').unwrap_or((name, ""));
    Some(match kind {
        "name" if v == "hidden" => "hidden item".into(),
        "name" => format!("name contains “{v}”"),
        "ext" => format!(".{v} file"),
        "in" if v == "hidden" => "inside a hidden folder".into(),
        "in" => format!("inside a “{v}” folder"),
        "top" if v.starts_with('/') => format!("in {v}"),
        "top" if v == "~" => "directly in your home folder".into(),
        "top" => format!("in ~/{v}"),
        "top2" => format!("in ~/{v}"),
        "age" => match v {
            "<1w" => "changed this week".into(),
            "<1m" => "changed this month".into(),
            "<3m" => "changed in the last 3 months".into(),
            "<1y" => "untouched for months".into(),
            "<2y" => "untouched for over a year".into(),
            _ => "untouched for 2+ years".into(),
        },
        "size" => format!("size {v}"),
        "files" => format!("{v} files inside"),
        "outside-home" => "outside your home folder".into(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer() {
        assert_eq!(tokens("GPUCache"), ["gpu", "cache"]);
        assert_eq!(tokens("com.spotify.client"), ["com", "spotify", "client"]);
        assert_eq!(tokens("node_modules"), ["node", "modules"]);
        assert_eq!(tokens("Xcode15.2.xip"), ["xcode", "xip"]);
    }
}
