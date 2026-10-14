use am_inspect::doc::{DocModel, Entry, NodeValue};
use am_inspect::format_time;
use automerge::{ObjId, ObjType};
use leptos::prelude::*;

use crate::fmt;
use crate::state::AppState;

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
            Some(Ok(_)) => view! {
                <section class="card tree-card">
                    <div class="tree">
                        <ObjectChildren obj=automerge::ROOT ty=ObjType::Map depth=0/>
                    </div>
                </section>
            }
            .into_any(),
        })
    }
}

const TREE_PAGE: usize = 100;

#[component]
fn ObjectChildren(obj: ObjId, ty: ObjType, depth: usize) -> impl IntoView {
    let app = expect_context::<AppState>();
    let limit = RwSignal::new(TREE_PAGE);
    let is_text = ty == ObjType::Text;
    move || {
        let heads = app.heads.get();
        let heads = heads.as_deref();
        let limit_now = limit.get();
        with_model(app, |m| {
            if is_text {
                let text = m.text(&obj, heads);
                let chars = text.chars().count();
                let marks = m.marks(&obj, heads);
                let shown: String = text.chars().take(limit_now * 40).collect();
                return view! {
                    <div class="text-obj" style:margin-left=format!("{}em", depth as f64 * 1.1 + 1.2)>
                        <pre class="text">{shown}{(chars > limit_now * 40).then(|| "…")}</pre>
                        {(chars > limit_now * 40).then(|| view! {
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
            let page = m.page(&obj, heads, 0, limit_now);
            let total = page.total;
            let rows = page
                .entries
                .into_iter()
                .map(|e| view! { <TreeRow entry=e depth=depth/> })
                .collect_view();
            view! {
                {rows}
                {(total > limit_now).then(|| view! {
                    <button class="link more" style:margin-left=format!("{}em", depth as f64 * 1.1 + 1.2)
                        on:click=move |_| limit.update(|l| *l += TREE_PAGE)>
                        {format!("show more ({} of {})", limit_now, total)}
                    </button>
                })}
                {(total == 0 && depth == 0).then(|| view! { <p class="dim">"(empty)"</p> })}
            }
            .into_any()
        })
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
        ObjType::Map => format!("{{map · {len}}}"),
        ObjType::Table => format!("{{table · {len}}}"),
        ObjType::List => format!("[list · {len}]"),
        ObjType::Text => format!("text · {len} chars"),
    }
}

#[component]
fn TreeRow(entry: Entry, depth: usize) -> impl IntoView {
    let app = expect_context::<AppState>();
    let small = matches!(entry.value, NodeValue::Object { len, ty, .. } if ty == ObjType::Text || len <= 12);
    let open = RwSignal::new(depth == 0 && small);
    let (origin_short, origin_long) = origin_label(app, entry.origin);
    let indent = format!("{}em", depth as f64 * 1.1);
    let conflict = entry.conflict.then(|| view! { <span class="conflict" title="concurrent values; showing the winner">"⚡"</span> });
    let key = entry.key.clone();
    let op = entry.op_id.clone();
    match entry.value {
        NodeValue::Scalar { text, kind } => view! {
            <div class="row" style:padding-left=indent>
                <span class="twisty"></span>
                <span class="key">{key}</span>
                <span class="sep">":"</span>
                <span class=format!("val v-{kind}")>{text}</span>
                <span class="vtype">{kind}</span>
                {conflict}
                <span class="origin" title=format!("{origin_long}\nop {op}")>{origin_short}</span>
            </div>
        }
        .into_any(),
        NodeValue::Object { id, ty, len } => view! {
            <div class="row obj" style:padding-left=indent on:click=move |_| open.update(|o| *o = !*o)>
                <span class="twisty">{move || if open.get() { "▾" } else { "▸" }}</span>
                <span class="key">{key}</span>
                <span class="sep">":"</span>
                <span class="objtype">{obj_label(ty, len)}</span>
                {conflict}
                <span class="origin" title=format!("{origin_long}\nobject {op}")>{origin_short}</span>
            </div>
            {move || open.get().then(|| view! { <ObjectChildren obj=id.clone() ty=ty depth=depth + 1/> })}
        }
        .into_any(),
    }
}
