use std::rc::Rc;

use am_inspect::chunk::{Chunk, DepOrder, SectionKind};
use am_inspect::columns::{
    build_rows, decode_runs, value_breakdown, value_type_name, Cell, ColType, ColumnInfo, DecodedRuns, Run,
    RunKind, RowTable,
};
use am_inspect::doc::short_hex;
use am_inspect::format_time;
use leptos::context::Provider;
use leptos::prelude::*;

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
        let original = app.file.with(|f| f.as_ref().map(|f| f.bytes.len()))?;
        let saved = app.saved.with(|f| f.as_ref().map(|f| f.bytes.len()));
        let failed = app.model.with(|m| m.as_deref().is_some_and(|m| m.is_err()));
        Some(match saved {
            Some(n) => format!(
                "What doc.save() writes after loading this file: {} ({} of the original {}).",
                fmt::bytes(n),
                fmt::pct(n, original.max(1)),
                fmt::bytes(original)
            ),
            None if failed => "The file didn't load as a document, so there's nothing to save.".into(),
            None => "Loading the document…".into(),
        })
    };
    view! {
        <p class="dim save-note">{note}</p>
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

fn select_chunk(app: AppState, index: usize) {
    app.chunk.set(Some(index));
    app.part.set(None);
    app.column.set(None);
    window().scroll_to_with_x_and_y(0.0, 0.0);
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
    match c.dep_order {
        DepOrder::InOrder => c.kind.key(),
        DepOrder::Late => "late",
        DepOrder::Missing => "missing",
    }
}

fn dep_warning(c: &Chunk) -> Option<&'static str> {
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
/// band at the left, and clicking the band closes the chunk.
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
                        let h = item.height().to_string();
                        match item {
                            StackItem::Block { index, .. } => view! {
                                <div class=format!("blk k-{}", color_key(&f.report.chunks[index]))
                                    class:sel=move || app.chunk.get() == Some(index)
                                    style:--h=h></div>
                            }
                            .into_any(),
                            StackItem::Bands { first, last } => view! {
                                <div class="bands"
                                    class:sel=move || app.chunk.get().is_some_and(|c| (first..=last).contains(&c))
                                    style:--h=h>
                                    <div class="band-stripes">
                                        {f.report.chunks[first..=last].iter()
                                            .map(|c| view! { <i class=format!("k-{}", color_key(c))></i> })
                                            .collect_view()}
                                    </div>
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
                <button class="stack-close" title="Back to all chunks"
                    on:click=move |_| { app.chunk.set(None); app.part.set(None); app.column.set(None); }>
                    <span class="stack-title" node_ref=title_el class:placed=move || title_y.get().is_some()
                        style:top=move || title_y.get().map(|y| format!("{y}px"))>{move || with_chunk(app, |_, c| format!("{} #{}", c.kind.label(), c.index))}</span>
                    <span class="stack-back">"‹"</span>
                </button>
            </Show>
        </div>
    }
}

fn parts(c: &Chunk) -> Vec<Part> {
    let mut parts = vec![Part::Header];
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
        Part::Header => "header",
        Part::Changes => "changecols",
        Part::Ops => "opcols",
        Part::Extra => "suffix",
    }
}

fn part_name(p: Part) -> &'static str {
    match p {
        Part::Header => "Header",
        Part::Changes => "Changes",
        Part::Ops => "Ops",
        Part::Extra => "Extra",
    }
}

fn part_bytes(c: &Chunk, p: Part) -> usize {
    let group = |ops: bool| c.group(ops).map_or(0, |g| g.stored_len());
    match p {
        Part::Header => c.len() - group(false) - group(true) - part_bytes(c, Part::Extra),
        Part::Changes => group(false),
        Part::Ops => group(true),
        Part::Extra => extra_ranges(c).map(|r| r.len()).sum(),
    }
}

fn part_label(c: &Chunk, p: Part) -> AnyView {
    let rows = |ops: bool| c.group(ops).map_or(0, |g| g.rows);
    match p {
        Part::Header => {
            let n = c.facts.actors.len();
            view! { "Header: " <b>{fmt::count_exact(n)}</b> {if n == 1 { " actor" } else { " actors" }} }.into_any()
        }
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
                            title=move || if app.part.get() == Some(p) { "Back to the chunk's parts" } else { "" }
                            on:click=move |_| {
                                app.part.update(|cur| *cur = if *cur == Some(p) { None } else { Some(p) });
                                app.column.set(None);
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
            Part::Header => with_chunk(app, |f, c| view! { <ChunkFacts chunk=c.clone() file_len=f.report.len/> }.into_any())?,
            Part::Changes => view! { <GroupPanel ops=false/><RunPanel/> }.into_any(),
            Part::Ops => view! { <GroupPanel ops=true/><RunPanel/> }.into_any(),
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
        let active = move || {
            let (a, b) = app.group_view.get();
            ops_part().is_some_and(|ops| if ops { b == v } else { a == v })
        };
        view! {
            <button class:active=active on:click=move |_| {
                let ops = ops_part().unwrap_or(false);
                app.group_view.update(|g| if ops { g.1 = v } else { g.0 = v });
                app.column.set(None);
            }><span>{label}</span></button>
        }
    };
    view! {
        <div class="chunk-view" class:open=move || app.part.get().is_some()>
            <div class="parts-wrap">
                {move || with_chunk(app, |_, c| dep_warning(c).map(|w| view! {
                    <div class=format!("dep-warning k-{}", color_key(c))>{format!("This change {w}.")}</div>
                })).flatten()}
                <div class="parts">{blocks}</div>
            </div>
            <section class="part-detail">{detail}</section>
            <Show when=move || ops_part().is_some()>
                <nav class="side-tabs">
                    {side_tab(GroupView::Physical, "Physical")}
                    {side_tab(GroupView::Logical, "Logical")}
                </nav>
            </Show>
        </div>
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
                <span class=format!("chip k-{}", color_key(&c))>{c.kind.label()}</span>
                <b>{format!("chunk #{}", c.index)}</b>
                <span>{fmt::bytes(c.len())}</span>
                <span class="dim">{format!("{} of file · bytes {}..{}", fmt::pct(c.len(), file_len), c.range.start, c.range.end)}</span>
                <span class=if c.checksum_ok { "ok" } else { "bad" } title=format!("hash {}", c.hash_hex())>
                    {if c.checksum_ok { "checksum ✓" } else { "checksum ✗" }}
                    " " <span class="mono">{hex_bytes(&c.checksum)}</span>
                </span>
            </div>
            {c.error.clone().map(|e| view! { <div class="error">{e}</div> })}
            {dep_warning(&c).map(|w| view! { <div class=format!("dep-warning k-{}", color_key(&c))>{format!("This change {w}.")}</div> })}
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
                    GroupView::Physical => view! { <ColumnStrip ops=ops/> }.into_any(),
                    GroupView::Logical => view! { <RowsView ops=ops/> }.into_any(),
                }}
            </div>
        </Show>
    }
}

const STRIP_H: f64 = 320.0;

/// The group's columns side by side, each as tall as its stored bytes.
#[component]
fn ColumnStrip(ops: bool) -> impl IntoView {
    let app = expect_context::<AppState>();
    move || {
        with_chunk(app, |f, c| {
            let g = c.group(ops)?;
            let file_len = f.report.len;
            let total = g.stored_len().max(1);
            let max = g.columns.iter().map(|c| c.stored_len).max().unwrap_or(1).max(1);
            let rows = g.rows;
            Some(view! {
                <div class="col-strip">
                    {g.columns.iter().enumerate().map(|(index, col)| {
                        let me = ColumnRef { ops, index };
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
                            <button class="strip-col" class:sel=move || app.column.get() == Some(me)
                                on:click=move |_| app.column.set(Some(me)) title=title>
                                <span class="strip-bar-area">
                                    <span class="strip-bytes">{fmt::bytes(col.stored_len)}</span>
                                    <span class=format!("strip-bar t-{ty}") style:height=format!("{h}px")></span>
                                </span>
                                <span class="strip-name">{col.name.clone()}{col.deflated().then(|| " z")}</span>
                            </button>
                        }
                    }).collect_view()}
                </div>
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
            {pager(page, pages, table.rows, "rows")}
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
