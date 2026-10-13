use std::rc::Rc;
use std::time::Duration;

use am_inspect::{parse_file, DocModel, FileReport};
use automerge::ChangeHash;
use leptos::prelude::*;

pub struct Loaded {
    pub name: String,
    pub bytes: Vec<u8>,
    pub report: FileReport,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Storage,
    Document,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GroupView {
    Columns,
    Rows,
    Changes,
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
    pub model: Local<Option<Rc<Result<DocModel, String>>>>,
    pub status: RwSignal<Option<String>>,
    pub tab: RwSignal<Tab>,
    /// The chunk whose detail panels are open; `None` shows the chunk stack.
    pub chunk: RwSignal<Option<usize>>,
    pub column: RwSignal<Option<ColumnRef>>,
    pub group_view: RwSignal<(GroupView, GroupView)>,
    pub step: RwSignal<usize>,
    pub heads: RwSignal<Option<Vec<ChangeHash>>>,
}

impl AppState {
    pub fn new() -> Self {
        let state = Self {
            file: RwSignal::new_local(None),
            model: RwSignal::new_local(None),
            status: RwSignal::new(None),
            tab: RwSignal::new(Tab::Storage),
            chunk: RwSignal::new(None),
            column: RwSignal::new(None),
            group_view: RwSignal::new((GroupView::Columns, GroupView::Columns)),
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
                self.group_view
                    .set((GroupView::Columns, GroupView::Columns));
                self.model.set(None);
                self.file.set(Some(Rc::new(Loaded { name, bytes, report })));
                self.status.set(Some("Loading document history…".into()));
                set_timeout(
                    move || {
                        let model = self
                            .file
                            .with_untracked(|f| f.as_ref().map(|f| DocModel::load(&f.bytes)));
                        if let Some(model) = model {
                            let n = model.as_ref().map(|m| m.changes.len()).unwrap_or(0);
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
