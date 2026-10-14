use std::rc::Rc;

use am_inspect::chunk::{Chunk, DepOrder, SectionKind};
use am_inspect::columns::{
    build_rows, decode_runs, decode_value, slice_hex, value_type_name, Cell, ColType, ColumnInfo, DecodedRuns, Run,
    RunKind, RowTable,
};
use am_inspect::format_time;
use leptos::context::Provider;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::fmt;
use crate::state::{AppState, Cache, ColumnRef, GroupView, Loaded, Part};

fn with_chunk<R>(app: AppState, f: impl FnOnce(&Rc<Loaded>, &Chunk) -> R) -> Option<R> {
    let idx = app.chunk.get()?;
    app.file
        .with(|file| file.as_ref().and_then(|l| l.report.chunks.get(idx).map(|c| f(l, c))))
}

fn file_key(l: &Rc<Loaded>) -> usize {
    Rc::as_ptr(l) as usize
}

#[component]
pub fn StorageTab() -> impl IntoView {
    let app = expect_context::<AppState>();
    let open = move || app.chunk.get().is_some();
    view! {
        <TrailingError/>
        <div class="storage" class:open=open>
            <ChunkStack/>
            <section class="detail">
                <ChunkDetail/>
            </section>
        </div>
    }
}

/// The storage view of what `doc.save()` writes for the loaded document,
/// with its own chunk selection.
#[component]
pub fn SaveTab() -> impl IntoView {
    let app = expect_context::<AppState>();
    let (chunk, part, column, group_view) = app.saved_sel;
    let saved = AppState { file: app.saved, chunk, part, column, group_view, ..app };
    let note = move || {
        if app.saved.with(|f| f.is_some()) {
            return None;
        }
        let failed = app.doc.with(|d| matches!(d, Some(Err(_))));
        Some(if failed { "The file didn't load as a document, so there's nothing to save." } else { "Loading the document…" })
    };
    view! {
        {move || note().map(|n| view! { <p class="dim save-note">{n}</p> })}
        <Provider value=saved>
            {move || app.saved.with(|f| f.is_some()).then(|| view! { <StorageTab/> })}
        </Provider>
    }
}

#[component]
fn TrailingError() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        app.file.with(|f| {
            let (off, e) = f.as_ref()?.report.trailing.clone()?;
            Some(view! { <div class="error">{format!("Stopped reading at byte {off}: {e}")}</div> })
        })
    }
}

/// Chunks under this share of the file are drawn as thin bands.
const TINY_SHARE: usize = 100;
/// Chunks up to this many times the cutoff join an adjacent run of bands.
const ABSORB_FACTOR: usize = 3;
const BLOCK_MIN_H: f64 = 26.0;
/// Vertical space per tiny chunk, so the cursor steps through them one by one.
const BAND_PITCH: f64 = 3.0;
/// Tightest spacing at which bands are still drawn one per chunk.
const MIN_STRIPE_PITCH: f64 = 2.0;
/// Past this height a band group squeezes its bands below one pitch.
const MAX_BANDS_H: f64 = 1500.0;
/// Size spreads above this ratio switch block heights to a log scale.
const LINEAR_SPREAD: usize = 8;

enum StackItem {
    Block { index: usize, height: f64 },
    Bands { first: usize, last: usize },
}

impl StackItem {
    fn height(&self) -> f64 {
        match self {
            StackItem::Block { height, .. } => *height,
            StackItem::Bands { first, last } => {
                let n = last - first + 1;
                band_pitch(n) * n as f64
            }
        }
    }
}

/// Which chunks are drawn as bands. Chunks a little over the cutoff are
/// pulled into neighbouring bands so a run of similar sizes isn't split
/// into alternating bands and blocks.
fn tiny_chunks(chunks: &[Chunk], file_len: usize) -> Vec<bool> {
    let mut tiny: Vec<bool> = chunks.iter().map(|c| c.len() * TINY_SHARE < file_len).collect();
    let near = |c: &Chunk| c.len() * TINY_SHARE < file_len * ABSORB_FACTOR;
    for i in (1..chunks.len()).chain((0..chunks.len().saturating_sub(1)).rev()) {
        let neighbour = (i > 0 && tiny[i - 1]) || tiny.get(i + 1).copied().unwrap_or(false);
        if !tiny[i] && neighbour && near(&chunks[i]) {
            tiny[i] = true;
        }
    }
    tiny
}

struct StackLayout {
    items: Vec<StackItem>,
}

fn stack_layout(chunks: &[Chunk], file_len: usize) -> StackLayout {
    let tiny = tiny_chunks(chunks, file_len);
    let big = || chunks.iter().zip(&tiny).filter(|(_, t)| !**t).map(|(c, _)| c.len().max(1));
    let (lo, hi) = big().fold((usize::MAX, 1), |(lo, hi), n| (lo.min(n), hi.max(n)));
    let n_big = big().count().max(1);
    let max_h = (1200.0 / n_big as f64).clamp(60.0, 260.0);
    let log = hi > lo.saturating_mul(LINEAR_SPREAD);
    let height = |len: usize| {
        let len = len.max(1) as f64;
        if log {
            let t = (len / lo as f64).ln() / (hi as f64 / lo as f64).ln();
            BLOCK_MIN_H + (max_h - BLOCK_MIN_H) * t
        } else {
            (max_h * len / hi as f64).max(BLOCK_MIN_H)
        }
    };
    let mut items: Vec<StackItem> = vec![];
    for (c, tiny) in chunks.iter().zip(tiny) {
        if !tiny {
            items.push(StackItem::Block { index: c.index, height: height(c.len()) });
            continue;
        }
        match items.last_mut() {
            Some(StackItem::Bands { last, .. }) => *last = c.index,
            _ => items.push(StackItem::Bands { first: c.index, last: c.index }),
        }
    }
    StackLayout { items }
}

/// A band's background: one stripe per chunk in its kind's colour, drawn as
/// a single gradient because thousands of 1px elements don't reliably paint.
/// Stripes get a gap between them while each is at least `MIN_STRIPE_PITCH`
/// tall at `height`.
fn band_background(chunks: &[Chunk], height: f64) -> String {
    let n = chunks.len().max(1);
    let pct = |i: usize| i as f64 * 100.0 / n as f64;
    let mut stops = vec![];
    let mut start = 0;
    for i in 1..=chunks.len() {
        if i == chunks.len() || color_key(&chunks[i]) != color_key(&chunks[start]) {
            let k = color_key(&chunks[start]);
            stops.push(format!("var(--k-{k}) {:.4}% {:.4}%", pct(start), pct(i)));
            start = i;
        }
    }
    let colors = format!("linear-gradient({})", stops.join(", "));
    if height / n as f64 >= MIN_STRIPE_PITCH {
        let p = 100.0 / n as f64;
        format!("repeating-linear-gradient(transparent 0 calc({p:.4}% - 1px), var(--panel) calc({p:.4}% - 1px) {p:.4}%), {colors}")
    } else {
        colors
    }
}

fn select_chunk(app: AppState, index: usize) {
    app.chunk.set(Some(index));
    app.part.set(None);
    app.column.set(None);
    window().scroll_to_with_x_and_y(0.0, 0.0);
}

fn close_chunk(app: AppState) {
    app.chunk.set(None);
    app.part.set(None);
    app.column.set(None);
}

fn chunk_stats(c: &Chunk, file_len: usize) -> String {
    let mut parts = vec![
        format!("{} B", fmt::count_exact(c.len())),
        fmt::pct(c.len(), file_len),
    ];
    match &c.facts.change {
        Some(h) => parts.push(format!("seq {}", h.seq)),
        None => parts.push(format!("{} changes", fmt::count(c.num_changes()))),
    }
    parts.push(format!("{} ops", fmt::count(c.num_ops())));
    if !c.facts.actors.is_empty() {
        parts.push(format!("{} actors", c.facts.actors.len()));
    }
    if let Some(n) = c.inflated_len {
        parts.push(format!("inflates to {}", fmt::bytes(n)));
    }
    parts.join(" · ")
}

/// Gap between stack items; matches `.stack { gap }`.
const STACK_GAP: f64 = 2.0;
/// Smallest zoomed panel; panels for taller blocks grow to the block's
/// height times `DECK_GROW`.
const DECK_MIN_H: f64 = 56.0;
const DECK_GROW: f64 = 1.1;
/// How many neighbouring panels are kept either side of the hovered chunk.
const DECK_REACH: usize = 2;

fn band_pitch(n: usize) -> f64 {
    BAND_PITCH.min(MAX_BANDS_H / n as f64)
}

/// Where a chunk sits in the stack, for finding the chunk under the cursor.
struct HitRow {
    top: f64,
    bottom: f64,
    band: bool,
}

fn hit_rows(layout: &StackLayout) -> Vec<HitRow> {
    let mut rows = vec![];
    let mut y = 0.0;
    for item in &layout.items {
        let h = item.height();
        match *item {
            StackItem::Block { .. } => rows.push(HitRow { top: y, bottom: y + h, band: false }),
            StackItem::Bands { first, last } => {
                let pitch = band_pitch(last - first + 1);
                for i in 0..=last - first {
                    let top = y + i as f64 * pitch;
                    rows.push(HitRow { top, bottom: top + pitch, band: true });
                }
            }
        }
        y += h + STACK_GAP;
    }
    rows
}

/// What the hover deck shows for one chunk.
#[derive(Clone)]
struct ChunkCard {
    index: usize,
    kind: &'static str,
    label: String,
    stats: String,
    context: String,
    flagged: bool,
}

fn color_key(c: &Chunk) -> &'static str {
    if c.redundant {
        return "redundant";
    }
    match c.dep_order {
        DepOrder::InOrder => c.kind.key(),
        DepOrder::Late => "late",
        DepOrder::Missing => "missing",
    }
}

fn dep_warning(c: &Chunk) -> Option<&'static str> {
    if c.redundant {
        return Some("is redundant: the server also sent a fragment or an earlier commit holding the same changes");
    }
    match c.dep_order {
        DepOrder::InOrder => None,
        DepOrder::Late => Some("depends on a change later in the file"),
        DepOrder::Missing => Some("depends on a change missing from the file; can't apply"),
    }
}

fn chunk_card(c: &Chunk, file_len: usize) -> ChunkCard {
    let context = match &c.facts.change {
        Some(h) => format!(
            "{} · actor {}{}",
            format_time(h.time),
            fmt::short(&h.actor, 12),
            h.message.as_ref().map(|m| format!(" · “{m}”")).unwrap_or_default()
        ),
        None => format!("bytes {}..{}", c.range.start, c.range.end),
    };
    let context = match dep_warning(c) {
        Some(w) => format!("{w} · {context}"),
        None => context,
    };
    ChunkCard {
        index: c.index,
        kind: color_key(c),
        label: c.kind.label(),
        stats: chunk_stats(c, file_len),
        context,
        flagged: !c.checksum_ok || c.error.is_some(),
    }
}

/// The file as a vertical stack of chunks. Hovering zooms out a panel over
/// the chunk under the cursor, and over bands its neighbours half-sized
/// behind it;
/// clicking opens that chunk. Once a chunk is open the stack shrinks into a
/// band at the left: clicking another chunk there opens it, clicking the
/// open chunk goes back one level, and clicking a band of small chunks closes it.
#[component]
fn ChunkStack() -> impl IntoView {
    let app = expect_context::<AppState>();
    let stack_el = NodeRef::<leptos::html::Div>::new();
    let rows = StoredValue::new(Vec::<HitRow>::new());
    let cards = StoredValue::new(Vec::<ChunkCard>::new());
    let hovered = RwSignal::new(None::<usize>);
    let anchor = RwSignal::new(0.0);
    let zoom_h = RwSignal::new(DECK_MIN_H);
    let on_band = RwSignal::new(false);
    let title_el = NodeRef::<leptos::html::Span>::new();
    let title_y = RwSignal::new(None::<f64>);
    Effect::new(move |_| {
        app.chunk.track();
        title_y.set(None);
        request_animation_frame(move || {
            let (Some(Some(stack)), Some(Some(title))) = (stack_el.try_get_untracked(), title_el.try_get_untracked()) else { return };
            let Ok(Some(sel)) = stack.query_selector(".sel") else { return };
            let s = stack.get_bounding_client_rect();
            let r = sel.get_bounding_client_rect();
            let half = title.get_bounding_client_rect().height() / 2.0;
            let mid = r.top() + r.height() / 2.0 - s.top();
            title_y.try_set(Some(mid.clamp(half, (s.height() - half).max(half))));
        });
    });
    let items = move || {
        app.file.with(|f| {
            let f = f.as_ref()?;
            let total = f.report.len.max(1);
            let layout = stack_layout(&f.report.chunks, total);
            rows.set_value(hit_rows(&layout));
            cards.set_value(f.report.chunks.iter().map(|c| chunk_card(c, total)).collect());
            Some(
                layout
                    .items
                    .into_iter()
                    .map(|item| {
                        let height = item.height();
                        let h = height.to_string();
                        match item {
                            StackItem::Block { index, .. } => view! {
                                <div class=format!("blk k-{}", color_key(&f.report.chunks[index]))
                                    class:sel=move || app.chunk.get() == Some(index) data-i=index
                                    style:--h=h></div>
                            }
                            .into_any(),
                            StackItem::Bands { first, last } => view! {
                                <div class="bands"
                                    class:sel=move || app.chunk.get().is_some_and(|c| (first..=last).contains(&c))
                                    style:--h=h>
                                    <div class="band-fill" style:background=band_background(&f.report.chunks[first..=last], height)></div>
                                </div>
                            }
                            .into_any(),
                        }
                    })
                    .collect_view(),
            )
        })
    };
    let track = move |ev: leptos::ev::MouseEvent| {
        if app.chunk.get_untracked().is_some() {
            return;
        }
        let Some(el) = stack_el.get_untracked() else { return };
        let y = ev.client_y() as f64 - el.get_bounding_client_rect().top();
        rows.with_value(|rows| {
            if rows.is_empty() {
                return;
            }
            let k = rows.partition_point(|r| r.top <= y).saturating_sub(1);
            let r = &rows[k];
            anchor.set((r.top + r.bottom) / 2.0);
            on_band.set(r.band);
            zoom_h.set(if r.band { DECK_MIN_H } else { (r.bottom - r.top) * DECK_GROW }.max(DECK_MIN_H));
            if hovered.get_untracked() != Some(k) {
                hovered.set(Some(k));
            }
        });
    };
    // Panels are keyed by chunk so each keeps its element, and animates, as
    // it moves between roles.
    let deck = move || match hovered.get() {
        Some(k) => (k.saturating_sub(DECK_REACH)..(k + DECK_REACH + 1).min(cards.with_value(|c| c.len()))).collect(),
        None => vec![],
    };
    let panel = move |k: usize| {
        let card = cards.with_value(|c| c[k].clone());
        let kind = card.kind;
        let block = if rows.with_value(|r| r[k].band) { "" } else { " block" };
        let class = move || {
            let role = match hovered.get().map(|h| k as isize - h as isize).unwrap_or(0) {
                ..=-2 => "before2",
                -1 => "before",
                0 => "current",
                1 => "after",
                _ => "after2",
            };
            format!("deck-panel k-{kind} {role}{block}")
        };
        view! {
            <div class=class>
                <div class="deck-line">
                    <span class="idx">{format!("#{}", card.index)}</span>
                    <span class=format!("chip k-{kind}")>{card.label}</span>
                    <span>{card.stats}</span>
                    {card.flagged.then(|| view! { <span class="bad" title="checksum mismatch or parse error">"!"</span> })}
                </div>
                <div class="deck-line dim">{card.context}</div>
            </div>
        }
    };
    view! {
        <div class="stack-wrap">
            <div class="stack" node_ref=stack_el
                on:mousemove=track
                on:mouseleave=move |_| hovered.set(None)
                on:click=move |ev| {
                    if let Some(open) = app.chunk.get_untracked() {
                        let target = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok());
                        let block = target.and_then(|t| t.closest(".blk").ok().flatten());
                        match block.and_then(|b| b.get_attribute("data-i")).and_then(|i| i.parse().ok()) {
                            Some(i) if i != open => select_chunk(app, i),
                            Some(_) if app.part.get_untracked().is_some() => {
                                app.part.set(None);
                                app.column.set(None);
                            }
                            _ => close_chunk(app),
                        }
                        return;
                    }
                    track(ev);
                    if let Some(k) = hovered.get_untracked() {
                        hovered.set(None);
                        select_chunk(app, k);
                    }
                }>
                {items}
                <div class="deck" class:solo=move || !on_band.get() style:top=move || format!("{}px", anchor.get())
                    style:--zh=move || zoom_h.get().to_string()>
                    <For each=deck key=|k| *k children=panel/>
                </div>
            </div>
            <Show when=move || app.chunk.get().is_some()>
                <div class="stack-label">
                    <span class="stack-title" node_ref=title_el class:placed=move || title_y.get().is_some()
                        style:top=move || title_y.get().map(|y| format!("{y}px"))>{move || with_chunk(app, |_, c| format!("{} #{}", c.kind.label(), c.index))}</span>
                </div>
            </Show>
        </div>
    }
}

fn parts(c: &Chunk) -> Vec<Part> {
    let mut parts = vec![];
    if c.group(false).is_some() {
        parts.push(Part::Changes);
    }
    if c.group(true).is_some() {
        parts.push(Part::Ops);
    }
    if part_bytes(c, Part::Extra) > 0 {
        parts.push(Part::Extra);
    }
    parts
}

fn extra_ranges(c: &Chunk) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    c.sections
        .iter()
        .filter(|s| s.kind == SectionKind::Suffix && s.label != "head indexes")
        .map(|s| s.range.clone())
}

fn part_key(p: Part) -> &'static str {
    match p {
        Part::Changes => "changecols",
        Part::Ops => "opcols",
        Part::Extra => "suffix",
    }
}

fn part_name(p: Part) -> &'static str {
    match p {
        Part::Changes => "Changes",
        Part::Ops => "Ops",
        Part::Extra => "Extra",
    }
}

fn part_bytes(c: &Chunk, p: Part) -> usize {
    let group = |ops: bool| c.group(ops).map_or(0, |g| g.stored_len());
    match p {
        Part::Changes => group(false),
        Part::Ops => group(true),
        Part::Extra => extra_ranges(c).map(|r| r.len()).sum(),
    }
}

fn part_label(c: &Chunk, p: Part) -> AnyView {
    let rows = |ops: bool| c.group(ops).map_or(0, |g| g.rows);
    match p {
        Part::Changes => view! { "Changes (" <b>{fmt::count_exact(rows(false))}</b> "): " {fmt::bytes(part_bytes(c, p))} }.into_any(),
        Part::Ops => view! { "Ops (" <b>{fmt::count_exact(rows(true))}</b> "): " {fmt::bytes(part_bytes(c, p))} }.into_any(),
        Part::Extra => view! { "Extra: " {fmt::bytes(part_bytes(c, p))} }.into_any(),
    }
}

const PART_MIN_H: f64 = 56.0;
const PART_SPAN_H: f64 = 420.0;

/// The open chunk's parts as blocks; opening one shrinks them into a band
/// at the left, like the chunk stack.
#[component]
fn ChunkDetail() -> impl IntoView {
    let app = expect_context::<AppState>();
    let blocks = move || {
        with_chunk(app, |_, c| {
            let total = c.len().max(1) as f64;
            parts(c)
                .into_iter()
                .map(|p| {
                    let h = (PART_SPAN_H * part_bytes(c, p) as f64 / total).max(PART_MIN_H);
                    view! {
                        <button class=format!("part s-{}", part_key(p)) class:sel=move || app.part.get() == Some(p)
                            style:--h=h.to_string()
                            title=move || if app.part.get() == Some(p) { "Back one level" } else { "" }
                            on:click=move |_| {
                                if app.part.get_untracked() == Some(p) && app.column.get_untracked().is_some() {
                                    app.column.set(None);
                                } else {
                                    app.part.update(|cur| *cur = if *cur == Some(p) { None } else { Some(p) });
                                    app.column.set(None);
                                }
                            }>
                            <span class="part-label">{part_label(c, p)}</span>
                            <span class="part-name">{part_name(p)}</span>
                        </button>
                    }
                })
                .collect_view()
        })
    };
    let detail = move || {
        let part = app.part.get()?;
        Some(match part {
            Part::Changes => view! { <GroupPanel ops=false/> }.into_any(),
            Part::Ops => view! { <GroupPanel ops=true/> }.into_any(),
            Part::Extra => with_chunk(app, |f, c| {
                let hex: String = extra_ranges(c).map(|r| hex_bytes(&f.bytes[r])).collect();
                view! {
                    <div class="card">
                        <div class="card-title"><b>"Extra bytes"</b><span class="dim">"after the columns, not read by automerge"</span></div>
                        <pre class="hex">{hex}</pre>
                    </div>
                }
                .into_any()
            })?,
        })
    };
    let ops_part = move || match app.part.get() {
        Some(Part::Changes) => Some(false),
        Some(Part::Ops) => Some(true),
        _ => None,
    };
    let side_tab = move |v: GroupView, label: &'static str| {
        let shows = move |cur: GroupView| cur == v || (v == GroupView::Physical && cur == GroupView::Raw);
        let active = move || {
            let (a, b) = app.group_view.get();
            ops_part().is_some_and(|ops| shows(if ops { b } else { a }))
        };
        view! {
            <button class:active=active on:click=move |_| {
                if active() {
                    return;
                }
                let ops = ops_part().unwrap_or(false);
                app.group_view.update(|g| if ops { g.1 = v } else { g.0 = v });
                app.column.set(None);
            }><span>{label}</span></button>
        }
    };
    view! {
        <div class="chunk-view" class:open=move || app.part.get().is_some()>
            <div class="parts-wrap">
                <div class="chunk-facts">
                    {move || with_chunk(app, |_, c| view! { <ChunkFacts chunk=c.clone()/> })}
                </div>
                <div class="parts">{blocks}</div>
            </div>
            <section class="part-detail">{detail}</section>
            <Show when=move || ops_part().is_some() && app.column.get().is_none()>
                <nav class="side-tabs">
                    {side_tab(GroupView::Physical, "Physical")}
                    {side_tab(GroupView::Logical, "Logical")}
                </nav>
            </Show>
        </div>
    }
}

const LIST_SHOWN: usize = 8;

/// One hex id per line, numbered when `numbered`, with the rest behind a toggle past [`LIST_SHOWN`].
fn id_list(ids: Vec<String>, numbered: bool) -> impl IntoView {
    let all = RwSignal::new(false);
    let more = ids.len().saturating_sub(LIST_SHOWN);
    let ids = StoredValue::new(ids);
    view! {
        <ul class="id-list mono">
            {move || ids.with_value(|ids| {
                let n = if all.get() { ids.len() } else { ids.len().min(LIST_SHOWN) };
                ids[..n].iter().enumerate().map(|(i, id)| view! {
                    <li>{numbered.then(|| view! { <span class="dim">{i}</span> })}{id.clone()}</li>
                }).collect_view()
            })}
        </ul>
        {(more > 0).then(|| view! {
            <button class="link" on:click=move |_| all.update(|a| *a = !*a)>
                {move || if all.get() { "show fewer".to_string() } else { format!("+{more} more") }}
            </button>
        })}
    }
}

#[component]
fn ChunkFacts(chunk: Chunk) -> impl IntoView {
    let c = chunk;
    let facts = &c.facts;
    let change = facts.change.clone();
    view! {
        <div class="card">
            <div class="card-title">
                <span class=format!("chip k-{}", color_key(&c))>{c.kind.label()}</span>
                <b>{format!("chunk #{}", c.index)}</b>
                <span>{fmt::bytes(c.len())}</span>
                <span class="dim">{format!("bytes {}..{}", c.range.start, c.range.end)}</span>
                {c.inflated_len.map(|n| view! { <span class="dim">{format!("deflated from {}", fmt::bytes(n))}</span> })}
                <span class=if c.checksum_ok { "ok" } else { "bad" } title=format!("checksum {}\nhash {}", hex_bytes(&c.checksum), c.hash_hex())>
                    {if c.checksum_ok { "checksum ✓" } else { "checksum ✗" }}
                </span>
            </div>
            {c.error.clone().map(|e| view! { <div class="error">{e}</div> })}
            {dep_warning(&c).map(|w| view! { <div class=format!("dep-warning k-{}", color_key(&c))>{format!("This change {w}.")}</div> })}
            <dl class="hdr">
                {change.map(|h| view! {
                    <dt>"change"</dt>
                    <dd>
                        {format!("seq {} · start op {} · {}", h.seq, h.start_op, format_time(h.time))}
                        {h.message.map(|m| view! { " · " <i>{format!("“{m}”")}</i> })}
                    </dd>
                })}
                <dt>"actors"</dt>
                <dd>{id_list(facts.actors.clone(), true)}</dd>
                {(!facts.heads.is_empty()).then(|| view! { <dt>"heads"</dt><dd>{id_list(facts.heads.clone(), false)}</dd> })}
                {(facts.change.is_some() || !facts.deps.is_empty()).then(|| view! {
                    <dt>"deps"</dt>
                    <dd>{if facts.deps.is_empty() { view! { <span class="dim">"none"</span> }.into_any() } else { id_list(facts.deps.clone(), false).into_any() }}</dd>
                })}
            </dl>
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
    let has_group = move || with_chunk(app, |_, c| c.group(ops).is_some()).unwrap_or(false);
    let title = if ops { "Op set" } else { "Change metadata" };
    view! {
        <Show when=has_group fallback=move || (!ops).then(|| view! {
            <div class="card group empty">
                <div class="card-title"><b>{title}</b></div>
                <p class="dim">"Stored in the chunk header (see above)."</p>
            </div>
        })>
            <div class="card group" class:logical=move || view_of() == GroupView::Logical>
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
                </div>
                {move || match view_of() {
                    GroupView::Physical | GroupView::Raw => {
                        let grouped = view_of() == GroupView::Physical;
                        let strip_tab = move |v: GroupView, label: &'static str| view! {
                            <button class:active=move || view_of() == v on:click=move |_| {
                                app.group_view.update(|g| if ops { g.1 = v } else { g.0 = v });
                                app.column.set(None);
                            }>{label}</button>
                        };
                        view! {
                            <nav class="strip-tabs" class:hidden=move || app.column.get().is_some()>
                                {strip_tab(GroupView::Physical, "Grouped")}
                                {strip_tab(GroupView::Raw, "Raw")}
                            </nav>
                            <div class="col-view" class:open=move || app.column.get().is_some_and(|c| c.ops == ops)>
                                <div class="cols-wrap"><ColumnStrip ops=ops grouped=grouped/></div>
                                <section class="col-detail">
                                    {if grouped { view! { <RunPanel/> }.into_any() } else { view! { <RawColumn/> }.into_any() }}
                                </section>
                            </div>
                        }.into_any()
                    }
                    GroupView::Logical => view! { <RowsView ops=ops/> }.into_any(),
                }}
            </div>
        </Show>
    }
}

const STRIP_H: f64 = 320.0;

/// The group's columns side by side, each as tall as its stored bytes.
/// When `grouped`, columns encoding one field highlight and open together.
#[component]
fn ColumnStrip(ops: bool, grouped: bool) -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        with_chunk(app, |f, c| {
            let g = c.group(ops)?;
            let file_len = f.report.len;
            let total = g.stored_len().max(1);
            let max = g.columns.iter().map(|c| c.stored_len).max().unwrap_or(1).max(1);
            let rows = g.rows;
            let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
            for (i, col) in g.columns.iter().enumerate() {
                match spans.last_mut() {
                    Some(r) if grouped && same_field(&g.columns[r.start], col) => r.end = i + 1,
                    _ => spans.push(i..i + 1),
                }
            }
            Some(view! {
                <div class="col-strip" class:grouped=grouped>
                    {spans.into_iter().map(|span| {
                        let me = ColumnRef { ops, index: span.start };
                        let label = (grouped && span.len() > 1).then(|| field_title(&g.columns[span.clone()]));
                        let bytes: usize = g.columns[span.clone()].iter().map(|c| c.stored_len.max(1)).sum();
                        view! {
                            <div class="strip-group" class:sel=move || app.column.get() == Some(me)
                                style=format!("--b: {bytes}")
                                on:click=move |_| app.column.update(|c| *c = (*c != Some(me)).then_some(me))>
                                {grouped.then(|| view! { <span class="group-label">{label}</span> })}
                                <div class="group-cols">
                    {g.columns[span].iter().map(|col| {
                        let ty = col.col_type().short();
                        let h = (STRIP_H * col.stored_len as f64 / max as f64).max(2.0);
                        let title = format!(
                            "{} · {ty}\n{} · {} of group · {} of file · {} B/row{}",
                            col.name,
                            fmt::bytes(col.stored_len),
                            fmt::pct(col.stored_len, total),
                            fmt::pct(col.stored_len, file_len),
                            fmt::per_row(col.stored_len, rows),
                            if col.deflated() { format!("\ndeflated from {}", fmt::bytes(col.data.len())) } else { String::new() }
                        );
                        view! {
                            <button class="strip-col"
                                style=format!("--cc: var(--t-{ty}); --b: {}", col.stored_len.max(1))
                                title=title>
                                <span class="strip-bar-area">
                                    <span class="strip-bytes">{fmt::bytes(col.stored_len)}</span>
                                    <span class=format!("strip-bar t-{ty}") style:height=format!("{h}px")></span>
                                </span>
                                <span class="strip-name">{col.name.clone()}{col.deflated().then(|| " z")}</span>
                            </button>
                        }
                    }).collect_view()}
                                </div>
                            </div>
                        }
                    }).collect_view()}
                </div>
            })
        })
        .flatten()
    }
}

fn pager(
    page: RwSignal<usize>,
    pages: impl Fn() -> usize + Copy + Send + Sync + 'static,
    total: usize,
    unit: &'static str,
) -> impl IntoView {
    view! {
        <div class="pager">
            <button disabled=move || page.get() == 0 on:click=move |_| page.set(0)>"«"</button>
            <button disabled=move || page.get() == 0 on:click=move |_| page.update(|p| *p = p.saturating_sub(1))>"‹"</button>
            <span>{move || format!("page {} / {} · {} {unit}", page.get() + 1, pages().max(1), fmt::count(total))}</span>
            <button disabled=move || { page.get() + 1 >= pages() } on:click=move |_| page.update(|p| *p += 1)>"›"</button>
            <button disabled=move || { page.get() + 1 >= pages() } on:click=move |_| page.set(pages().saturating_sub(1))>"»"</button>
        </div>
    }
}

const ROWS_PAGE: usize = 200;

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
        let actors = with_chunk(app, |_, c| c.facts.actors.clone()).unwrap_or_default();
        Some(view! {
            {pager(page, move || pages, table.rows, "rows")}
            <div class="rows-wrap">
                <table class="dense rows mono">
                    <thead><tr>
                        {table.shown().into_iter().map(|(name, _)| view! { <th>{name}</th> }).collect_view()}
                    </tr></thead>
                    <tbody>
                        {move || t.with_value(|t| {
                            let start = page.get() * ROWS_PAGE;
                            let cols = t.shown();
                            (start..(start + ROWS_PAGE).min(t.rows)).map(|r| view! {
                                <tr>
                                    {cols.iter().map(|&(_, col)| {
                                        let v = t.render_shown(col, r, &actors);
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

const RUNS_PAGE: usize = 100;
const RAW_HEX_MAX: usize = 48;
const RUN_ROW_H: f64 = 22.0;
const FIT_MARGIN: f64 = 40.0;

fn fmt_cell(col: &ColumnInfo, c: &Cell, actors: &[String]) -> String {
    match (col.col_type(), c) {
        (ColType::ValueMetadata, Cell::U(v)) => format!("{}·{}", value_type_name(v & 0xf), v >> 4),
        (ColType::Actor, Cell::U(v)) => {
            let a = actors.get(*v as usize).map(|s| fmt::short(s, 4)).unwrap_or_else(|| "?".into());
            format!("{v} ({a})")
        }
        (ColType::DeltaInteger, Cell::I(v)) => format!("{v:+}"),
        (ColType::String, Cell::S(s)) => format!("{:?}", fmt::short(s, 40)),
        (_, c) => c.to_string(),
    }
}

fn hex_spaced(b: &[u8]) -> String {
    let mut s: String = b.iter().take(RAW_HEX_MAX).map(|x| format!("{x:02x} ")).collect();
    if b.len() > RAW_HEX_MAX {
        s.push('…');
    }
    s
}

/// One cell of a run table: a whole repeat, null or bool run, one value of a
/// literal run, or one row's raw value bytes.
struct RunLine {
    kind: RunKind,
    rows: std::ops::Range<usize>,
    value: String,
    title: String,
}

/// A column cut into the lines of its run table, with the row each line starts at.
enum ColLines {
    Runs { col: ColumnInfo, runs: DecodedRuns, line_rows: Vec<usize>, run_first_line: Vec<usize> },
    /// value.raw split by row; `decoded` shows each row's materialized value instead of its bytes.
    Raw { col: ColumnInfo, lines: Vec<(std::ops::Range<usize>, std::ops::Range<usize>, u64)>, line_rows: Vec<usize>, decoded: bool },
}

impl ColLines {
    fn runs(col: ColumnInfo) -> Self {
        let runs = decode_runs(&col);
        let mut line_rows = Vec::new();
        let mut run_first_line = Vec::with_capacity(runs.runs.len());
        for r in &runs.runs {
            run_first_line.push(line_rows.len());
            match r.kind {
                RunKind::Literal => line_rows.extend(r.first_row..r.first_row + r.values.len()),
                _ => line_rows.push(r.first_row),
            }
        }
        ColLines::Runs { col, runs, line_rows, run_first_line }
    }

    /// Raw values split by row, with consecutive empty values merged when one
    /// metadata run covers them.
    fn raw(col: ColumnInfo, meta: &DecodedRuns) -> Self {
        let mut lines = Vec::new();
        let mut pos = 0;
        for run in &meta.runs {
            let code = |v: Option<&Cell>| v.and_then(Cell::as_u64).unwrap_or(0);
            let mut each = |row: usize, m: u64| {
                let len = (m >> 4) as usize;
                lines.push((row..row + 1, pos..pos + len, m & 0xf));
                pos += len;
            };
            match run.kind {
                RunKind::Literal => run.values.iter().enumerate().for_each(|(i, v)| each(run.first_row + i, code(Some(v)))),
                _ if code(run.values.first()) >> 4 == 0 => {
                    lines.push((run.first_row..run.first_row + run.count, pos..pos, code(run.values.first()) & 0xf))
                }
                _ => (0..run.count).for_each(|i| each(run.first_row + i, code(run.values.first()))),
            }
        }
        let line_rows = lines.iter().map(|l| l.0.start).collect();
        ColLines::Raw { col, lines, line_rows, decoded: false }
    }

    fn col(&self) -> &ColumnInfo {
        match self {
            ColLines::Runs { col, .. } | ColLines::Raw { col, .. } => col,
        }
    }

    fn line_rows(&self) -> &[usize] {
        match self {
            ColLines::Runs { line_rows, .. } | ColLines::Raw { line_rows, .. } => line_rows,
        }
    }

    fn rows(&self) -> usize {
        match self {
            ColLines::Runs { runs, .. } => runs.stats.rows,
            ColLines::Raw { lines, .. } => lines.last().map_or(0, |l| l.0.end),
        }
    }

    /// The lines overlapping `rows`.
    fn lines(&self, rows: std::ops::Range<usize>, actors: &[String]) -> Vec<RunLine> {
        let first = self.line_rows().partition_point(|&r| r <= rows.start).saturating_sub(1);
        let last = self.line_rows().partition_point(|&r| r < rows.end);
        let mut out = Vec::with_capacity(last.saturating_sub(first));
        match self {
            ColLines::Raw { col, lines, decoded, .. } => {
                for (r, b, code) in &lines[first..last.min(lines.len())] {
                    let bytes = col.data.get(b.clone()).unwrap_or_default();
                    let value = if col.name.starts_with("extra") { slice_hex(bytes) } else { decode_value(*code, bytes).to_string() };
                    let hex = hex_spaced(bytes);
                    let (value, title) = match (*decoded, r.len()) {
                        (true, 1) => (value, format!("{} bytes: {hex}", b.len())),
                        (true, n) => (format!("{n} × {value}"), format!("{} bytes: {hex}", b.len())),
                        (false, _) => (hex, format!("{value} · {} bytes", b.len())),
                    };
                    out.push(RunLine { kind: RunKind::Raw, rows: r.clone(), value, title });
                }
            }
            ColLines::Runs { col, runs, run_first_line, .. } => {
                let mut i = run_first_line.partition_point(|&l| l <= first).saturating_sub(1);
                while i < runs.runs.len() && run_first_line[i] < last {
                    let run = &runs.runs[i];
                    let skip = first.saturating_sub(run_first_line[i]);
                    run_lines(col, run, actors, skip, last - run_first_line[i], &mut out);
                    i += 1;
                }
            }
        }
        out
    }
}

fn run_lines(col: &ColumnInfo, run: &Run, actors: &[String], skip: usize, until: usize, out: &mut Vec<RunLine>) {
    let rows = run.first_row..run.first_row + run.count;
    let encoded = |b: &std::ops::Range<usize>| format!("{} bytes: {}", b.len(), hex_spaced(&col.data[b.clone()]));
    if run.kind != RunKind::Literal {
        let value = match (run.kind, run.values.first(), run.delta_span) {
            (RunKind::Null, ..) => format!("{} × null", run.count),
            (_, Some(v), Some((a, b))) => format!("{} × {}  → {a}..{b}", run.count, fmt_cell(col, v, actors)),
            (_, Some(v), None) => format!("{} × {}", run.count, fmt_cell(col, v, actors)),
            _ => String::new(),
        };
        let title = format!("{} run · rows {}..{} · {}", run.kind.label(), rows.start, rows.end, encoded(&run.bytes));
        out.push(RunLine { kind: run.kind, rows, value, title });
        return;
    }
    let mut abs = run.delta_span.map(|(first, _)| first);
    for (i, v) in run.values.iter().enumerate().take(until) {
        if i > 0 {
            if let (Some(a), Cell::I(d)) = (abs.as_mut(), v) {
                *a = a.wrapping_add(*d);
            }
        }
        if i < skip {
            continue;
        }
        let start = if i == 0 { run.bytes.start } else { run.value_starts[i] };
        let end = run.value_starts.get(i + 1).copied().unwrap_or(run.bytes.end);
        let value = match abs {
            Some(a) => format!("{a}  ({})", fmt_cell(col, v, actors)),
            None => fmt_cell(col, v, actors),
        };
        let title = format!("literal · row {} · {}", run.first_row + i, encoded(&(start..end)));
        out.push(RunLine { kind: run.kind, rows: run.first_row + i..run.first_row + i + 1, value, title });
    }
}

const RAW_SHOWN: usize = 64 * 1024;

/// The selected column's bytes exactly as stored in the file.
#[component]
fn RawColumn() -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        let sel = app.column.get()?;
        let hex = with_chunk(app, |f, c| {
            let col = c.group(sel.ops)?.columns.get(sel.index)?;
            let bytes = col
                .file_offset
                .and_then(|o| f.bytes.get(o..o + col.stored_len))
                .unwrap_or(&col.data);
            let mut hex: String = bytes.iter().take(RAW_SHOWN).map(|x| format!("{x:02x} ")).collect();
            if bytes.len() > RAW_SHOWN {
                hex.push_str(&format!("… {} more bytes", bytes.len() - RAW_SHOWN));
            }
            Some(hex)
        })
        .flatten()?;
        Some(view! {
            <div class="card runs">
                <button class="link runs-close" on:click=move |_| app.column.set(None)>"close ✕"</button>
                <div class="hex raw-col">{hex}</div>
            </div>
        })
    }
}

/// The name of a field made of several columns, e.g. `Obj` or `ElemId`.
fn field_title(cols: &[ColumnInfo]) -> String {
    let base = cols[0].name.split('.').next().unwrap_or_default();
    match base {
        "key" => "ElemId".into(),
        "obj" | "id" | "succ" | "pred" | "value" | "deps" | "extra" => base[..1].to_uppercase() + &base[1..],
        _ => base.to_string(),
    }
}

/// Whether two columns encode the same field, e.g. an op id's actor and
/// counter; a key's string stands apart from its element id.
fn same_field(a: &ColumnInfo, b: &ColumnInfo) -> bool {
    a.spec.id() == b.spec.id() && (a.col_type() == ColType::String) == (b.col_type() == ColType::String)
}

fn col_order(c: &ColumnInfo) -> u8 {
    match c.col_type() {
        ColType::Group => 0,
        ColType::DeltaInteger | ColType::Integer => 1,
        ColType::Actor => 2,
        _ => 3,
    }
}

/// The selected field's columns: a summary of each, then their runs side by
/// side, every run one cell spanning the rows it covers.
#[component]
fn RunPanel() -> impl IntoView {
    let app = expect_context::<AppState>();
    let cache: Cache<(usize, usize, ColumnRef), Vec<ColLines>> = Cache::new();
    move || {
        let sel = app.column.get()?;
        let (cols, actors) = with_chunk(app, |f, c| {
            let all = &c.group(sel.ops)?.columns;
            let first = all.get(sel.index)?;
            let n = all[sel.index..].iter().take_while(|c| same_field(first, c)).count();
            let cols = cache.get((file_key(f), c.index, sel), || {
                let mut picked: Vec<ColumnInfo> = all[sel.index..sel.index + n].to_vec();
                picked.sort_by_key(col_order);
                let mut out: Vec<ColLines> = Vec::new();
                for col in picked {
                    if col.col_type() == ColType::Value {
                        let meta = out.iter().find_map(|l| match l {
                            ColLines::Runs { col, runs, .. } if col.col_type() == ColType::ValueMetadata => Some(runs.clone()),
                            _ => None,
                        });
                        let raw = ColLines::raw(col, &meta.unwrap_or(DecodedRuns { runs: vec![], stats: Default::default(), error: None }));
                        if let ColLines::Raw { col, lines, line_rows, .. } = &raw {
                            let materialized = ColLines::Raw { col: col.clone(), lines: lines.clone(), line_rows: line_rows.clone(), decoded: true };
                            out.push(raw);
                            out.push(materialized);
                        }
                    } else {
                        out.push(ColLines::runs(col));
                    }
                }
                out
            });
            Some((cols, c.facts.actors.clone()))
        })
        .flatten()?;
        let (counts, items): (Vec<usize>, Vec<usize>) =
            (0..cols.len()).partition(|&i| cols[i].col().col_type() == ColType::Group);
        Some(view! {
            <div class="card runs">
                <button class="link runs-close" on:click=move |_| app.column.set(None)>"close ✕"</button>
                <div class="run-tables">
                    {[counts, items].into_iter().filter(|s| !s.is_empty()).map(|set| {
                        view! { <RunTable cols=cols.clone() set=set actors=actors.clone()/> }
                    }).collect_view()}
                </div>
            </div>
        })
    }
}

/// Columns that share row numbers, side by side: the first column splits the
/// rows wherever any of their runs starts. Pages are sized to fit the window.
#[component]
fn RunTable(cols: Rc<Vec<ColLines>>, set: Vec<usize>, actors: Vec<String>) -> impl IntoView {
    let total = set.iter().map(|&i| cols[i].rows()).max().unwrap_or(0);
    let mut bounds: Vec<usize> = set.iter().flat_map(|&i| cols[i].line_rows().iter().copied()).collect();
    bounds.push(total);
    bounds.sort_unstable();
    bounds.dedup();
    let segments = bounds.len().saturating_sub(1);
    let page = RwSignal::new(0usize);
    let page_size = RwSignal::new(RUNS_PAGE);
    let body_el = NodeRef::<leptos::html::Tbody>::new();
    fn fit(body_el: NodeRef<leptos::html::Tbody>, page: RwSignal<usize>, page_size: RwSignal<usize>, tries: u8) {
        let Some(Some(tbody)) = body_el.try_get_untracked() else { return };
        let w = window();
        let r = tbody.get_bounding_client_rect();
        let shown = tbody.child_element_count();
        let row_h = if shown > 0 { r.height() / shown as f64 } else { RUN_ROW_H };
        let height = w.inner_height().ok().and_then(|h| h.as_f64()).unwrap_or(800.0);
        let top = r.top() + w.scroll_y().unwrap_or(0.0);
        let rows = ((height - top - FIT_MARGIN) / row_h).floor().max(5.0) as usize;
        if page_size.get_untracked() != rows {
            let first = page.get_untracked() * page_size.get_untracked();
            page_size.set(rows);
            page.set(first / rows);
            if tries > 0 {
                request_animation_frame(move || fit(body_el, page, page_size, tries - 1));
            }
        }
    }
    request_animation_frame(move || fit(body_el, page, page_size, 2));
    let resize = window_event_listener(leptos::ev::resize, move |_| fit(body_el, page, page_size, 2));
    on_cleanup(move || resize.remove());
    let pages = move || segments.div_ceil(page_size.get().max(1));
    let bounds = StoredValue::new(bounds);
    let cols = StoredValue::new_local(cols);
    let heads = cols.with_value(|cols| {
        set.iter()
            .map(|&i| {
                let col = cols[i].col();
                if matches!(cols[i], ColLines::Raw { decoded: true, .. }) {
                    let base = col.name.split('.').next().unwrap_or_default().to_string();
                    let title = format!("each row's value, read from {base}.meta and {base}.raw together");
                    return view! {
                        <th class="colhead materialized" title=title><b>{base}</b><span>"materialized"</span></th>
                    }.into_any();
                }
                let title = format!(
                    "{} · {}{}",
                    col.col_type().short(),
                    fmt::bytes(col.stored_len),
                    if col.deflated() { format!(", {} inflated", fmt::bytes(col.data.len())) } else { String::new() }
                );
                view! {
                    <th class="colhead" style=format!("--cc: var(--t-{})", col.col_type().short()) title=title>
                        <b>{col.name.clone()}</b>
                        <span>{format!("{} bytes, {} segments", fmt::count_exact(col.stored_len), fmt::count_exact(cols[i].line_rows().len()))}</span>
                    </th>
                }.into_any()
            })
            .collect_view()
    });
    let body = move || {
        let (p, size) = (page.get(), page_size.get());
        let (s0, s1) = ((p * size).min(segments), ((p + 1) * size).min(segments));
        let segs: Vec<std::ops::Range<usize>> = bounds.with_value(|b| (s0..s1).map(|k| b[k]..b[k + 1]).collect());
        let Some(rows) = segs.first().zip(segs.last()).map(|(a, b)| a.start..b.end) else {
            return vec![];
        };
        let mut grid: Vec<Vec<Option<(RunLine, usize)>>> = (0..segs.len()).map(|_| set.iter().map(|_| None).collect()).collect();
        cols.with_value(|cols| {
            for (c, &i) in set.iter().enumerate() {
                for line in cols[i].lines(rows.clone(), &actors) {
                    let from = segs.partition_point(|s| s.end <= line.rows.start);
                    let to = segs.partition_point(|s| s.start < line.rows.end);
                    if from < to {
                        grid[from][c] = Some((line, to - from));
                    }
                }
            }
        });
        grid.into_iter().zip(segs).map(|(cells, seg)| view! {
            <tr>
                <td class="seg">{if seg.len() == 1 { seg.start.to_string() } else { format!("{}..{}", seg.start, seg.end) }}</td>
                {cells.into_iter().flatten().map(|(l, span)| view! {
                    <td class=format!("val rk-{}", l.kind.label()) rowspan=span title=l.title>{l.value}</td>
                }).collect_view()}
            </tr>
        }).collect::<Vec<_>>()
    };
    view! {
        <div class="run-table">
            <div class="run-table-top">
                <b>{format!("{} rows", fmt::count_exact(total))}</b>
                {pager(page, pages, segments, "row ranges")}
            </div>
            <table class="dense mono runs-table">
                <thead><tr><th class="colhead rows-head"><b>"rows"</b></th>{heads}</tr></thead>
                <tbody node_ref=body_el>{body}</tbody>
            </table>
        </div>
    }
}
