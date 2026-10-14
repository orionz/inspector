use std::rc::Rc;
use std::time::Duration;

use std::collections::HashSet;

use am_inspect::{parse_structure, FileReport};
use leptos::prelude::*;

use crate::worker::{self, ObjKind, Req, Resp, Summary};

#[derive(Clone)]
pub struct Loaded {
    pub name: String,
    pub bytes: Vec<u8>,
    pub report: FileReport,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Load,
    Document,
    Save,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GroupView {
    Physical,
    Logical,
    Raw,
}

/// What the document view shows for the open object.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DocMode {
    /// Its current keys and values.
    Value,
    /// The ops that built it.
    Ops,
    /// It and everything below it as JSON.
    Json,
}

/// An object opened in the document view, by the key that leads to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Crumb {
    pub key: String,
    pub id: String,
    pub kind: ObjKind,
    pub len: usize,
}

/// A part of the selected chunk that can be opened on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Changes,
    Ops,
    Extra,
}

/// Which column group of the selected chunk: change metadata or ops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ColumnRef {
    pub ops: bool,
    pub index: usize,
}

type Local<T> = RwSignal<T, LocalStorage>;

#[derive(Clone, Copy)]
pub struct AppState {
    pub file: Local<Option<Rc<Loaded>>>,
    /// What `doc.save()` writes for the loaded document.
    pub saved: Local<Option<Rc<Loaded>>>,
    /// How long automerge takes to load the file and the saved file, in ms.
    pub load_ms: (RwSignal<Option<f64>>, RwSignal<Option<f64>>),
    /// The Save tab's chunk selection, kept apart from the Load tab's.
    pub saved_sel: (
        RwSignal<Option<usize>>,
        RwSignal<Option<Part>>,
        RwSignal<Option<ColumnRef>>,
        RwSignal<(GroupView, GroupView)>,
    ),
    /// The document as the worker loaded it; `None` while it loads.
    pub doc: RwSignal<Option<Result<Summary, String>>>,
    pub status: RwSignal<Option<String>>,
    pub tab: RwSignal<Tab>,
    /// The chunk whose detail panels are open; `None` shows the chunk stack.
    pub chunk: RwSignal<Option<usize>>,
    pub part: RwSignal<Option<Part>>,
    pub column: RwSignal<Option<ColumnRef>>,
    pub group_view: RwSignal<(GroupView, GroupView)>,
    /// How many changes of history the document view shows.
    pub step: RwSignal<usize>,
    /// While the history bar is being dragged, the step the drag started at.
    pub drag_from: RwSignal<Option<usize>>,
    /// Objects opened in the document view, from just below the root.
    pub doc_path: RwSignal<Vec<Crumb>>,
    pub doc_mode: RwSignal<DocMode>,
    /// Whether ops later than the shown version stay listed, struck through.
    pub tombstones: RwSignal<bool>,
    /// For a document fetched from a sync server, how much of what it sent was kept.
    pub fetched: RwSignal<Option<String>>,
}

impl AppState {
    pub fn new() -> Self {
        let state = Self {
            file: RwSignal::new_local(None),
            saved: RwSignal::new_local(None),
            load_ms: (RwSignal::new(None), RwSignal::new(None)),
            saved_sel: (
                RwSignal::new(None),
                RwSignal::new(None),
                RwSignal::new(None),
                RwSignal::new((GroupView::Physical, GroupView::Physical)),
            ),
            doc: RwSignal::new(None),
            status: RwSignal::new(None),
            tab: RwSignal::new(Tab::Load),
            chunk: RwSignal::new(None),
            part: RwSignal::new(None),
            column: RwSignal::new(None),
            group_view: RwSignal::new((GroupView::Physical, GroupView::Physical)),
            step: RwSignal::new(0),
            drag_from: RwSignal::new(None),
            doc_path: RwSignal::new(Vec::new()),
            doc_mode: RwSignal::new(DocMode::Value),
            tombstones: RwSignal::new(false),
            fetched: RwSignal::new(None),
        };
        state
    }

    /// Closes the open file and returns to the start page.
    pub fn close(self) {
        self.chunk.set(None);
        self.column.set(None);
        self.part.set(None);
        self.saved_sel.0.set(None);
        self.saved_sel.1.set(None);
        self.saved_sel.2.set(None);
        self.tab.set(Tab::Load);
        self.status.set(None);
        self.fetched.set(None);
        self.doc.set(None);
        self.doc_path.set(Vec::new());
        self.saved.set(None);
        self.load_ms.0.set(None);
        self.load_ms.1.set(None);
        self.file.set(None);
    }

    /// Parses the file in two phases so the storage view appears before the
    /// (much slower) full document load finishes.
    pub fn open(self, name: String, bytes: Vec<u8>) {
        self.status
            .set(Some(format!("Parsing {name} ({})…", crate::fmt::bytes(bytes.len()))));
        set_timeout(
            move || {
                let report = parse_structure(&bytes);
                self.column.set(None);
                self.chunk.set(None);
                self.part.set(None);
                self.saved_sel.0.set(None);
                self.saved_sel.1.set(None);
                self.saved_sel.2.set(None);
                self.group_view
                    .set((GroupView::Physical, GroupView::Physical));
                self.doc.set(None);
                self.doc_path.set(Vec::new());
                self.saved.set(None);
                self.load_ms.0.set(None);
                self.load_ms.1.set(None);
                self.file.set(Some(Rc::new(Loaded { name, bytes, report })));
                self.status.set(Some("Loading document history…".into()));
                let hidden = self.file.with_untracked(|f| f.as_ref().map(|f| f.report.hidden_deps()).unwrap_or_default());
                let bytes = self.file.with_untracked(|f| f.as_ref().map(|f| f.bytes.clone()).unwrap_or_default());
                let opened = self.file.with_untracked(|f| f.as_ref().map(Rc::as_ptr));
                leptos::task::spawn_local(async move {
                    let Some((Resp::Loaded(doc), saved)) =
                        worker::call(None, Req::Load { hidden_deps: hidden }, Some(bytes)).await
                    else {
                        return;
                    };
                    if self.file.with_untracked(|f| f.as_ref().map(Rc::as_ptr)) != opened {
                        return;
                    }
                    if let Ok(sum) = &doc {
                        self.load_ms.0.set(Some(sum.load_ms));
                        let known: HashSet<&str> = sum.known_deps.iter().map(String::as_str).collect();
                        self.file.update(|f| {
                            if let Some(f) = f.as_mut().filter(|f| f.report.has_hidden_deps()) {
                                resolve_deps(&mut Rc::make_mut(f).report, &known);
                            }
                        });
                        let bytes = saved.unwrap_or_default();
                        let mut report = parse_structure(&bytes);
                        resolve_deps(&mut report, &sum.saved_known_deps.iter().map(String::as_str).collect());
                        self.saved.set(Some(Rc::new(Loaded { name: "doc.save()".into(), bytes, report })));
                        self.step.set(sum.changes);
                        leptos::task::spawn_local(async move {
                            if let Some((Resp::Ms(ms), _)) = worker::call(None, Req::SavedLoadMs, None).await {
                                self.load_ms.1.set(Some(ms));
                            }
                        });
                    }
                    self.doc.set(Some(doc));
                    self.status.set(None);
                });
            },
            Duration::from_millis(30),
        );
    }
}

/// Settles the dependencies that [`parse_structure`] left in order, given
/// which of them automerge found inside the file's document chunks.
fn resolve_deps(report: &mut FileReport, known: &HashSet<&str>) {
    let bulk = report.first_bulk_chunk();
    report.resolve_deps(|h| bulk.filter(|_| known.contains(h)));
}

/// A single-entry memo for expensive derived data that isn't `Send`.
pub struct Cache<K, V> {
    slot: StoredValue<Option<(K, Rc<V>)>, LocalStorage>,
}

impl<K, V> Clone for Cache<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for Cache<K, V> {}

impl<K: PartialEq + 'static, V: 'static> Cache<K, V> {
    pub fn new() -> Self {
        Self {
            slot: StoredValue::new_local(None),
        }
    }

    pub fn get(&self, key: K, compute: impl FnOnce() -> V) -> Rc<V> {
        if let Some(v) = self
            .slot
            .with_value(|s| s.as_ref().filter(|(k, _)| *k == key).map(|(_, v)| v.clone()))
        {
            return v;
        }
        let v = Rc::new(compute());
        self.slot.set_value(Some((key, v.clone())));
        v
    }
}
