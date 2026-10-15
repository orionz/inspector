//! The location hash: which document is open and where the UI is in it, so
//! back and forward step through UI states and a link reopens the same view.

use leptos::prelude::*;
use wasm_bindgen::JsValue;

use crate::state::{AppState, ColumnRef, Crumb, DocMode, GroupView, Part, Tab};
use crate::worker::ObjKind;

/// Hash parameters, in the order they're written.
pub type Params = Vec<(String, String)>;

/// Where the open document came from, as the hash names it.
#[derive(Clone, PartialEq, Debug)]
pub enum Source {
    Sample(String),
    /// A Patchwork document, by its `automerge:…` id.
    Doc { id: String, title: Option<String> },
    /// A file opened from disk, by the start of its SHA-256.
    File { sha: String, name: String },
}

impl Source {
    fn params(&self) -> Params {
        let p = |k: &str, v: &str| (k.to_string(), v.to_string());
        match self {
            Source::Sample(name) => vec![p("sample", name)],
            Source::Doc { id, title } => std::iter::once(p("doc", &format!("automerge:{id}")))
                .chain(title.as_deref().map(|t| p("title", t)))
                .collect(),
            Source::File { sha, name } => vec![p("file", sha), p("name", name)],
        }
    }

    fn from_params(params: &Params) -> Option<Self> {
        let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        if let Some(name) = get("sample") {
            return Some(Source::Sample(name));
        }
        if let Some(doc) = get("doc") {
            let id = crate::patchwork::doc_id(&doc)?;
            return Some(Source::Doc { id, title: get("title") });
        }
        Some(Source::File { sha: get("file")?, name: get("name").unwrap_or_default() })
    }
}

/// How many hex digits of a file's SHA-256 a link keeps.
const SHA_LEN: usize = 16;

/// The [`Source`] for a file opened from disk.
pub fn file_source(name: String, bytes: &[u8]) -> Source {
    use sha2::{Digest, Sha256};
    let sha = hex(&Sha256::digest(bytes))[..SHA_LEN].to_string();
    Source::File { sha, name }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn parse(hash: &str) -> Params {
    hash.trim_start_matches('#')
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn decode(s: &str) -> String {
    js_sys::decode_uri_component(&s.replace('+', " "))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| s.to_string())
}

fn encode(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}

fn write(params: &Params) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

fn current_hash() -> String {
    window().location().hash().unwrap_or_default().trim_start_matches('#').to_string()
}

/// The source in the current location, if any.
pub fn location_source() -> Option<(Source, Params)> {
    let params = parse(&current_hash());
    Source::from_params(&params).map(|s| (s, params))
}

fn part_key(p: Part) -> &'static str {
    match p {
        Part::Changes => "changes",
        Part::Ops => "ops",
        Part::Extra => "extra",
    }
}

fn part_of(s: &str) -> Option<Part> {
    match s {
        "changes" => Some(Part::Changes),
        "ops" => Some(Part::Ops),
        "extra" => Some(Part::Extra),
        _ => None,
    }
}

fn col_key(c: ColumnRef) -> String {
    format!("{}{}", if c.ops { 'o' } else { 'c' }, c.index)
}

fn col_of(s: &str) -> Option<ColumnRef> {
    let ops = match s.chars().next()? {
        'o' => true,
        'c' => false,
        _ => return None,
    };
    Some(ColumnRef { ops, index: s[1..].parse().ok()? })
}

fn gv_char(g: GroupView) -> char {
    match g {
        GroupView::Physical => 'p',
        GroupView::Logical => 'l',
        GroupView::Raw => 'r',
    }
}

fn gv_of(c: char) -> GroupView {
    match c {
        'l' => GroupView::Logical,
        'r' => GroupView::Raw,
        _ => GroupView::Physical,
    }
}

fn kind_key(k: ObjKind) -> &'static str {
    match k {
        ObjKind::Map => "map",
        ObjKind::List => "list",
        ObjKind::Text => "text",
        ObjKind::Table => "table",
    }
}

fn kind_of(s: &str) -> Option<ObjKind> {
    match s {
        "map" => Some(ObjKind::Map),
        "list" => Some(ObjKind::List),
        "text" => Some(ObjKind::Text),
        "table" => Some(ObjKind::Table),
        _ => None,
    }
}

/// The open object's path: one `key:id:kind:len` per object, joined by `/`.
fn path_key(path: &[Crumb]) -> String {
    path.iter()
        .map(|c| format!("{}:{}:{}:{}", encode(&c.key), encode(&c.id), kind_key(c.kind), c.len))
        .collect::<Vec<_>>()
        .join("/")
}

fn path_of(s: &str) -> Vec<Crumb> {
    s.split('/')
        .filter_map(|c| {
            let mut f = c.split(':');
            let (key, id, kind, len) = (f.next()?, f.next()?, f.next()?, f.next()?);
            Some(Crumb { key: decode(key), id: decode(id), kind: kind_of(kind)?, len: len.parse().ok()? })
        })
        .collect()
}

/// The UI state worth a link, tracking every signal it reads.
fn view_params(app: AppState) -> Params {
    let mut out: Params = vec![];
    let mut put = |k: &str, v: String| out.push((k.to_string(), v));
    match app.tab.get() {
        Tab::Load => {}
        Tab::Document => put("tab", "doc".into()),
        Tab::Save => put("tab", "save".into()),
    }
    let mut selection = |prefix: &str, chunk: Option<usize>, part: Option<Part>, col: Option<ColumnRef>, gv: (GroupView, GroupView)| {
        let Some(c) = chunk else { return };
        put(&format!("{prefix}chunk"), c.to_string());
        if let Some(p) = part {
            put(&format!("{prefix}part"), part_key(p).into());
        }
        if gv != (GroupView::Physical, GroupView::Physical) {
            put(&format!("{prefix}view"), format!("{}{}", gv_char(gv.0), gv_char(gv.1)));
        }
        if let Some(col) = col {
            put(&format!("{prefix}col"), col_key(col));
        }
    };
    selection("", app.chunk.get(), app.part.get(), app.column.get(), app.group_view.get());
    let (sc, sp, scol, sgv) = app.saved_sel;
    selection("s", sc.get(), sp.get(), scol.get(), sgv.get());
    if app.in_memory.get() {
        put("mem", "1".into());
        if let Some((_, node, slab)) = app.mem_pick.get() {
            put("node", node.to_string());
            if let Some(s) = slab {
                put("slab", s.to_string());
            }
        }
    }
    let path = app.doc_path.get();
    if !path.is_empty() {
        put("path", path_key(&path));
    }
    match app.doc_mode.get() {
        DocMode::Value => {}
        DocMode::Ops => put("mode", "ops".into()),
        DocMode::Json => put("mode", "json".into()),
    }
    let changes = app.doc.with(|d| d.as_ref().and_then(|d| d.as_ref().ok()).map(|d| d.changes));
    let step = match changes {
        Some(n) => Some(app.step.get()).filter(|&s| s != n),
        None => app.pending_step.get_value(),
    };
    if let Some(s) = step {
        put("step", s.to_string());
    }
    if app.tombstones.get() {
        put("tomb", "1".into());
    }
    out
}

/// Sets the UI to what `params` describe, for the document already open.
pub fn apply(app: AppState, params: &Params) {
    let get = |k: &str| params.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    app.tab.set(match get("tab") {
        Some("doc") => Tab::Document,
        Some("save") => Tab::Save,
        _ => Tab::Load,
    });
    let gv = |k: &str| {
        let mut c = get(k).unwrap_or("pp").chars();
        (gv_of(c.next().unwrap_or('p')), gv_of(c.next().unwrap_or('p')))
    };
    let chunk = |k: &str| get(k).and_then(|v| v.parse().ok());
    app.chunk.set(chunk("chunk"));
    app.part.set(get("part").and_then(part_of));
    app.group_view.set(gv("view"));
    app.column.set(get("col").and_then(col_of));
    let (sc, sp, scol, sgv) = app.saved_sel;
    sc.set(chunk("schunk"));
    sp.set(get("spart").and_then(part_of));
    sgv.set(gv("sview"));
    scol.set(get("scol").and_then(col_of));
    app.in_memory.set(get("mem").is_some());
    let col = if get("tab") == Some("save") { scol.get_untracked() } else { app.column.get_untracked() };
    app.mem_pick.set(col.zip(chunk("node")).map(|(c, n)| (c, n, chunk("slab"))));
    app.doc_path.set(get("path").map(path_of).unwrap_or_default());
    app.doc_mode.set(match get("mode") {
        Some("ops") => DocMode::Ops,
        Some("json") => DocMode::Json,
        _ => DocMode::Value,
    });
    app.tombstones.set(get("tomb").is_some());
    let step = chunk("step");
    if app.doc.with_untracked(|d| d.as_ref().is_some_and(|d| d.is_ok())) {
        if let Some(s) = step {
            app.step.set(s);
        }
    } else {
        app.pending_step.set_value(step);
    }
}

/// Keeps the location hash in step with the UI: each change pushes a
/// history entry, except quick successions (a dragged slider) which
/// replace the last one.
pub fn sync(app: AppState) {
    let last_push = StoredValue::new(0.0f64);
    Effect::new(move |_| {
        let source = app.source.get();
        let file_open = app.file.with(|f| f.is_some());
        let mut params = match (&source, file_open) {
            (Some(s), true) => s.params(),
            _ => vec![],
        };
        if file_open {
            params.extend(view_params(app));
        }
        if app.pending.with_value(|p| p.is_some()) {
            return;
        }
        let hash = write(&params);
        if hash == current_hash() {
            return;
        }
        let Ok(history) = window().history() else { return };
        let url = if hash.is_empty() {
            window().location().pathname().unwrap_or_default() + &window().location().search().unwrap_or_default()
        } else {
            format!("#{hash}")
        };
        let now = js_sys::Date::now();
        let quick = now - last_push.get_value() < 600.0;
        if quick || app.navigating.get_value() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
        } else {
            let _ = history.push_state_with_url(&JsValue::NULL, "", Some(&url));
        }
        app.navigating.set_value(false);
        last_push.set_value(now);
    });
}
