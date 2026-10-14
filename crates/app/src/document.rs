use leptos::prelude::*;

use crate::fmt;
use crate::state::{AppState, Crumb, DocMode};
use crate::worker::{use_query, Entry, ObjKind, Req, Resp, Value};

#[component]
pub fn DocumentTab() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        app.doc.with(|d| match d {
            None => view! { <div class="card busy-wrap busy"><p class="dim">"Loading document…"</p></div> }.into_any(),
            Some(Err(e)) => view! {
                <div class="card error">"automerge could not load this file: " {e.clone()}</div>
            }
            .into_any(),
            Some(Ok(sum)) => {
                let changes = sum.changes;
                let tab = move |mode: DocMode, label: &'static str| view! {
                    <button class:active=move || app.doc_mode.get() == mode on:click=move |_| app.doc_mode.set(mode)>
                        <span>{label}</span>
                    </button>
                };
                view! {
                    <Scrubber changes=changes/>
                    <DocBrowser/>
                    <nav class="side-tabs">
                        {tab(DocMode::Value, "Value")}{tab(DocMode::Ops, "Ops")}{tab(DocMode::Json, "JSON")}
                        <label class="side-check" title="Keep ops after this version, struck through">
                            <input type="checkbox" prop:checked=move || app.tombstones.get()
                                on:change=move |ev| app.tombstones.set(event_target_checked(&ev))/>
                            <span>"show tombstones"</span>
                        </label>
                    </nav>
                }
                .into_any()
            }
        })
    }
}

/// One pane per open object, from the root down; every pane but the last
/// is squashed into a band of its entries.
#[component]
fn DocBrowser() -> impl IntoView {
    let app = expect_context::<AppState>();
    let levels = move || {
        let root = Crumb { key: "/".into(), id: "_root".into(), kind: ObjKind::Map, len: 0 };
        std::iter::once(root).chain(app.doc_path.get()).enumerate().collect::<Vec<_>>()
    };
    view! {
        <div class="doc-view">
            <For each=levels key=|(level, c)| (*level, c.id.clone())
                children=|(level, c)| view! { <DocLevel level=level crumb=c/> }/>
        </div>
    }
}

const PANE_LIMIT: usize = 500;
const BAND_STRIPES: usize = 200;
const OPS_PAGE: usize = 200;
const JSON_LIMIT: usize = 2 << 20;
const TEXT_CHARS: usize = 20_000;

#[component]
fn DocLevel(level: usize, crumb: Crumb) -> impl IntoView {
    let app = expect_context::<AppState>();
    let band = move || app.doc_path.with(|p| p.len() > level);
    let limit = RwSignal::new(PANE_LIMIT);
    let text_chars = RwSignal::new(TEXT_CHARS);
    let ops_page = RwSignal::new(0usize);
    let Crumb { key: title, id: obj, kind, .. } = crumb;
    let (band_data, _) = {
        let obj = obj.clone();
        use_query(format!("band:{level}"), move || {
            (band() && kind != ObjKind::Text)
                .then(|| Req::Page { obj: obj.clone(), step: app.step.get(), limit: BAND_STRIPES + 1, tombstones: false })
        })
    };
    let (pane_data, busy) = {
        let obj = obj.clone();
        use_query(format!("pane:{level}"), move || {
            if band() {
                return None;
            }
            let (obj, step) = (obj.clone(), app.step.get());
            let tombstones = app.tombstones.get() && app.drag_from.get().is_none();
            Some(match app.doc_mode.get() {
                DocMode::Ops => {
                    let upto = (!app.tombstones.get()).then(|| app.drag_from.get().map_or(step, |f| f.max(step)));
                    Req::Ops { obj, upto, start: ops_page.get() * OPS_PAGE, limit: OPS_PAGE }
                }
                DocMode::Json => Req::Json { obj, step, limit: JSON_LIMIT },
                DocMode::Value if kind == ObjKind::Text => Req::Text { obj, step, limit_chars: text_chars.get(), tombstones },
                DocMode::Value => Req::Page { obj, step, limit: limit.get(), tombstones },
            })
        })
    };
    let (base_data, _) = {
        let obj = obj.clone();
        use_query(format!("base:{level}"), move || {
            let from = app.drag_from.get()?;
            if band() {
                return None;
            }
            let obj = obj.clone();
            match app.doc_mode.get() {
                DocMode::Ops => None,
                DocMode::Json => Some(Req::Json { obj, step: from, limit: JSON_LIMIT }),
                DocMode::Value if kind == ObjKind::Text => Some(Req::Text { obj, step: from, limit_chars: text_chars.get(), tombstones: false }),
                DocMode::Value => Some(Req::Page { obj, step: from, limit: limit.get(), tombstones: false }),
            }
        })
    };
    let diffing = move || app.drag_from.get().is_some_and(|f| f != app.step.get());
    let band_view = {
        let title = title.clone();
        move || {
            let sel = app.doc_path.with(|p| p.get(level).map(|c| c.key.clone()));
            let stripes = band_data.with(|d| match d {
                Some(Resp::Page { entries, total }) if *total <= BAND_STRIPES => Some(
                    entries
                        .iter()
                        .map(|e| {
                            let key = if kind == ObjKind::List { format!("[{}]", e.key) } else { e.key.clone() };
                            let is_sel = sel.as_deref() == Some(key.as_str());
                            let target = match &e.value {
                                Value::Object { id, kind, len } if !is_sel => {
                                    Some(Crumb { key: key.clone(), id: id.clone(), kind: *kind, len: *len })
                                }
                                _ => None,
                            };
                            let is_obj = matches!(e.value, Value::Object { .. });
                            view! {
                                <div class="doc-stripe" class:obj=is_obj class:sel=is_sel title=key
                                    on:click=move |ev| {
                                        ev.stop_propagation();
                                        open_at(app, level, target.clone());
                                    }></div>
                            }
                        })
                        .collect_view(),
                ),
                _ => None,
            });
            view! {
                <div class="doc-band" on:click=move |_| open_at(app, level, None)>
                    {stripes}
                    <span class="doc-band-title">{title.clone()}</span>
                </div>
            }
        }
    };
    let pane_view = move || {
        let head = |extra: String| view! {
            <div class="card-title">{pane_title(&title, &obj)}<span class="dim">{extra}</span></div>
        };
        let base = if diffing() { base_data.get() } else { None };
        let body = pane_data.with(|d| match d {
            None => view! { <div class="card doc-pane">{head(String::new())}</div> }.into_any(),
            Some(Resp::Text { text, chars, marks, spans }) => {
                let chars = *chars;
                view! {
                    <div class="card doc-pane text-pane">
                        {head(obj_label(ObjKind::Text, chars))}
                        <pre class="text">
                            {match &base {
                                Some(Resp::Text { text: old, .. }) => diff_text(old, text).into_any(),
                                _ => match spans {
                                    Some(spans) => spans.iter().map(|(t, gone)| if *gone {
                                        view! { <del class="tomb">{t.clone()}</del> }.into_any()
                                    } else {
                                        t.clone().into_any()
                                    }).collect_view().into_any(),
                                    None => text.clone().into_any(),
                                },
                            }}
                            {(chars > text.chars().count()).then(|| "…")}
                        </pre>
                        {(chars > text_chars.get_untracked()).then(|| view! {
                            <button class="link" on:click=move |_| text_chars.update(|l| *l *= 4)>{format!("show more of {} chars", fmt::count(chars))}</button>
                        })}
                        {(!marks.is_empty()).then(|| view! {
                            <div class="marks">
                                <span class="dim">{format!("{} marks: ", marks.len())}</span>
                                {marks.iter().take(30).map(|mk| view! { <span class="mark">{mk.clone()}</span> }).collect_view()}
                            </div>
                        })}
                    </div>
                }
                .into_any()
            }
            Some(Resp::Page { entries, total }) => {
                let total = *total;
                let list = kind == ObjKind::List;
                let shown = entries.iter().filter(|e| !e.deleted).count();
                let marked = match &base {
                    Some(Resp::Page { entries: old, .. }) if list => diff_list(old, entries),
                    Some(Resp::Page { entries: old, .. }) => diff_map(old, entries),
                    _ => entries.iter().map(|e| (if e.deleted { Mark::Tomb } else { Mark::Same }, e.clone())).collect(),
                };
                let items = if list {
                    let items = marked.into_iter().enumerate().map(|(i, (mark, e))| view! {
                        {(i > 0).then(|| view! { <span class="sep">", "</span> })}
                        <span class=mark.class()><DocValue entry=e level=level in_list=true/></span>
                    }).collect_view();
                    view! {
                        <div class="doc-list mono">
                            <span class="sep">"[ "</span>{items}<span class="sep">{if total > shown { " … ]" } else { " ]" }}</span>
                        </div>
                    }
                    .into_any()
                } else {
                    let rows = marked.into_iter().map(|(mark, e)| view! {
                        <div class=format!("doc-key {}", mark.class())>{e.key.clone()}</div>
                        <div class=format!("doc-val {}", mark.class())><DocValue entry=e level=level in_list=false/></div>
                    }).collect_view();
                    view! { <div class="doc-map mono">{rows}</div> }.into_any()
                };
                view! {
                    <div class="card doc-pane">
                        {head(obj_label(kind, total))}
                        {items}
                        {entries.is_empty().then(|| view! { <p class="dim">"(empty)"</p> })}
                        {(total > shown).then(|| view! {
                            <button class="link more" on:click=move |_| limit.update(|l| *l += PANE_LIMIT)>
                                {format!("show more ({shown} of {})", fmt::count(total))}
                            </button>
                        })}
                    </div>
                }
                .into_any()
            }
            Some(Resp::Json { json, cut }) => view! {
                <div class="card doc-pane text-pane">
                    {head("JSON".into())}
                    <pre class="json">
                        {match &base {
                            Some(Resp::Json { json: old, .. }) => diff_text(old, json).into_any(),
                            _ => json.clone().into_any(),
                        }}
                        {cut.then(|| format!("\n… cut off after {}", fmt::bytes(JSON_LIMIT)))}
                    </pre>
                </div>
            }
            .into_any(),
            Some(Resp::Ops { ops, total }) => {
                let total = *total;
                let pages = total.div_ceil(OPS_PAGE);
                let rows = ops.iter().map(|op| {
                    let change = op.change;
                    view! {
                        <tr title=op.origin.clone()
                            class:future=move || change.is_some_and(|c| c >= app.step.get()) && drag_moved(app, change).is_none()
                            class:diff-add=move || drag_moved(app, change) == Some(true)
                            class:diff-del=move || drag_moved(app, change) == Some(false)>
                            <td>{op.id.clone()}</td>
                            <td>{op.key.clone()}</td>
                            <td>{op.action.clone()}</td>
                            <td>{op.pred.clone()}</td>
                        </tr>
                    }
                }).collect_view();
                view! {
                    <div class="card doc-pane">
                        <div class="card-title">
                            {pane_title(&title, &obj)}
                            <span class="dim">{format!("{} ops", fmt::count_exact(total))}</span>
                            {(pages > 1).then(|| view! {
                                <span class="pager">
                                    <button disabled=move || ops_page.get() == 0 on:click=move |_| ops_page.update(|p| *p -= 1)>"‹"</button>
                                    <span>{move || format!("page {} / {pages}", ops_page.get() + 1)}</span>
                                    <button disabled=move || { ops_page.get() + 1 >= pages } on:click=move |_| ops_page.update(|p| *p += 1)>"›"</button>
                                </span>
                            })}
                        </div>
                        <table class="dense rows mono">
                            <thead><tr><th>"id"</th><th>"key"</th><th>"Action"</th><th>"pred"</th></tr></thead>
                            <tbody>{rows}</tbody>
                        </table>
                    </div>
                }
                .into_any()
            }
            Some(other) => view! { <div class="card error">{format!("{other:?}")}</div> }.into_any(),
        });
        view! { <div class="busy-wrap" class:busy=move || busy.get()>{body}</div> }
    };
    view! {
        <div class="doc-level" class:band=band>
            {move || if band() { band_view.clone()().into_any() } else { pane_view.clone()().into_any() }}
        </div>
    }
}

/// How a value differs from the version the history bar's drag started at.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Same,
    Added,
    Removed,
    Tomb,
}

impl Mark {
    fn class(self) -> &'static str {
        match self {
            Mark::Same => "",
            Mark::Added => "diff-add",
            Mark::Removed => "diff-del",
            Mark::Tomb => "tomb",
        }
    }
}

fn unchanged(a: &Entry, b: &Entry) -> bool {
    a.id == b.id && (matches!(a.value, Value::Object { .. }) || a.value == b.value)
}

/// Map entries of both versions in key order, marking what came and went.
fn diff_map(old: &[Entry], new: &[Entry]) -> Vec<(Mark, Entry)> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(new.len());
    loop {
        match (old.get(i), new.get(j)) {
            (Some(a), Some(b)) if a.key == b.key => {
                if unchanged(a, b) {
                    out.push((Mark::Same, b.clone()));
                } else {
                    out.push((Mark::Removed, a.clone()));
                    out.push((Mark::Added, b.clone()));
                }
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) if a.key < b.key => {
                out.push((Mark::Removed, a.clone()));
                i += 1;
            }
            (_, Some(b)) => {
                out.push((Mark::Added, b.clone()));
                j += 1;
            }
            (Some(a), None) => {
                out.push((Mark::Removed, a.clone()));
                i += 1;
            }
            (None, None) => return out,
        }
    }
}

/// List elements of both versions in order, matched by the op that set them.
fn diff_list(old: &[Entry], new: &[Entry]) -> Vec<(Mark, Entry)> {
    let old_ids: std::collections::HashSet<&str> = old.iter().map(|e| e.id.as_str()).collect();
    let new_ids: std::collections::HashSet<&str> = new.iter().map(|e| e.id.as_str()).collect();
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(new.len());
    loop {
        match (old.get(i), new.get(j)) {
            (Some(a), Some(b)) if a.id == b.id => {
                if unchanged(a, b) {
                    out.push((Mark::Same, b.clone()));
                } else {
                    out.push((Mark::Removed, a.clone()));
                    out.push((Mark::Added, b.clone()));
                }
                i += 1;
                j += 1;
            }
            (Some(a), _) if !new_ids.contains(a.id.as_str()) => {
                out.push((Mark::Removed, a.clone()));
                i += 1;
            }
            (_, Some(b)) if !old_ids.contains(b.id.as_str()) => {
                out.push((Mark::Added, b.clone()));
                j += 1;
            }
            (Some(_), Some(b)) => {
                out.push((Mark::Same, b.clone()));
                i += 1;
                j += 1;
            }
            (Some(a), None) => {
                out.push((Mark::Removed, a.clone()));
                i += 1;
            }
            (None, Some(b)) => {
                out.push((Mark::Added, b.clone()));
                j += 1;
            }
            (None, None) => return out,
        }
    }
}

/// `new` with the stretch that differs from `old` shown as removed and added text.
fn diff_text(old: &str, new: &str) -> impl IntoView {
    let mut pre = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
        pre -= 1;
    }
    let room = old.len().min(new.len()) - pre;
    let mut suf = old.bytes().rev().zip(new.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - suf) || !new.is_char_boundary(new.len() - suf) {
        suf -= 1;
    }
    view! {
        {new[..pre].to_string()}
        <del class="diff-del">{old[pre..old.len() - suf].to_string()}</del>
        <ins class="diff-add">{new[pre..new.len() - suf].to_string()}</ins>
        {new[new.len() - suf..].to_string()}
    }
}

/// While the history bar is dragged: `Some(true)` for an op made since the
/// drag started, `Some(false)` for one undone by dragging back.
fn drag_moved(app: AppState, change: Option<usize>) -> Option<bool> {
    let (c, from, now) = (change?, app.drag_from.get()?, app.step.get());
    match (c < from, c < now) {
        (false, true) => Some(true),
        (true, false) => Some(false),
        _ => None,
    }
}

/// A pane's name with its object id, e.g. `items 2@f018`.
fn pane_title(title: &str, obj: &str) -> impl IntoView {
    let short = match obj.split_once('@') {
        Some((ctr, actor)) => format!("{ctr}@{}", &actor[..actor.len().min(4)]),
        None => "ROOT".into(),
    };
    view! { <b class="mono">{title.to_string()}</b><span class="mono obj-id" title=obj.to_string()>{short}</span> }
}

/// A bar across the top of the document: dragging it shows the document as
/// it was after that many changes.
#[component]
fn Scrubber(changes: usize) -> impl IntoView {
    let app = expect_context::<AppState>();
    let n = changes;
    let (time, _) = use_query("scrub".into(), move || {
        let k = app.step.get();
        (k > 0 && k < n).then(|| Req::ChangeTime { index: k - 1 })
    });
    let label = move || match app.step.get() {
        0 => format!("0 / {n} changes"),
        k if k >= n => format!("{n} / {n} changes · now"),
        k => match time.get() {
            Some(Resp::Time(t)) => format!("{k} / {n} changes · {t}"),
            _ => format!("{k} / {n} changes"),
        },
    };
    view! {
        <div class="scrubber">
            <input type="range" min="0" max=n.to_string() prop:value=move || app.step.get().to_string()
                on:pointerdown=move |_| app.drag_from.set(Some(app.step.get_untracked()))
                on:pointerup=move |_| app.drag_from.set(None)
                on:change=move |_| app.drag_from.set(None)
                on:input=move |ev| app.step.set(event_target_value(&ev).parse().unwrap_or(n))/>
            <span>{label}</span>
        </div>
    }
}

/// Opens `target` below `level`, or with `None` makes `level` the open pane again.
fn open_at(app: AppState, level: usize, target: Option<Crumb>) {
    app.doc_path.update(|p| {
        p.truncate(level);
        p.extend(target);
    });
    window().scroll_to_with_x_and_y(0.0, 0.0);
}

/// A value in a pane: scalars in their type's colour, objects as a chip that opens them.
#[component]
fn DocValue(entry: Entry, level: usize, in_list: bool) -> impl IntoView {
    let app = expect_context::<AppState>();
    let conflict = entry.conflict.then(|| view! { <span class="conflict" title="concurrent values; showing the winner">"⚡"</span> });
    match entry.value {
        Value::Scalar { text, kind } => view! {
            <span class=format!("val v-{kind}") title=entry.origin>{text}</span>{conflict}
        }
        .into_any(),
        Value::Object { id, kind, len } => {
            let key = if in_list { format!("[{}]", entry.key) } else { entry.key.clone() };
            let crumb = Crumb { key, id, kind, len };
            let sel = {
                let crumb = crumb.clone();
                move || app.doc_path.with(|p| p.get(level) == Some(&crumb))
            };
            view! {
                <button class="obj-chip" class:sel=sel title=entry.origin
                    on:click=move |_| open_at(app, level, Some(crumb.clone()))>{obj_label(kind, len)}</button>
                {conflict}
            }
            .into_any()
        }
    }
}

fn obj_label(kind: ObjKind, len: usize) -> String {
    match kind {
        ObjKind::Map => format!("{{map {len}}}"),
        ObjKind::Table => format!("{{table {len}}}"),
        ObjKind::List => format!("[list {len}]"),
        ObjKind::Text => format!("text {len} chars"),
    }
}
