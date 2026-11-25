//! Data layer: package sources (archlinux.org JSON, AUR RPC + website, pacman),
//! relevance ranking, install routing, and text helpers. No UI code lives here.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const UA: &str = "archstore/2.0 (Arch Linux terminal package store)";
const ARCH_JSON: &str = "https://archlinux.org/packages/search/json/";
const AUR_RPC: &str = "https://aur.archlinux.org/rpc/v5";
const AUR_HTML: &str = "https://aur.archlinux.org/packages";

// --------------------------------------------------------------------- model

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Src {
    Repo,
    Aur,
}

#[derive(Clone, Debug)]
pub struct Pkg {
    pub src: Src,
    pub repo: String,
    pub name: String,
    pub version: String,
    pub desc: String,
    pub maintainer: String,
    pub votes: i64,
    pub popularity: f64,
    pub outdated: bool,
    pub installed: bool,
}

#[derive(Clone, Debug)]
pub struct Detail {
    pub name: String,
    pub src: Src,
    pub rows: Vec<(String, String)>,
    pub url: String,
}

// ------------------------------------------------------------------ worker

pub enum Job {
    Search { gen: u64, q: String, offline: bool },
    Top,
    Updated,
    Installed,
    Detail { name: String, src: Src },
}

pub enum Out {
    Search { gen: u64, items: Vec<Pkg>, net_down: bool },
    Top { items: Vec<Pkg> },
    Updated { items: Vec<Pkg> },
    Installed { items: Vec<Pkg> },
    Detail { name: String, detail: Option<Detail> },
}

pub fn worker(rx: Receiver<Job>, tx: Sender<Out>) {
    while let Ok(job) = rx.recv() {
        let out = match job {
            Job::Search { gen, q, offline } => {
                let (mut items, net_down) = search_official(&q, offline);
                if !offline {
                    items.extend(search_aur(&q));
                }
                let items = rank(&q, items, 50);
                Out::Search { gen, items, net_down }
            }
            Job::Top => Out::Top { items: aur_listing("p", "d", 25) },
            Job::Updated => Out::Updated { items: aur_listing("l", "d", 25) },
            Job::Installed => Out::Installed { items: installed_pkgs() },
            Job::Detail { name, src } => Out::Detail {
                detail: detail_of(&name, src),
                name,
            },
        };
        if tx.send(out).is_err() {
            break;
        }
    }
}

// --------------------------------------------------------------------- http

fn http() -> &'static reqwest::blocking::Client {
    static C: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build the http client")
    })
}

fn get_json(url: &str) -> Option<Value> {
    let resp = http().get(url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().ok()
}

fn get_text(url: &str) -> Option<String> {
    let resp = http().get(url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().ok()
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encode for use inside a URL path (spaces become %20, not +).
fn encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ------------------------------------------------------------------ parsers

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn pkg_from_arch(r: &Value) -> Pkg {
    let pkgrel = s(r, "pkgrel");
    Pkg {
        src: Src::Repo,
        repo: s(r, "repo").to_string(),
        name: s(r, "pkgname").to_string(),
        version: format!(
            "{}-{}",
            s(r, "pkgver"),
            if pkgrel.is_empty() { "1" } else { pkgrel }
        ),
        desc: s(r, "pkgdesc").trim().to_string(),
        maintainer: s(r, "packager").to_string(),
        votes: 0,
        popularity: 0.0,
        outdated: false,
        installed: false,
    }
}

fn pkg_from_aur(r: &Value) -> Pkg {
    Pkg {
        src: Src::Aur,
        repo: "aur".to_string(),
        name: s(r, "Name").to_string(),
        version: s(r, "Version").to_string(),
        desc: s(r, "Description").trim().to_string(),
        maintainer: s(r, "Maintainer").to_string(),
        votes: r.get("NumVotes").and_then(Value::as_i64).unwrap_or(0),
        popularity: r.get("Popularity").and_then(Value::as_f64).unwrap_or(0.0),
        outdated: r.get("OutOfDate").is_some_and(|v| !v.is_null()),
        installed: false,
    }
}

fn stub(name: &str) -> Pkg {
    Pkg {
        src: Src::Aur,
        repo: "aur".to_string(),
        name: name.to_string(),
        version: String::new(),
        desc: String::new(),
        maintainer: String::new(),
        votes: 0,
        popularity: 0.0,
        outdated: false,
        installed: false,
    }
}

/// Parse `Key : Value` blocks (pacman -Si / -Qi output), joining indented
/// continuation lines into the value they belong to.
fn parse_kv(text: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) && line.contains(':') {
            if let Some(r) = cur.take() {
                rows.push(r);
            }
            let (k, v) = line.split_once(':').unwrap_or((line, ""));
            cur = Some((k.trim().to_string(), v.trim().to_string()));
        } else if let Some(r) = cur.as_mut() {
            r.1.push(' ');
            r.1.push_str(line.trim());
        }
    }
    if let Some(r) = cur.take() {
        rows.push(r);
    }
    rows
}

fn row(rows: &[(String, String)], key: &str) -> String {
    rows.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

// ----------------------------------------------------------------- searches

/// Official repositories via archlinux.org; falls back to `pacman -Ss` when the
/// network (or the API) is unavailable. Returns (items, network_was_down).
pub fn search_official(q: &str, offline: bool) -> (Vec<Pkg>, bool) {
    let mut net_down = false;
    if !offline {
        let url = format!("{ARCH_JSON}?q={}", urlencode(q));
        match get_json(&url) {
            Some(v) => {
                if let Some(arr) = v.get("results").and_then(Value::as_array) {
                    let items: Vec<Pkg> = arr.iter().map(pkg_from_arch).collect();
                    if !items.is_empty() {
                        return (items, false);
                    }
                }
            }
            None => net_down = true,
        }
    }
    (pacman_search(q), net_down)
}

fn pacman_search(q: &str) -> Vec<Pkg> {
    let out = match Command::new("pacman").args(["-Ss", "--", q]).output() {
        Ok(o) => o.stdout,
        Err(_) => return vec![],
    };
    let text = String::from_utf8_lossy(&out);
    let mut items: Vec<Pkg> = Vec::new();
    let mut cur: Option<Pkg> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(p) = cur.as_mut() {
                p.desc = rest.trim().to_string();
            }
            if let Some(p) = cur.take() {
                items.push(p);
            }
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(rp), Some(ver)) = (it.next(), it.next()) else {
            continue;
        };
        let (repo, name) = match rp.split_once('/') {
            Some(x) => x,
            None => continue,
        };
        let flags: Vec<&str> = it.collect();
        let installed = flags.iter().any(|t| t.starts_with("[installed"));
        if let Some(p) = cur.take() {
            items.push(p);
        }
        cur = Some(Pkg {
            src: Src::Repo,
            repo: repo.to_string(),
            name: name.to_string(),
            version: ver.to_string(),
            desc: String::new(),
            maintainer: String::new(),
            votes: 0,
            popularity: 0.0,
            outdated: false,
            installed,
        });
    }
    if let Some(p) = cur.take() {
        items.push(p);
    }
    items
}

pub fn search_aur(q: &str) -> Vec<Pkg> {
    let url = format!("{AUR_RPC}/search/{}?by=name-desc", encode_path(q));
    match get_json(&url) {
        Some(v) => v
            .get("results")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(pkg_from_aur).collect())
            .unwrap_or_default(),
        None => vec![],
    }
}

fn aur_info(names: &[String]) -> Vec<Value> {
    if names.is_empty() {
        return vec![];
    }
    let query = names
        .iter()
        .map(|n| format!("arg[]={}", urlencode(n)))
        .collect::<Vec<_>>()
        .join("&");
    match get_json(&format!("{AUR_RPC}/info?{query}")) {
        Some(v) => v
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        None => vec![],
    }
}

/// Popular / newest / recently-updated AUR packages: scrape the site listing,
/// then hydrate through the info RPC.
pub fn aur_listing(sort_by: &str, order: &str, limit: usize) -> Vec<Pkg> {
    let url = format!("{AUR_HTML}?SB={sort_by}&SO={order}&PP={}", limit.max(10));
    let page = match get_text(&url) {
        Some(t) => t,
        None => return vec![],
    };
    let mut names: Vec<String> = Vec::new();
    let mut rest = page.as_str();
    const MARK: &str = "href=\"/packages/";
    while let Some(i) = rest.find(MARK) {
        rest = &rest[i + MARK.len()..];
        let end = rest.find('"').unwrap_or(0);
        let name = &rest[..end];
        rest = &rest[end..];
        let ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'));
        if ok && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
            if names.len() >= limit {
                break;
            }
        }
    }
    let vals = aur_info(&names);
    let by_name: HashMap<String, Pkg> = vals
        .into_iter()
        .map(|v| (s(&v, "Name").to_string(), pkg_from_aur(&v)))
        .collect();
    let mut out: Vec<Pkg> = names.iter().filter_map(|n| by_name.get(n).cloned()).collect();
    if out.is_empty() {
        out = names.iter().map(|n| stub(n)).collect();
    }
    out
}

// ------------------------------------------------------------------- detail

pub fn detail_of(name: &str, src: Src) -> Option<Detail> {
    // official repo (works for installed packages too, as long as it is synced)
    if let Ok(o) = Command::new("pacman").args(["-Si", name]).output() {
        if o.status.success() {
            let rows = parse_kv(&String::from_utf8_lossy(&o.stdout));
            let url = row(&rows, "URL");
            return Some(Detail { name: name.to_string(), src: Src::Repo, rows, url });
        }
    }
    if src == Src::Aur {
        if let Some(d) = aur_detail(name) {
            return Some(d);
        }
    }
    // installed locally but no longer in any sync db
    if let Ok(o) = Command::new("pacman").args(["-Qi", name]).output() {
        if o.status.success() {
            let rows = parse_kv(&String::from_utf8_lossy(&o.stdout));
            let url = row(&rows, "URL");
            return Some(Detail { name: name.to_string(), src, rows, url });
        }
    }
    None
}

fn aur_detail(name: &str) -> Option<Detail> {
    let vals = aur_info(&[name.to_string()]);
    let v = vals.into_iter().next()?;
    let arr = |k: &str| -> String {
        v.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .unwrap_or_default()
    };
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, val: String| rows.push((k.to_string(), val));
    push("Version", s(&v, "Version").to_string());
    push("Description", s(&v, "Description").to_string());
    push("Maintainer", s(&v, "Maintainer").to_string());
    push("Votes", s(&v, "NumVotes").to_string());
    push("Popularity", format!("{:.2}", v.get("Popularity").and_then(Value::as_f64).unwrap_or(0.0)));
    push("License", arr("License"));
    push("Depends On", arr("DependsOn"));
    push("Make Deps", arr("MakeDepends"));
    push("Opt Deps", arr("OptDepends"));
    push("Provides", arr("Provides"));
    push("Conflicts With", arr("ConflictsWith"));
    let url = format!("https://aur.archlinux.org/packages/{name}");
    rows.push(("AUR page".to_string(), url.clone()));
    Some(Detail { name: name.to_string(), src: Src::Aur, rows, url })
}

// ------------------------------------------------------------------ install

pub fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Route an install to pacman (official repos) or an AUR helper.
pub fn install_cmd(name: &str) -> Option<(Vec<String>, &'static str)> {
    if which("pacman") {
        let ok = Command::new("pacman")
            .args(["-Si", name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some((
                vec![
                    "sudo".into(),
                    "pacman".into(),
                    "-S".into(),
                    "--needed".into(),
                    name.into(),
                ],
                "official repo",
            ));
        }
    }
    if which("yay") {
        return Some((vec!["yay".into(), "-S".into(), name.into()], "AUR (yay)"));
    }
    if which("paru") {
        return Some((vec!["paru".into(), "-S".into(), name.into()], "AUR (paru)"));
    }
    None
}

/// Uninstall: everything installed lives in pacman's local database, so one
/// command covers repo and AUR packages alike. `-Rns` also drops dependencies
/// that nothing else needs.
pub fn remove_cmd(name: &str) -> (Vec<String>, &'static str) {
    (
        vec![
            "sudo".into(),
            "pacman".into(),
            "-Rns".into(),
            name.into(),
        ],
        "sudo pacman -Rns",
    )
}

// -------------------------------------------------------------- installed db

pub fn installed_names() -> HashSet<String> {
    Command::new("pacman")
        .args(["-Qq"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn installed_pkgs() -> Vec<Pkg> {
    let out = match Command::new("pacman").args(["-Qi"]).output() {
        Ok(o) => o.stdout,
        Err(_) => return vec![],
    };
    let text = String::from_utf8_lossy(&out);
    let mut items: Vec<Pkg> = Vec::new();
    for block in text.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }
        let mut kv: Vec<(String, String)> = Vec::new();
        for line in block.lines() {
            if let Some((k, v)) = line.split_once(':') {
                kv.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let get = |key: &str| -> String {
            kv.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let name = get("Name");
        if name.is_empty() {
            continue;
        }
        let repo = get("Repository");
        items.push(Pkg {
            src: Src::Repo,
            repo: if repo.is_empty() { "local".into() } else { repo },
            name,
            version: get("Version"),
            desc: get("Description"),
            maintainer: get("Maintainer"),
            votes: 0,
            popularity: 0.0,
            outdated: false,
            installed: true,
        });
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items
}

// ------------------------------------------------------------------- ranking

fn ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                prev[j].max(cur[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
        cur.iter_mut().for_each(|x| *x = 0);
    }
    let m = prev[b.len()] as f64;
    2.0 * m / (a.len() + b.len()) as f64
}

pub fn score(q: &str, p: &Pkg) -> f64 {
    let ql = q.trim().to_lowercase();
    if ql.is_empty() {
        return 0.0;
    }
    let name = p.name.to_lowercase();
    let desc = p.desc.to_lowercase();
    let mut s = 0.0f64;
    if name == ql {
        s = 100.0;
    } else if name.starts_with(&ql) {
        s = 70.0;
    } else if name.split(['-', '_']).any(|w| w == ql) {
        s = 58.0;
    } else if name.contains(&ql) {
        s = 45.0;
    } else if desc.contains(&ql) {
        s = 22.0;
    } else {
        let r = ratio(&ql, &name);
        if r > 0.55 {
            s = r * 25.0;
        }
    }
    if s > 0.0 {
        if p.src == Src::Aur {
            s += ((p.votes.max(0) as f64).sqrt() * 0.35 + p.popularity * 1.5).min(15.0);
        }
        if matches!(p.repo.as_str(), "core" | "extra" | "multilib") {
            s += 3.0;
        }
    }
    s
}

pub fn rank(q: &str, items: Vec<Pkg>, limit: usize) -> Vec<Pkg> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut scored: Vec<(f64, Pkg)> = Vec::new();
    for p in items {
        if p.name.is_empty() || !seen.insert(p.name.clone()) {
            continue;
        }
        scored.push((score(q, &p), p));
    }
    if scored.iter().any(|(s, _)| *s > 0.0) {
        scored.retain(|(s, _)| *s > 0.0);
    }
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.votes.cmp(&a.1.votes))
            .then(a.1.name.cmp(&b.1.name))
    });
    scored.into_iter().take(limit).map(|(_, p)| p).collect()
}

// ------------------------------------------------------------- text helpers

pub fn dwidth(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn pad(s: &str, w: usize) -> String {
    let mut out = s.to_string();
    let dw = dwidth(s);
    if dw < w {
        out.push_str(&" ".repeat(w - dw));
    }
    out
}

pub fn truncate(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if dwidth(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut acc = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
        if acc + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        acc += cw;
    }
    out.push('…');
    out
}
