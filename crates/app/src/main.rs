mod document;
mod fmt;
mod state;
mod storage;
mod worker;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use std::rc::Rc;

use state::{AppState, Loaded, Tab};

const APP_VERSION: &str = match option_env!("APP_VERSION") {
    Some(v) => v,
    None => "dev",
};
const APP_COMMIT: &str = match option_env!("APP_COMMIT") {
    Some(c) => c,
    None => "dev",
};

const SAMPLES: &[(&str, &str)] = &[
    ("todo-save-plus-incrementals.automerge", "save() then 104 × save_incremental(): bulk imports between runs of single edits"),
    ("text-next-save-plus-incrementals.automerge", "next API: change-set save + incremental change sets"),
    ("text-next-changeset.automerge", "whole document as one change set chunk"),
    ("text-doc.automerge", "collaborative rich text, 3 authors, marks"),
    ("todo-doc.automerge", "maps, lists, counters, all scalar types"),
    ("text-changes.automerge", "121 individual change chunks"),
    ("two_change_chunks_compressed.automerge", "deflate-compressed change chunks"),
    ("bundle-v0-3.3.2.automerge", "frozen bundle format from automerge 3.3.2"),
    ("monday-meeting-notes.automerge", "real-world doc, 15k changes, deflated columns"),
    ("essay.amrg", "real-world doc chunk + 449 change chunks, 263k changes"),
    ("godot-modeled.amrg", "real-world doc, Godot scene model"),
    ("64bit_obj_id_change.automerge", "invalid: 64-bit object counter (error handling)"),
];

async fn read_file(file: web_sys::File) -> Result<Vec<u8>, String> {
    let buf = JsFuture::from(file.array_buffer())
        .await
        .map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

async fn fetch_bytes(url: String) -> Result<Vec<u8>, String> {
    let resp = JsFuture::from(window().fetch_with_str(&url))
        .await
        .map_err(|e| format!("{e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "bad response".to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

fn open_file(app: AppState, file: web_sys::File) {
    let name = file.name();
    spawn_local(async move {
        match read_file(file).await {
            Ok(bytes) => app.open(name, bytes),
            Err(e) => app.status.set(Some(format!("Could not read file: {e}"))),
        }
    });
}

fn open_sample(app: AppState, name: &'static str) {
    app.status.set(Some(format!("Fetching {name}…")));
    spawn_local(async move {
        match fetch_bytes(format!("samples/{name}")).await {
            Ok(bytes) => app.open(name.to_string(), bytes),
            Err(e) => app.status.set(Some(format!("Could not fetch {name}: {e}"))),
        }
    });
}

#[component]
fn Header() -> impl IntoView {
    let app = expect_context::<AppState>();
    let input: NodeRef<html::Input> = NodeRef::new();
    let on_change = move |_| {
        if let Some(f) = input
            .get()
            .and_then(|i| i.files())
            .and_then(|fs| fs.get(0))
        {
            open_file(app, f);
        }
    };
    let summary = move || {
        app.file.with(|f| {
            f.as_ref().map(|f| {
                let r = &f.report;
                let changes: usize = r.chunks.iter().map(|c| c.num_changes()).sum();
                let ops: usize = r.chunks.iter().map(|c| c.num_ops()).sum();
                view! {
                    <span class="file-name" title=f.name.clone()>{f.name.clone()}</span>
                    <span class="pill">{fmt::bytes(r.len)}</span>
                    <span class="pill">{format!("{} chunks", r.chunks.len())}</span>
                    <span class="pill">{format!("{} changes", fmt::count(changes))}</span>
                    <span class="pill">{format!("{} ops", fmt::count(ops))}</span>
                }
            })
        })
    };
    view! {
        <header class="top">
            <div class="brand">
                <button class="home" title="Back to the start page" on:click=move |_| {
                    if let Some(i) = input.get() { i.set_value(""); }
                    app.close();
                }>
                    <svg viewBox="0 0 32 32" class="logo"><use href="#logo"/></svg>
                    <span>"Automerge Inspector"</span>
                </button>
                <span class="version" title=format!("build {APP_VERSION} · commit {APP_COMMIT}")>{APP_VERSION}</span>
            </div>
            <div class="file-summary">{summary}</div>
            <label class="btn primary open-file">
                "Open file…"
                <input type="file" node_ref=input on:change=on_change hidden/>
            </label>
        </header>
    }
}

#[component]
fn Welcome() -> impl IntoView {
    let app = expect_context::<AppState>();
    view! {
        <div class="welcome">
            <div class="drop-hint">
                <svg viewBox="0 0 32 32" class="logo big"><use href="#logo"/></svg>
                <h1>"Drop an automerge file anywhere"</h1>
                <p>"or use " <b>"Open file…"</b> ". Everything runs locally in your browser; nothing is uploaded."</p>
                <p class="dim">"Documents, change chunks, compressed changes, change sets and v0 bundles — concatenated in any order."</p>
            </div>
            <h2>"Or try a sample"</h2>
            <ul class="samples">
                {SAMPLES
                    .iter()
                    .map(|(name, desc)| {
                        view! {
                            <li>
                                <button class="link" on:click=move |_| open_sample(app, name)>{*name}</button>
                                <span class="dim">{*desc}</span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </div>
    }
}

fn file_stats(file: RwSignal<Option<Rc<Loaded>>, LocalStorage>, load_ms: RwSignal<Option<f64>>) -> String {
    file.with(|f| {
        let Some(f) = f else { return String::new() };
        let r = &f.report;
        let changes: usize = r.chunks.iter().map(|c| c.num_changes()).sum();
        let ops: usize = r.chunks.iter().map(|c| c.num_ops()).sum();
        let load = match load_ms.get() {
            Some(ms) => format!(", loads in {ms:.0}ms"),
            None => String::new(),
        };
        format!(
            "{} chunks, {} bytes, {} changes, {} ops{load}",
            fmt::count_exact(r.chunks.len()),
            fmt::count_exact(r.len),
            fmt::count_exact(changes),
            fmt::count_exact(ops)
        )
    })
}

/// The open file's three views, as labelled folder tabs over one body.
#[component]
fn Folder() -> impl IntoView {
    let app = expect_context::<AppState>();
    let tabs = [
        (Tab::Load, "t-load", "Load", "The file as loaded"),
        (Tab::Document, "t-doc", "Document", "The document's objects, maps and text"),
        (Tab::Save, "t-save", "Save", "What doc.save() writes for this document"),
    ];
    let color = move || tabs.iter().find(|t| t.0 == app.tab.get()).map_or("", |t| t.1);
    let stats = move || match app.tab.get() {
        Tab::Load => file_stats(app.file, app.load_ms.0),
        Tab::Save => file_stats(app.saved, app.load_ms.1),
        Tab::Document => app.doc.with(|d| match d {
            Some(Ok(d)) => format!(
                "{} ops, {} changes, {} actors",
                fmt::count_exact(d.ops as usize),
                fmt::count_exact(d.changes),
                fmt::count_exact(d.actors)
            ),
            _ => String::new(),
        }),
    };
    view! {
        <div class="folder">
            <nav class="folder-tabs">
                {tabs.map(|(tab, color, label, title)| view! {
                    <button class=color class:active=move || app.tab.get() == tab title=title
                        on:click=move |_| app.tab.set(tab)>{label}</button>
                })}
                <div class="folder-stats">{stats}</div>
            </nav>
            <div class=move || format!("folder-body {}", color())>
                {move || match app.tab.get() {
                    Tab::Load => view! { <storage::StorageTab/> }.into_any(),
                    Tab::Document => view! { <document::DocumentTab/> }.into_any(),
                    Tab::Save => view! { <storage::SaveTab/> }.into_any(),
                }}
            </div>
        </div>
    }
}

#[component]
fn App() -> impl IntoView {
    let app = AppState::new();
    provide_context(app);
    let dragging = RwSignal::new(false);
    let on_drop = move |ev: leptos::ev::DragEvent| {
        ev.prevent_default();
        dragging.set(false);
        if let Some(f) = ev
            .data_transfer()
            .and_then(|dt| dt.files())
            .and_then(|fs| fs.get(0))
        {
            open_file(app, f);
        }
    };
    view! {
        <div class="app"
            on:dragover=move |ev: leptos::ev::DragEvent| { ev.prevent_default(); dragging.set(true); }
            on:dragleave=move |ev: leptos::ev::DragEvent| {
                if ev.related_target().is_none() { dragging.set(false); }
            }
            on:drop=on_drop>
            <Header/>
            {move || app.status.get().map(|s| view! { <div class="status">{s}</div> })}
            <main>
                {move || {
                    if app.file.with(|f| f.is_none()) {
                        view! { <Welcome/> }.into_any()
                    } else {
                        view! { <Folder/> }.into_any()
                    }
                }}
            </main>
            <Show when=move || dragging.get()>
                <div class="drop-overlay"><div>"Drop to inspect"</div></div>
            </Show>
        </div>
    }
}

const APP_BUILD: &str = match option_env!("APP_BUILD") {
    Some(b) => b,
    None => "dev",
};

fn main() {
    console_error_panic_hook::set_once();
    if worker::in_worker() {
        worker::serve();
        return;
    }
    worker::start(&format!("worker.js?v={APP_BUILD}"));
    mount_to_body(App);
}
