use am_inspect::doc::{DocModel, Entry, NodeValue};
use am_inspect::format_time;
use automerge::{ObjType, ROOT};
use leptos::prelude::*;

use crate::fmt;
use crate::state::{AppState, Crumb, DocMode};

fn with_model<R>(app: AppState, f: impl FnOnce(&DocModel) -> R) -> Option<R> {
    app.model.with(|m| match m.as_deref() {
        Some(Ok(m)) => Some(f(m)),
        _ => None,
    })
}

fn with_model_untracked<R>(app: AppState, f: impl FnOnce(&DocModel) -> R) -> Option<R> {
    app.model.with_untracked(|m| match m.as_deref() {
        Some(Ok(m)) => Some(f(m)),
        _ => None,
    })
}

#[component]
pub fn DocumentTab() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        app.model.with(|m| match m.as_deref() {
            None => view! { <div class="card dim">"Loading document…"</div> }.into_any(),
            Some(Err(e)) => view! {
                <div class="card error">"automerge could not load this file: " {e.clone()}</div>
            }
            .into_any(),
            Some(Ok(_)) => {
                let tab = move |mode: DocMode, label: &'static str| view! {
                    <button class:active=move || app.doc_mode.get() == mode on:click=move |_| app.doc_mode.set(mode)>
                        <span>{label}</span>
                    </button>
                };
                view! {
                    <DocBrowser/>
                    <nav class="side-tabs">{tab(DocMode::Value, "Value")}{tab(DocMode::Ops, "Ops")}{tab(DocMode::Json, "JSON")}</nav>
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
        let root = Crumb { key: "ROOT".into(), id: ROOT, ty: ObjType::Map, len: 0 };
        std::iter::once(root).chain(app.doc_path.get()).enumerate().collect::<Vec<_>>()
    };
    view! {
        <div class="doc-view">
            <For each=levels key=|(level, c)| (*level, format!("{:?}", c.id))
                children=|(level, c)| view! { <DocLevel level=level crumb=c/> }/>
        </div>
    }
}

const PANE_LIMIT: usize = 500;
const BAND_STRIPES: usize = 200;

#[component]
fn DocLevel(level: usize, crumb: Crumb) -> impl IntoView {
    let app = expect_context::<AppState>();
    let band = move || app.doc_path.with(|p| p.len() > level);
    let limit = RwSignal::new(PANE_LIMIT);
    let obj = crumb.id.clone();
    let ty = crumb.ty;
    let title = crumb.key.clone();
    let body = move || {
        let heads = app.heads.get();
        let heads = heads.as_deref();
        if band() {
            let sel = app.doc_path.with(|p| p.get(level).map(|c| c.key.clone()));
            return with_model(app, |m| {
                let page = m.page(&obj, heads, 0, BAND_STRIPES + 1);
                let stripes = (page.total <= BAND_STRIPES && ty != ObjType::Text).then(|| {
                    page.entries.into_iter().map(|e| {
                        let key = if ty == ObjType::List { format!("[{}]", e.key) } else { e.key.clone() };
                        let is_sel = sel.as_deref() == Some(key.as_str());
                        let target = match &e.value {
                            NodeValue::Object { id, ty, len } if !is_sel => Some(Crumb { key: key.clone(), id: id.clone(), ty: *ty, len: *len }),
                            _ => None,
                        };
                        let is_obj = matches!(e.value, NodeValue::Object { .. });
                        view! {
                            <div class="doc-stripe" class:obj=is_obj class:sel=is_sel title=key
                                on:click=move |ev| {
                                    ev.stop_propagation();
                                    open_at(app, level, target.clone());
                                }></div>
                        }
                    }).collect_view()
                });
                view! {
                    <div class="doc-band" on:click=move |_| open_at(app, level, None)>
                        {stripes}
                        <span class="doc-band-title">{title.clone()}</span>
                    </div>
                }
                .into_any()
            });
        }
        match app.doc_mode.get() {
            DocMode::Ops => return Some(view! { <ObjOps obj=obj.clone() title=title.clone()/> }.into_any()),
            DocMode::Json => {
                return with_model(app, |m| {
                    let (json, cut) = m.json(&obj, heads, JSON_LIMIT);
                    view! {
                        <div class="card doc-pane text-pane">
                            <div class="card-title"><b class="mono">{title.clone()}</b><span class="dim">"JSON"</span></div>
                            <pre class="json">{json}{cut.then(|| format!("\n… cut off after {}", fmt::bytes(JSON_LIMIT)))}</pre>
                        </div>
                    }
                    .into_any()
                });
            }
            DocMode::Value => {}
        }
        with_model(app, |m| {
            let header = |len: usize| view! {
                <div class="card-title"><b class="mono">{title.clone()}</b><span class="dim">{obj_label(ty, len)}</span></div>
            };
            if ty == ObjType::Text {
                let text = m.text(&obj, heads);
                let chars = text.chars().count();
                let marks = m.marks(&obj, heads);
                let shown: String = text.chars().take(limit.get() * 40).collect();
                return view! {
                    <div class="card doc-pane text-pane">
                        {header(chars)}
                        <pre class="text">{shown}{(chars > limit.get() * 40).then(|| "…")}</pre>
                        {(chars > limit.get() * 40).then(|| view! {
                            <button class="link" on:click=move |_| limit.update(|l| *l *= 4)>{format!("show more of {} chars", fmt::count(chars))}</button>
                        })}
                        {(!marks.is_empty()).then(|| view! {
                            <div class="marks">
                                <span class="dim">{format!("{} marks: ", marks.len())}</span>
                                {marks.iter().take(30).map(|mk| view! { <span class="mark">{mk.clone()}</span> }).collect_view()}
                            </div>
                        })}
                    </div>
                }
                .into_any();
            }
            let page = m.page(&obj, heads, 0, limit.get());
            let total = page.total;
            let more = (total > limit.get()).then(|| view! {
                <button class="link more" on:click=move |_| limit.update(|l| *l += PANE_LIMIT)>
                    {format!("show more ({} of {})", limit.get_untracked(), fmt::count(total))}
                </button>
            });
            let list = matches!(ty, ObjType::List);
            let entries = if list {
                let items = page.entries.into_iter().enumerate().map(|(i, e)| view! {
                    {(i > 0).then(|| view! { <span class="sep">", "</span> })}
                    <DocValue entry=e level=level in_list=true/>
                }).collect_view();
                view! { <div class="doc-list mono"><span class="sep">"[ "</span>{items}<span class="sep">{if total > limit.get() { " … ]" } else { " ]" }}</span></div> }.into_any()
            } else {
                let rows = page.entries.into_iter().map(|e| {
                    let key = e.key.clone();
                    view! { <div class="doc-key">{key}</div><div class="doc-val"><DocValue entry=e level=level in_list=false/></div> }
                }).collect_view();
                view! { <div class="doc-map mono">{rows}</div> }.into_any()
            };
            view! {
                <div class="card doc-pane">
                    {header(total)}
                    {entries}
                    {(total == 0).then(|| view! { <p class="dim">"(empty)"</p> })}
                    {more}
                </div>
            }
            .into_any()
        })
    };
    view! { <div class="doc-level" class:band=band>{body}</div> }
}

const OPS_PAGE: usize = 200;
const JSON_LIMIT: usize = 2 << 20;

/// The ops whose object is `obj`, as logical rows.
#[component]
fn ObjOps(obj: automerge::ObjId, title: String) -> impl IntoView {
    let app = expect_context::<AppState>();
    let page = RwSignal::new(0usize);
    move || {
        with_model(app, |m| {
            let ops = m.object_ops(&obj);
            let total = ops.len();
            let first = (page.get() * OPS_PAGE).min(total);
            let rows = ops[first..(first + OPS_PAGE).min(total)].iter().map(|op| {
                let (_, origin) = origin_label(app, op.change);
                view! {
                    <tr title=origin>
                        <td>{op.id.clone()}</td>
                        <td>{op.key.clone()}</td>
                        <td>{if op.insert { "t" } else { "f" }}</td>
                        <td>{op.action.clone()}</td>
                        <td>{op.pred.clone()}</td>
                    </tr>
                }
            }).collect_view();
            let pages = total.div_ceil(OPS_PAGE);
            view! {
                <div class="card doc-pane">
                    <div class="card-title">
                        <b class="mono">{title.clone()}</b>
                        <span class="dim">{format!("{} ops", fmt::count_exact(total))}</span>
                        {(pages > 1).then(|| view! {
                            <span class="pager">
                                <button disabled=move || page.get() == 0 on:click=move |_| page.update(|p| *p -= 1)>"‹"</button>
                                <span>{move || format!("page {} / {pages}", page.get() + 1)}</span>
                                <button disabled=move || page.get() + 1 >= pages on:click=move |_| page.update(|p| *p += 1)>"›"</button>
                            </span>
                        })}
                    </div>
                    <table class="dense rows mono">
                        <thead><tr><th>"id"</th><th>"key"</th><th>"insert"</th><th>"Action"</th><th>"pred"</th></tr></thead>
                        <tbody>{rows}</tbody>
                    </table>
                </div>
            }
        })
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
    let (_, origin) = origin_label(app, entry.origin);
    let conflict = entry.conflict.then(|| view! { <span class="conflict" title="concurrent values; showing the winner">"⚡"</span> });
    let title = format!("{origin}\nop {}", entry.op_id);
    match entry.value {
        NodeValue::Scalar { text, kind } => view! {
            <span class=format!("val v-{kind}") title=title>{text}</span>{conflict}
        }
        .into_any(),
        NodeValue::Object { id, ty, len } => {
            let key = if in_list { format!("[{}]", entry.key) } else { entry.key.clone() };
            let crumb = Crumb { key, id, ty, len };
            let sel = {
                let crumb = crumb.clone();
                move || app.doc_path.with(|p| p.get(level) == Some(&crumb))
            };
            view! {
                <button class="obj-chip" class:sel=sel title=title
                    on:click=move |_| open_at(app, level, Some(crumb.clone()))>{obj_label(ty, len)}</button>
                {conflict}
            }
            .into_any()
        }
    }
}

fn origin_label(app: AppState, origin: Option<usize>) -> (String, String) {
    with_model_untracked(app, |m| {
        origin
            .and_then(|i| m.changes.get(i).map(|c| (i, c)))
            .map(|(i, c)| {
                let who = c
                    .author
                    .clone()
                    .unwrap_or_else(|| m.short_actor(&c.actor).to_string());
                (
                    format!("{who} · {}", format_time(c.time)),
                    format!(
                        "set in change #{} ({}@{}) by {who} at {}{}",
                        i + 1,
                        c.seq,
                        m.short_actor(&c.actor),
                        format_time(c.time),
                        c.message.as_ref().map(|m| format!("\n“{m}”")).unwrap_or_default()
                    ),
                )
            })
    })
    .flatten()
    .unwrap_or_default()
}

fn obj_label(ty: ObjType, len: usize) -> String {
    match ty {
        ObjType::Map => format!("{{map {len}}}"),
        ObjType::Table => format!("{{table {len}}}"),
        ObjType::List => format!("[list {len}]"),
        ObjType::Text => format!("text {len} chars"),
    }
}
