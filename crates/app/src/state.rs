use std::rc::Rc;
use std::time::Duration;

use am_inspect::{parse_file, DocModel, FileReport};
use automerge::{Automerge, ChangeHash, LoadOptions, OnPartialLoad};
use leptos::prelude::*;

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
    pub model: Local<Option<Rc<Result<DocModel, String>>>>,
    pub status: RwSignal<Option<String>>,
    pub tab: RwSignal<Tab>,
    /// The chunk whose detail panels are open; `None` shows the chunk stack.
    pub chunk: RwSignal<Option<usize>>,
    pub part: RwSignal<Option<Part>>,
    pub column: RwSignal<Option<ColumnRef>>,
    pub group_view: RwSignal<(GroupView, GroupView)>,
    pub step: RwSignal<usize>,
    pub heads: RwSignal<Option<Vec<ChangeHash>>>,
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
            model: RwSignal::new_local(None),
            status: RwSignal::new(None),
            tab: RwSignal::new(Tab::Load),
            chunk: RwSignal::new(None),
            part: RwSignal::new(None),
            column: RwSignal::new(None),
            group_view: RwSignal::new((GroupView::Physical, GroupView::Physical)),
            step: RwSignal::new(0),
            heads: RwSignal::new(None),
        };
        Effect::new(move |_| {
            let step = state.step.get();
            let heads = state.model.with(|m| match m.as_deref() {
                Some(Ok(m)) => m.heads_at(step),
                _ => None,
            });
            state.heads.set(heads);
        });
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
        self.model.set(None);
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
                let report = parse_file(&bytes);
                self.column.set(None);
                self.chunk.set(None);
                self.part.set(None);
                self.saved_sel.0.set(None);
                self.saved_sel.1.set(None);
                self.saved_sel.2.set(None);
                self.group_view
                    .set((GroupView::Physical, GroupView::Physical));
                self.model.set(None);
                self.saved.set(None);
                self.load_ms.0.set(None);
                self.load_ms.1.set(None);
                self.file.set(Some(Rc::new(Loaded { name, bytes, report })));
                self.status.set(Some("Loading document history…".into()));
                set_timeout(
                    move || {
                        let model = self.file.with_untracked(|f| {
                            let f = f.as_ref()?;
                            let (doc, ms) = timed(|| {
                                Automerge::load_with_options(
                                    &f.bytes,
                                    LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
                                )
                            });
                            self.load_ms.0.set(Some(ms));
                            Some(doc.map(DocModel::from_doc).map_err(|e| e.to_string()))
                        });
                        if let Some(model) = model {
                            let n = model.as_ref().map(|m| m.changes.len()).unwrap_or(0);
                            let saved = model.as_ref().ok().map(|m| {
                                let bytes = m.doc.save();
                                let report = parse_file(&bytes);
                                let (_, ms) = timed(|| Automerge::load(&bytes));
                                self.load_ms.1.set(Some(ms));
                                Rc::new(Loaded { name: "doc.save()".into(), bytes, report })
                            });
                            self.saved.set(saved);
                            self.model.set(Some(Rc::new(model)));
                            self.step.set(n);
                        }
                        self.status.set(None);
                    },
                    Duration::from_millis(30),
                );
            },
            Duration::from_millis(30),
        );
    }
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let start = js_sys::Date::now();
    let out = f();
    (out, js_sys::Date::now() - start)
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
