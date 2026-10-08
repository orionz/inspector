use std::rc::Rc;

use am_inspect::chunk::{Chunk, ChunkKind};
use am_inspect::columns::{
    build_rows, decode_runs, value_breakdown, value_type_name, Cell, ColType, ColumnInfo, DecodedRuns, Run,
    RunKind, RowTable,
};
use am_inspect::doc::{decode_change, short_hex};
use am_inspect::{chunk_changes, format_time};
use leptos::prelude::*;

use crate::fmt;
use crate::state::{AppState, Cache, ColumnRef, GroupView, Loaded};

fn with_chunk<R>(app: AppState, f: impl FnOnce(&Rc<Loaded>, &Chunk) -> R) -> Option<R> {
    let idx = app.chunk.get();
    app.file
        .with(|file| file.as_ref().and_then(|l| l.report.chunks.get(idx).map(|c| f(l, c))))
}

fn file_key(l: &Rc<Loaded>) -> usize {
    Rc::as_ptr(l) as usize
}

#[component]
pub fn StorageTab() -> impl IntoView {
    view! {
        <FileMap/>
        <div class="storage">
            <aside class="sidebar">
                <TypeSummary/>
                <ChunkList/>
            </aside>
            <section class="detail">
                <ChunkDetail/>
            </section>
        </div>
    }
}

/// One horizontal bar for the whole file; tiny neighbouring chunks of the
/// same kind are merged so huge files stay readable.
#[component]
fn FileMap() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        app.file.with(|f| {
            let f = f.as_ref()?;
            let total = f.report.len.max(1);
            let mut segs: Vec<(ChunkKind, usize, usize, usize)> = vec![];
            for c in &f.report.chunks {
                let small = c.len() * 400 < total;
                match segs.last_mut() {
                    Some(last) if small && last.0 == c.kind && last.3 * 400 < total => {
                        last.2 = c.index;
                        last.3 += c.len();
                    }
                    _ => segs.push((c.kind, c.index, c.index, c.len())),
                }
            }
            let trailing = f.report.trailing.clone();
            Some(view! {
                <div class="filemap">
                    {segs
                        .into_iter()
                        .map(|(kind, first, last, len)| {
                            let title = if first == last {
                                format!("#{first} {} · {}", kind.label(), fmt::bytes(len))
                            } else {
                                format!("#{first}–#{last} {} × {} · {}", kind.label(), last - first + 1, fmt::bytes(len))
                            };
                            view! {
                                <div class=format!("seg k-{}", kind.key())
                                    class:sel=move || (first..=last).contains(&app.chunk.get())
                                    style:flex-grow=len.to_string()
                                    title=title
                                    on:click=move |_| { app.chunk.set(first); app.column.set(None); }></div>
                            }
                        })
                        .collect_view()}
                    {trailing.map(|(off, _)| {
                        let len = total - off;
                        view! { <div class="seg k-unknown" style:flex-grow=len.to_string() title="unparsed trailing bytes"></div> }
                    })}
                </div>
                {f.report.trailing.as_ref().map(|(off, e)| view! {
                    <div class="error">{format!("Stopped reading at byte {off}: {e}")}</div>
                })}
            })
        })
    }
}

#[component]
fn TypeSummary() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        app.file.with(|f| {
            let f = f.as_ref()?;
            let mut rows: Vec<(ChunkKind, usize, usize, usize, usize)> = vec![];
            for c in &f.report.chunks {
                match rows.iter_mut().find(|r| r.0 == c.kind) {
                    Some(r) => {
                        r.1 += 1;
                        r.2 += c.len();
                        r.3 += c.num_changes();
                        r.4 += c.num_ops();
                    }
                    None => rows.push((c.kind, 1, c.len(), c.num_changes(), c.num_ops())),
                }
            }
            let total = f.report.len;
            Some(view! {
                <table class="dense summary">
                    <thead><tr><th>"type"</th><th>"#"</th><th>"bytes"</th><th>"changes"</th><th>"ops"</th></tr></thead>
                    <tbody>
                        {rows.into_iter().map(|(k, n, b, ch, ops)| view! {
                            <tr>
                                <td><span class=format!("chip k-{}", k.key())>{k.label()}</span></td>
                                <td class="num">{n}</td>
                                <td class="num" title=fmt::pct(b, total)>{fmt::bytes(b)}</td>
                                <td class="num">{fmt::count(ch)}</td>
                                <td class="num">{fmt::count(ops)}</td>
                            </tr>
                        }).collect_view()}
                    </tbody>
                </table>
            })
        })
    }
}

const LIST_PAGE: usize = 200;

#[component]
fn ChunkList() -> impl IntoView {
    let app = expect_context::<AppState>();
    let shown = RwSignal::new(LIST_PAGE);
    move || {
        app.file.with(|f| {
            let f = f.as_ref()?;
            let total = f.report.len.max(1);
            let max = f.report.chunks.iter().map(|c| c.len()).max().unwrap_or(1);
            let n = f.report.chunks.len();
            let limit = shown.get();
            Some(view! {
                <div class="chunk-list">
                    {f.report.chunks.iter().take(limit).map(|c| {
                        let i = c.index;
                        let w = fmt::ratio(c.len(), max);
                        let detail = match c.kind {
                            ChunkKind::Change | ChunkKind::Compressed => c.facts.change.as_ref()
                                .map(|h| format!("seq {} · {} ops", h.seq, c.num_ops()))
                                .unwrap_or_default(),
                            _ => format!("{} ch · {} ops", fmt::count(c.num_changes()), fmt::count(c.num_ops())),
                        };
                        view! {
                            <button class="chunk-row" class:sel=move || app.chunk.get() == i
                                title=format!("{} of file", fmt::pct(c.len(), total))
                                on:click=move |_| { app.chunk.set(i); app.column.set(None); }>
                                <span class="idx">{format!("#{i}")}</span>
                                <span class=format!("chip k-{}", c.kind.key())>{c.kind.label()}</span>
                                <span class="dim detail">{detail}</span>
                                <span class="num">{fmt::bytes(c.len())}</span>
                                {(!c.checksum_ok || c.error.is_some()).then(|| view! { <span class="bad">"!"</span> })}
                                <span class=format!("minibar k-{}", c.kind.key()) style:width=format!("{w}%")></span>
                            </button>
                        }
                    }).collect_view()}
                    {(n > limit).then(|| view! {
                        <button class="link more" on:click=move |_| shown.update(|s| *s += LIST_PAGE)>
                            {format!("show more ({} remaining)", n - limit)}
                        </button>
                    })}
                </div>
            })
        })
    }
}

#[component]
fn ChunkDetail() -> impl IntoView {
    let app = expect_context::<AppState>();
    view! {
        {move || with_chunk(app, |f, c| view! {
            <ChunkFacts chunk=c.clone() file_len=f.report.len/>
        })}
        <div class="groups">
            <GroupPanel ops=false/>
            <GroupPanel ops=true/>
        </div>
        <RunPanel/>
    }
}

fn fact(label: &'static str, value: impl IntoView) -> impl IntoView {
    view! { <div class="fact"><span class="k">{label}</span><span class="v">{value}</span></div> }
}

fn hash_list(hashes: &[String]) -> impl IntoView {
    let title = hashes.join("\n");
    let shown: Vec<String> = hashes.iter().take(3).map(|h| fmt::short(h, 8)).collect();
    let more = hashes.len().saturating_sub(3);
    view! {
        <span class="mono" title=title>
            {if hashes.is_empty() { "none".to_string() } else { shown.join(" ") }}
            {(more > 0).then(|| format!(" +{more}"))}
        </span>
    }
}

#[component]
fn ChunkFacts(chunk: Chunk, file_len: usize) -> impl IntoView {
    let c = chunk;
    let total = c.len().max(1);
    let facts = &c.facts;
    let change = facts.change.clone();
    view! {
        <div class="card">
            <div class="card-title">
                <span class=format!("chip k-{}", c.kind.key())>{c.kind.label()}</span>
                <b>{format!("chunk #{}", c.index)}</b>
                <span>{fmt::bytes(c.len())}</span>
                <span class="dim">{format!("{} of file · bytes {}..{}", fmt::pct(c.len(), file_len), c.range.start, c.range.end)}</span>
                <span class=if c.checksum_ok { "ok" } else { "bad" } title=format!("hash {}", c.hash_hex())>
                    {if c.checksum_ok { "checksum ✓" } else { "checksum ✗" }}
                    " " <span class="mono">{hex_bytes(&c.checksum)}</span>
                </span>
            </div>
            {c.error.clone().map(|e| view! { <div class="error">{e}</div> })}
            <div class="facts">
                {fact("changes", fmt::count(c.num_changes()))}
                {fact("ops", fmt::count(c.num_ops()))}
                {fact("actors", view! { <span title=facts.actors.join("\n")>{facts.actors.len()}</span> })}
                {(!facts.heads.is_empty()).then(|| fact("heads", hash_list(&facts.heads)))}
                {(!facts.deps.is_empty() || change.is_some()).then(|| fact("deps", hash_list(&facts.deps)))}
                {c.inflated_len.map(|n| fact("inflated", format!("{} ({:.1}×)", fmt::bytes(n), n as f64 / total as f64)))}
                {change.map(|h| view! {
                    {fact("actor", view! { <span class="mono" title=h.actor.clone()>{fmt::short(&h.actor, 12)}</span> })}
                    {fact("seq", h.seq)}
                    {fact("start op", h.start_op)}
                    {fact("time", format_time(h.time))}
                    {fact("message", h.message.unwrap_or_default())}
                })}
                {(facts.extra_bytes > 0).then(|| fact("extra", fmt::bytes(facts.extra_bytes)))}
                {(!facts.head_indexes.is_empty()).then(|| fact("head idx", format!("{:?}", facts.head_indexes)))}
                {facts.change_set_prefix.iter().map(|(k, n)| fact(k, *n)).collect_view()}
            </div>
            <div class="layout-bar">
                {c.sections.iter().map(|s| view! {
                    <div class=format!("seg s-{}", s.kind.key())
                        style:flex-grow=s.len().to_string()
                        title=format!("{} · {} · {}", s.label, fmt::bytes(s.len()), fmt::pct(s.len(), total))></div>
                }).collect_view()}
            </div>
            <div class="layout-legend">
                {c.sections.iter().map(|s| view! {
                    <span class="legend-item">
                        <i class=format!("swatch s-{}", s.kind.key())></i>
                        {s.label.clone()} " "
                        <b>{fmt::bytes(s.len())}</b>
                        <span class="dim">" " {fmt::pct(s.len(), total)}</span>
                    </span>
                }).collect_view()}
            </div>
        </div>
    }
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[component]
fn GroupPanel(ops: bool) -> impl IntoView {
    let app = expect_context::<AppState>();
    let view_of = move || {
        let (a, b) = app.group_view.get();
        if ops {
            b
        } else {
            a
        }
    };
    let set_view = move |v: GroupView| {
        app.group_view.update(|g| if ops { g.1 = v } else { g.0 = v });
    };
    let has_group = move || with_chunk(app, |_, c| c.group(ops).is_some()).unwrap_or(false);
    let title = if ops { "Op set" } else { "Change metadata" };
    let tab = move |v: GroupView, label: &'static str| {
        view! { <button class:active=move || view_of() == v on:click=move |_| set_view(v)>{label}</button> }
    };
    view! {
        <Show when=has_group fallback=move || (!ops).then(|| view! {
            <div class="card group empty">
                <div class="card-title"><b>{title}</b></div>
                <p class="dim">"Stored in the chunk header (see above)."</p>
            </div>
        })>
            <div class="card group">
                <div class="card-title">
                    <b>{title}</b>
                    {move || with_chunk(app, |_, c| c.group(ops).map(|g| view! {
                        <span>{format!("{} rows", fmt::count(g.rows))}</span>
                        <span>{fmt::bytes(g.stored_len())}</span>
                        {(g.inflated_len() != g.stored_len()).then(|| view! {
                            <span class="dim">{format!("({} inflated)", fmt::bytes(g.inflated_len()))}</span>
                        })}
                        <span class="dim">{format!("+{} B col meta", g.meta_len)}</span>
                    }))}
                    <span class="spacer"></span>
                    <nav class="seg-tabs">
                        {tab(GroupView::Columns, "columns")}
                        {tab(GroupView::Rows, "rows")}
                        {ops.then(|| tab(GroupView::Changes, "changes"))}
                    </nav>
                </div>
                {move || match view_of() {
                    GroupView::Columns => view! { <ColumnTable ops=ops/> }.into_any(),
                    GroupView::Rows => view! { <RowsView ops=ops/> }.into_any(),
                    GroupView::Changes => view! { <ChangesView/> }.into_any(),
                }}
            </div>
        </Show>
    }
}

#[component]
fn ColumnTable(ops: bool) -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        with_chunk(app, |f, c| {
            let g = c.group(ops)?;
            let file_len = f.report.len;
            let total = g.stored_len().max(1);
            let max = g.columns.iter().map(|c| c.stored_len.max(c.data.len())).max().unwrap_or(1);
            let rows = g.rows;
            Some(view! {
                <table class="dense cols">
                    <thead><tr>
                        <th>"column"</th><th>"type"</th><th class="num">"bytes"</th><th class="bar-col">"size"</th>
                        <th class="num">"%grp"</th><th class="num">"%file"</th><th class="num">"B/row"</th>
                    </tr></thead>
                    <tbody>
                        {g.columns.iter().enumerate().map(|(index, col)| {
                            let me = ColumnRef { ops, index };
                            let stored = fmt::ratio(col.stored_len, max);
                            let inflated = fmt::ratio(col.data.len(), max);
                            let title = format!(
                                "spec {:#x} · id {} · {}{}",
                                col.spec.0, col.spec.id(), col.col_type(),
                                if col.deflated() { format!(" · deflated from {}", fmt::bytes(col.data.len())) } else { String::new() }
                            );
                            view! {
                                <tr class="click" class:sel=move || app.column.get() == Some(me)
                                    on:click=move |_| app.column.set(Some(me)) title=title>
                                    <td class="mono">{col.name.clone()}{col.deflated().then(|| view! { <span class="z" title="deflated">"z"</span> })}</td>
                                    <td><span class=format!("tchip t-{}", col.col_type().short())>{col.col_type().short()}</span></td>
                                    <td class="num">{fmt::bytes(col.stored_len)}</td>
                                    <td class="bar-col">
                                        <div class="bar">
                                            {col.deflated().then(|| view! { <div class="ghost" style:width=format!("{inflated}%")></div> })}
                                            <div class=format!("fill t-{}", col.col_type().short()) style:width=format!("{stored}%")></div>
                                        </div>
                                    </td>
                                    <td class="num">{fmt::pct(col.stored_len, total)}</td>
                                    <td class="num dim">{fmt::pct(col.stored_len, file_len)}</td>
                                    <td class="num dim">{fmt::per_row(col.stored_len, rows)}</td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            })
        })
        .flatten()
    }
}

fn pager(page: RwSignal<usize>, pages: usize, total: usize, unit: &'static str) -> impl IntoView {
    view! {
        <div class="pager">
            <button disabled=move || page.get() == 0 on:click=move |_| page.set(0)>"«"</button>
            <button disabled=move || page.get() == 0 on:click=move |_| page.update(|p| *p = p.saturating_sub(1))>"‹"</button>
            <span>{move || format!("page {} / {} · {} {unit}", page.get() + 1, pages.max(1), fmt::count(total))}</span>
            <button disabled=move || { page.get() + 1 >= pages } on:click=move |_| page.update(|p| *p += 1)>"›"</button>
            <button disabled=move || { page.get() + 1 >= pages } on:click=move |_| page.set(pages.saturating_sub(1))>"»"</button>
        </div>
    }
}

const ROWS_PAGE: usize = 50;

#[component]
fn RowsView(ops: bool) -> impl IntoView {
    let app = expect_context::<AppState>();
    let cache: Cache<(usize, usize, bool), RowTable> = Cache::new();
    move || {
        let table = with_chunk(app, |f, c| {
            c.group(ops)
                .map(|g| cache.get((file_key(f), c.index, ops), || build_rows(g)))
        })
        .flatten()?;
        let page = RwSignal::new(0usize);
        let pages = table.rows.div_ceil(ROWS_PAGE);
        let t = StoredValue::new_local(table.clone());
        Some(view! {
            {pager(page, pages, table.rows, "rows")}
            <div class="scroll-x">
                <table class="dense rows mono">
                    <thead><tr>
                        <th class="num">"#"</th>
                        {table.fields.iter().map(|f| view! { <th>{f.name.clone()}</th> }).collect_view()}
                    </tr></thead>
                    <tbody>
                        {move || t.with_value(|t| {
                            let start = page.get() * ROWS_PAGE;
                            (start..(start + ROWS_PAGE).min(t.rows)).map(|r| view! {
                                <tr>
                                    <td class="num dim">{r}</td>
                                    {t.fields.iter().map(|f| {
                                        let v = f.render(r);
                                        let title = v.clone();
                                        view! { <td title=title>{v}</td> }
                                    }).collect_view()}
                                </tr>
                            }).collect_view()
                        })}
                    </tbody>
                </table>
            </div>
        })
    }
}

const CHANGES_PAGE: usize = 25;

#[component]
fn ChangesView() -> impl IntoView {
    let app = expect_context::<AppState>();
    let cache: Cache<(usize, usize), Result<Vec<automerge::Change>, String>> = Cache::new();
    move || {
        let changes = with_chunk(app, |f, c| {
            cache.get((file_key(f), c.index), || chunk_changes(&f.bytes, c))
        })?;
        let n = match &*changes {
            Ok(list) => list.len(),
            Err(e) => return Some(view! { <div class="error">{e.clone()}</div> }.into_any()),
        };
        let changes = StoredValue::new_local(changes);
        let list = move |i: usize| {
            changes.with_value(|c| match &**c {
                Ok(list) => list[i].clone(),
                Err(_) => unreachable!("checked above"),
            })
        };
        let page = RwSignal::new(0usize);
        let open = RwSignal::new(None::<usize>);
        Some(view! {
            {pager(page, n.div_ceil(CHANGES_PAGE), n, "changes")}
            <div class="changes">
                {move || {
                    let start = page.get() * CHANGES_PAGE;
                    (start..(start + CHANGES_PAGE).min(n)).map(move |i| {
                        let ch = list(i);
                        let author = ch.author().map(|a| am_inspect::doc::author_label(a.as_bytes()));
                        let actor = ch.actor_id().to_hex_string();
                        view! {
                            <div class="change">
                                <button class="change-row" class:sel=move || open.get() == Some(i)
                                    on:click=move |_| open.update(|o| *o = if *o == Some(i) { None } else { Some(i) })>
                                    <span class="mono">{format!("{}@{}", ch.seq(), short_hex(&actor))}</span>
                                    <span class="author">{author.unwrap_or_default()}</span>
                                    <span class="dim">{format_time(ch.timestamp())}</span>
                                    <span class="num">{format!("{} ops", ch.len())}</span>
                                    <span class="msg">{ch.message().map(|m| m.to_string()).unwrap_or_default()}</span>
                                    <span class="mono dim">{fmt::short(&ch.hash().to_string(), 8)}</span>
                                </button>
                                {move || (open.get() == Some(i)).then(|| {
                                    let d = decode_change(&list(i));
                                    view! {
                                        <div class="change-body">
                                            <div class="facts">
                                                {fact("hash", view! { <span class="mono">{d.hash.clone()}</span> })}
                                                {fact("actor", view! { <span class="mono">{d.actor.clone()}</span> })}
                                                {fact("start op", d.start_op)}
                                                {fact("deps", hash_list(&d.deps))}
                                                {fact("extra", fmt::bytes(d.extra_bytes))}
                                            </div>
                                            <table class="dense mono ops">
                                                <thead><tr><th>"id"</th><th>"action"</th><th>"obj"</th><th>"key"</th><th>"ins"</th><th>"pred"</th></tr></thead>
                                                <tbody>
                                                    {d.ops.iter().take(500).map(|o| view! {
                                                        <tr>
                                                            <td>{o.id.clone()}</td><td>{o.action.clone()}</td><td>{o.obj.clone()}</td>
                                                            <td>{o.key.clone()}</td><td>{if o.insert { "✓" } else { "" }}</td><td>{o.pred.clone()}</td>
                                                        </tr>
                                                    }).collect_view()}
                                                </tbody>
                                            </table>
                                            {(d.ops.len() > 500).then(|| view! { <p class="dim">{format!("… {} more ops", d.ops.len() - 500)}</p> })}
                                        </div>
                                    }
                                })}
                            </div>
                        }
                    }).collect_view()
                }}
            </div>
        }.into_any())
    }
}

const RUNS_PAGE: usize = 50;

fn run_value(col: &ColumnInfo, run: &Run, actors: &[String]) -> String {
    let fmt_cell = |c: &Cell| -> String {
        match (col.col_type(), c) {
            (ColType::ValueMetadata, Cell::U(v)) => format!("{}·{}", value_type_name(v & 0xf), v >> 4),
            (ColType::Actor, Cell::U(v)) => {
                let a = actors.get(*v as usize).map(|s| short_hex(s)).unwrap_or("?");
                format!("{v}({a})")
            }
            (ColType::DeltaInteger, Cell::I(v)) => format!("{v:+}"),
            (ColType::String, Cell::S(s)) => format!("{:?}", fmt::short(s, 40)),
            (_, c) => c.to_string(),
        }
    };
    let mut s = match run.kind {
        RunKind::Null => "null".to_string(),
        RunKind::Repeat | RunKind::Bool => fmt_cell(&run.values[0]),
        RunKind::Literal => {
            let shown: Vec<String> = run.values.iter().take(12).map(fmt_cell).collect();
            let more = run.values.len().saturating_sub(12);
            format!("{}{}", shown.join(" "), if more > 0 { format!(" …+{more}") } else { String::new() })
        }
        RunKind::Raw => String::new(),
    };
    if let Some((a, b)) = run.delta_span {
        s.push_str(&format!("  → {a}..{b}"));
    }
    s
}

#[component]
fn RunPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let cache: Cache<(usize, usize, ColumnRef), DecodedRuns> = Cache::new();
    move || {
        let col_ref = app.column.get()?;
        let (col, runs, actors, by_type) = with_chunk(app, |f, c| {
            let group = c.group(col_ref.ops)?;
            let col = group.columns.get(col_ref.index)?.clone();
            let runs = cache.get((file_key(f), c.index, col_ref), || decode_runs(&col));
            let by_type = (col.col_type() == ColType::Value)
                .then(|| {
                    group.columns.iter().find(|m| {
                        m.spec.id() == col.spec.id() && m.col_type() == ColType::ValueMetadata
                    })
                })
                .flatten()
                .map(value_breakdown)
                .unwrap_or_default();
            Some((col, runs, c.facts.actors.clone(), by_type))
        })
        .flatten()?;
        let s = runs.stats.clone();
        let len = col.data.len().max(1);
        let page = RwSignal::new(0usize);
        let n = runs.runs.len();
        let raw = col.col_type() == ColType::Value;
        let data = col.data.clone();
        let split = [
            ("repeat", s.repeat_bytes, s.repeat_rows),
            ("literal", s.literal_bytes, s.literal_rows),
            ("null", s.null_bytes, s.null_rows),
        ];
        Some(view! {
            <div class="card runs">
                <div class="card-title">
                    <b class="mono">{col.name.clone()}</b>
                    <span class=format!("tchip t-{}", col.col_type().short())>{col.col_type().short()}</span>
                    <span>{format!("{} stored", fmt::bytes(col.stored_len))}</span>
                    {col.deflated().then(|| view! { <span class="dim">{format!("{} inflated ({:.1}× deflate)", fmt::bytes(col.data.len()), col.data.len() as f64 / col.stored_len.max(1) as f64)}</span> })}
                    {(!raw).then(|| view! {
                        <span>{format!("{} rows", fmt::count(s.rows))}</span>
                        <span>{format!("{} runs", fmt::count(s.runs))}</span>
                        <span class="dim">{format!("{} rows/run", fmt::per_row(s.rows, s.runs))}</span>
                    })}
                    <span class="spacer"></span>
                    <button class="link" on:click=move |_| app.column.set(None)>"close ✕"</button>
                </div>
                {runs.error.clone().map(|e| view! { <div class="error">{e}</div> })}
                {(!raw).then(|| view! {
                    <div class="split">
                        {split.iter().filter(|(_, b, _)| *b > 0).map(|(k, b, r)| view! {
                            <div class=format!("seg r-{k}") style:flex-grow=b.to_string()
                                title=format!("{k}: {} bytes for {} rows", b, r)>
                                {format!("{k} {} · {} rows", fmt::pct(*b, len), fmt::pct(*r, s.rows))}
                            </div>
                        }).collect_view()}
                    </div>
                })}
                {if raw {
                    view! {
                        <p class="dim">"Raw value bytes, concatenated; row boundaries and types come from the matching value.meta column."</p>
                        <table class="dense breakdown">
                            <thead><tr><th>"value type"</th><th class="num">"rows"</th><th class="num">"raw bytes"</th><th class="bar-col">"share"</th><th class="num">"B/row"</th></tr></thead>
                            <tbody>
                                {by_type.iter().map(|(t, rows, b)| view! {
                                    <tr>
                                        <td class="mono">{*t}</td>
                                        <td class="num">{fmt::count(*rows)}</td>
                                        <td class="num">{fmt::bytes(*b)}</td>
                                        <td class="bar-col"><div class="bar"><div class="fill t-raw" style:width=format!("{}%", fmt::ratio(*b, len))></div></div></td>
                                        <td class="num dim">{fmt::per_row(*b, *rows)}</td>
                                    </tr>
                                }).collect_view()}
                            </tbody>
                        </table>
                        <pre class="hex">{fmt::hex_preview(&data, 384)}</pre>
                    }.into_any()
                } else {
                    let runs = StoredValue::new_local(runs.clone());
                    view! {
                        {pager(page, n.div_ceil(RUNS_PAGE), n, "runs")}
                        <div class="table-wrap"><table class="dense mono runs-table">
                            <thead><tr><th class="num">"#"</th><th>"kind"</th><th class="num">"rows"</th><th class="num">"×"</th><th class="num">"bytes"</th><th>"value"</th><th>"encoded"</th></tr></thead>
                            <tbody>
                                {move || runs.with_value(|runs| {
                                    let start = page.get() * RUNS_PAGE;
                                    runs.runs[start..(start + RUNS_PAGE).min(n)].iter().enumerate().map(|(k, r)| view! {
                                        <tr>
                                            <td class="num dim">{start + k}</td>
                                            <td><span class=format!("rchip r-{}", r.kind.label())>{r.kind.label()}</span></td>
                                            <td class="num dim">{format!("{}..{}", r.first_row, r.first_row + r.count)}</td>
                                            <td class="num">{r.count}</td>
                                            <td class="num">{r.bytes.len()}</td>
                                            <td class="val">{run_value(&col, r, &actors)}</td>
                                            <td class="dim hexcell">{fmt::hex_preview(&data[r.bytes.clone()], 12)}</td>
                                        </tr>
                                    }).collect_view()
                                })}
                            </tbody>
                        </table></div>
                    }.into_any()
                }}
            </div>
        })
    }
}
