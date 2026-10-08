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
                <Timeline/>
                <div class="doc-layout">
                    <section class="card tree-card">
                        <div class="card-title">
                            <b>"Document"</b>
                            <span class="dim">{move || match app.heads.get() {
                                None => "latest state".to_string(),
                                Some(h) => format!("as of {} head{}", h.len(), if h.len() == 1 { "" } else { "s" }),
                            }}</span>
                        </div>
                        <div class="tree">
                            <ObjectChildren obj=automerge::ROOT ty=ObjType::Map depth=0/>
                        </div>
                    </section>
                    <aside class="doc-side">
                        <StepDetail/>
                        <Authors/>
                    </aside>
                </div>
            }
            .into_any(),
        })
    }
}

const BUCKETS: usize = 120;

#[component]
fn Timeline() -> impl IntoView {
    let app = expect_context::<AppState>();
    let n = with_model_untracked(app, |m| m.changes.len()).unwrap_or(0);
    let buckets: Vec<u64> = with_model_untracked(app, |m| {
        let mut b = vec![0u64; BUCKETS.min(n.max(1))];
        let len = b.len();
        for (i, c) in m.changes.iter().enumerate() {
            b[i * len / n.max(1)] += c.num_ops();
        }
        b
    })
    .unwrap_or_default();
    let max = buckets.iter().copied().max().unwrap_or(1).max(1);
    let nb = buckets.len().max(1);
    let label = move || {
        let step = app.step.get();
        with_model(app, |m| {
            if step == 0 {
                return "empty document — before the first change".to_string();
            }
            let c = &m.changes[step - 1];
            format!(
                "{} · {} · {}",
                format_time(c.time),
                c.author.clone().unwrap_or_else(|| m.short_actor(&c.actor).to_string()),
                c.message.clone().unwrap_or_default()
            )
        })
        .unwrap_or_default()
    };
    let go = move |d: i64| app.step.update(|s| *s = (*s as i64 + d).clamp(0, n as i64) as usize);
    view! {
        <div class="card timeline">
            <div class="card-title">
                <b>"History"</b>
                <span class="mono">{move || format!("step {} / {}", fmt::count(app.step.get()), fmt::count(n))}</span>
                <span class="dim step-label">{label}</span>
                <span class="spacer"></span>
                <div class="stepper">
                    <button on:click=move |_| app.step.set(0) title="before first change">"«"</button>
                    <button on:click=move |_| go(-1) title="previous change">"‹"</button>
                    <button on:click=move |_| go(1) title="next change">"›"</button>
                    <button on:click=move |_| app.step.set(n) title="latest">"»"</button>
                </div>
            </div>
            <svg class="spark" viewBox=format!("0 0 {nb} 20") preserveAspectRatio="none">
                {buckets.iter().enumerate().map(|(i, v)| {
                    let h = (*v as f64 / max as f64 * 20.0).max(if *v > 0 { 0.6 } else { 0.0 });
                    view! { <rect x=i.to_string() y=(20.0 - h).to_string() width="0.85" height=h.to_string()/> }
                }).collect_view()}
                <rect class="cursor" y="0" height="20" width="0.4"
                    x=move || (app.step.get() as f64 / n.max(1) as f64 * nb as f64 - 0.2).to_string()/>
            </svg>
            <input type="range" class="slider" min="0" max=n.to_string()
                prop:value=move || app.step.get().to_string()
                on:input=move |ev| {
                    if let Ok(v) = event_target_value(&ev).parse::<usize>() { app.step.set(v) }
                }/>
        </div>
    }
}

#[component]
fn StepDetail() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        let step = app.step.get();
        if step == 0 {
            return None;
        }
        with_model(app, |m| {
            let c = m.changes[step - 1].clone();
            let deps: Vec<String> = c.deps.iter().map(|d| format!("#{}", d + 1)).collect();
            view! {
                <div class="card">
                    <div class="card-title"><b>{format!("Change #{step}")}</b><span class="dim">"the last change applied"</span></div>
                    <dl class="kv">
                        <dt>"hash"</dt><dd class="mono" title=c.hash.to_string()>{fmt::short(&c.hash.to_string(), 16)}</dd>
                        <dt>"author"</dt><dd>{c.author.clone().unwrap_or_else(|| "—".into())}</dd>
                        <dt>"actor"</dt><dd class="mono" title=c.actor.clone()>{fmt::short(&c.actor, 16)}</dd>
                        <dt>"seq"</dt><dd>{c.seq}</dd>
                        <dt>"ops"</dt><dd>{format!("{} ({}..={})", c.num_ops(), c.start_op, c.max_op)}</dd>
                        <dt>"time"</dt><dd>{format_time(c.time)}</dd>
                        <dt>"message"</dt><dd>{c.message.clone().unwrap_or_default()}</dd>
                        <dt>"deps"</dt><dd class="mono">{if deps.is_empty() { "none".to_string() } else { deps.join(" ") }}</dd>
                    </dl>
                </div>
            }
        })
    }
}

#[component]
fn Authors() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        with_model(app, |m| {
            let mut rows: Vec<(usize, i64, i64)> = vec![(0, i64::MAX, i64::MIN); m.actors.len()];
            for c in &m.changes {
                let r = &mut rows[c.actor_idx];
                r.0 += 1;
                r.1 = r.1.min(c.time);
                r.2 = r.2.max(c.time);
            }
            let mut order: Vec<usize> = (0..m.actors.len()).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(m.actors[i].ops));
            let total = m.actors.len();
            view! {
                <div class="card">
                    <div class="card-title"><b>"Actors"</b><span class="dim">{format!("{total} actors")}</span></div>
                    <div class="scroll-y">
                        <table class="dense">
                            <thead><tr><th>"author"</th><th>"actor"</th><th class="num">"changes"</th><th class="num">"ops"</th><th>"active"</th></tr></thead>
                            <tbody>
                                {order.into_iter().take(200).map(|i| {
                                    let a = &m.actors[i];
                                    let (_, first, last) = rows[i];
                                    view! {
                                        <tr>
                                            <td>{a.author.clone().unwrap_or_default()}</td>
                                            <td class="mono" title=a.actor.clone()>{m.short_actor(&a.actor).to_string()}</td>
                                            <td class="num">{fmt::count(a.changes)}</td>
                                            <td class="num">{fmt::count(a.ops as usize)}</td>
                                            <td class="dim small">{format!("{} → {}", format_time(first), format_time(last))}</td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                </div>
            }
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
                        "set in change #{} ({}@{}) by {who} at {}{}\nclick to jump there",
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
    let origin = entry.origin;
    let jump = move |_| {
        if let Some(i) = origin {
            app.step.set(i + 1);
        }
    };
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
                <span class="origin" title=format!("{origin_long}\nop {op}") on:click=jump>{origin_short}</span>
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
                <span class="origin" title=format!("{origin_long}\nobject {op}") on:click=move |ev| { ev.stop_propagation(); jump(ev) }>{origin_short}</span>
            </div>
            {move || open.get().then(|| view! { <ObjectChildren obj=id.clone() ty=ty depth=depth + 1/> })}
        }
        .into_any(),
    }
}
