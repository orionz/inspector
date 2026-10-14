//! The automerge document lives in a Web Worker running this same wasm, so
//! loading and querying it never blocks the page. The page sends [`Req`]s and
//! gets [`Resp`]s back as JSON; requests sharing a key are coalesced so only
//! the newest one waits behind the one in flight.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use am_inspect::doc::{DocModel, NodeValue};
use am_inspect::{format_time, parse_structure};
use automerge::{Automerge, ChangeHash, LoadOptions, ObjType, OnPartialLoad, ROOT};
use futures_channel::oneshot;
use js_sys::{Object, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjKind {
    Map,
    List,
    Text,
    Table,
}

impl From<ObjType> for ObjKind {
    fn from(t: ObjType) -> Self {
        match t {
            ObjType::Map => ObjKind::Map,
            ObjType::List => ObjKind::List,
            ObjType::Text => ObjKind::Text,
            ObjType::Table => ObjKind::Table,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Value {
    Scalar { text: String, kind: String },
    Object { id: String, kind: ObjKind, len: usize },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Entry {
    pub key: String,
    /// The op that set the value, which tells list elements apart across versions.
    pub id: String,
    pub value: Value,
    pub conflict: bool,
    /// Which change set the value, for a tooltip.
    pub origin: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Op {
    pub id: String,
    pub key: String,
    pub action: String,
    pub pred: String,
    /// Position of the op's change in history order.
    pub change: Option<usize>,
    pub origin: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Summary {
    pub changes: usize,
    pub actors: usize,
    pub ops: u64,
    pub load_ms: f64,
    /// The requested hidden dependencies that the document holds.
    pub known_deps: Vec<String>,
    /// The same for the hidden dependencies of `doc.save()`'s output.
    pub saved_known_deps: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Req {
    /// Loads the file sent alongside; answered with [`Resp::Loaded`] and the
    /// bytes `doc.save()` writes.
    Load { hidden_deps: Vec<String> },
    /// How long automerge takes to load the saved bytes.
    SavedLoadMs,
    Page { obj: String, step: usize, limit: usize },
    Text { obj: String, step: usize, limit_chars: usize },
    Json { obj: String, step: usize, limit: usize },
    Ops { obj: String, start: usize, limit: usize },
    ChangeTime { index: usize },
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub enum Resp {
    Loaded(Result<Summary, String>),
    Ms(f64),
    Page { entries: Vec<Entry>, total: usize },
    Text { text: String, chars: usize, marks: Vec<String> },
    Json { json: String, cut: bool },
    Ops { ops: Vec<Op>, total: usize },
    Time(String),
    Error(String),
}

// ---------------------------------------------------------------------------
// Worker side

#[derive(Default)]
struct Held {
    model: Option<DocModel>,
    saved: Vec<u8>,
    heads: Option<(usize, Option<Vec<ChangeHash>>)>,
}

thread_local! {
    static HELD: RefCell<Held> = RefCell::default();
}

/// Whether this wasm instance is running inside a worker.
pub fn in_worker() -> bool {
    js_sys::global().has_type::<web_sys::DedicatedWorkerGlobalScope>()
}

/// Answers requests from the page until the worker is terminated.
pub fn serve() {
    let scope: web_sys::DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let reply_to = scope.clone();
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| {
        let data = ev.data();
        let id = Reflect::get(&data, &"id".into()).unwrap_or(JsValue::NULL);
        let bytes = Reflect::get(&data, &"bytes".into())
            .ok()
            .filter(|b| !b.is_undefined())
            .map(|b| Uint8Array::new(&b).to_vec());
        let req = Reflect::get(&data, &"json".into())
            .ok()
            .and_then(|j| j.as_string())
            .and_then(|j| serde_json::from_str::<Req>(&j).ok());
        let (resp, out) = match req {
            Some(req) => HELD.with_borrow_mut(|held| handle(held, req, bytes)),
            None => (Resp::Error("unreadable request".into()), None),
        };
        let _ = reply_to.post_message(&message(id, &resp, out.as_deref()));
    });
    scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
    let ready = Object::new();
    let _ = Reflect::set(&ready, &"ready".into(), &JsValue::TRUE);
    let _ = scope.post_message(&ready);
}

fn message(id: JsValue, body: &impl Serialize, bytes: Option<&[u8]>) -> Object {
    let msg = Object::new();
    let _ = Reflect::set(&msg, &"id".into(), &id);
    let json = serde_json::to_string(body).unwrap_or_default();
    let _ = Reflect::set(&msg, &"json".into(), &json.into());
    if let Some(b) = bytes {
        let _ = Reflect::set(&msg, &"bytes".into(), &Uint8Array::from(b).into());
    }
    msg
}

fn now() -> f64 {
    js_sys::Date::now()
}

fn handle(held: &mut Held, req: Req, bytes: Option<Vec<u8>>) -> (Resp, Option<Vec<u8>>) {
    if let Req::Load { hidden_deps } = req {
        return load(held, &bytes.unwrap_or_default(), hidden_deps);
    }
    if let Req::SavedLoadMs = req {
        let start = now();
        let _ = Automerge::load(&held.saved);
        return (Resp::Ms(now() - start), None);
    }
    let Some(m) = held.model.as_ref() else {
        return (Resp::Error("no document loaded".into()), None);
    };
    let obj_of = |s: &str| m.doc.import_obj(s).unwrap_or(ROOT);
    let mut heads_at = |step: usize| -> Option<Vec<ChangeHash>> {
        match &held.heads {
            Some((s, h)) if *s == step => h.clone(),
            _ => {
                let h = m.heads_at(step);
                held.heads = Some((step, h.clone()));
                h
            }
        }
    };
    let resp = match req {
        Req::Page { obj, step, limit } => {
            let heads = heads_at(step);
            let page = m.page(&obj_of(&obj), heads.as_deref(), 0, limit);
            let entries = page
                .entries
                .into_iter()
                .map(|e| Entry {
                    origin: origin_label(m, e.origin, &e.op_id),
                    id: e.op_id.clone(),
                    value: match e.value {
                        NodeValue::Scalar { text, kind } => Value::Scalar { text, kind: kind.into() },
                        NodeValue::Object { id, ty, len } => Value::Object { id: id.to_string(), kind: ty.into(), len },
                    },
                    key: e.key,
                    conflict: e.conflict,
                })
                .collect();
            Resp::Page { entries, total: page.total }
        }
        Req::Text { obj, step, limit_chars } => {
            let heads = heads_at(step);
            let obj = obj_of(&obj);
            let text = m.text(&obj, heads.as_deref());
            let chars = text.chars().count();
            Resp::Text {
                text: text.chars().take(limit_chars).collect(),
                chars,
                marks: m.marks(&obj, heads.as_deref()),
            }
        }
        Req::Json { obj, step, limit } => {
            let heads = heads_at(step);
            let (json, cut) = m.json(&obj_of(&obj), heads.as_deref(), limit);
            Resp::Json { json, cut }
        }
        Req::Ops { obj, start, limit } => {
            let all = m.object_ops(&obj_of(&obj));
            let ops = all
                .iter()
                .skip(start)
                .take(limit)
                .map(|op| Op {
                    id: op.id.clone(),
                    key: op.key.clone(),
                    action: op.action.clone(),
                    pred: op.pred.clone(),
                    change: op.change,
                    origin: origin_label(m, op.change, &op.id),
                })
                .collect();
            Resp::Ops { ops, total: all.len() }
        }
        Req::ChangeTime { index } => Resp::Time(m.changes.get(index).map(|c| format_time(c.time)).unwrap_or_default()),
        Req::Load { .. } | Req::SavedLoadMs => unreachable!(),
    };
    (resp, None)
}

fn load(held: &mut Held, bytes: &[u8], hidden_deps: Vec<String>) -> (Resp, Option<Vec<u8>>) {
    *held = Held::default();
    let start = now();
    let doc = Automerge::load_with_options(bytes, LoadOptions::new().on_partial_load(OnPartialLoad::Ignore));
    let load_ms = now() - start;
    let m = match doc {
        Ok(doc) => DocModel::from_doc(doc),
        Err(e) => return (Resp::Loaded(Err(e.to_string())), None),
    };
    let saved = m.doc.save();
    let known = |deps: Vec<String>| deps.into_iter().filter(|d| m.has_change(d)).collect();
    let summary = Summary {
        changes: m.changes.len(),
        actors: m.actors.len(),
        ops: m.changes.iter().map(|c| c.num_ops()).sum(),
        load_ms,
        known_deps: known(hidden_deps),
        saved_known_deps: known(parse_structure(&saved).hidden_deps()),
    };
    held.model = Some(m);
    held.saved = saved.clone();
    (Resp::Loaded(Ok(summary)), Some(saved))
}

fn origin_label(m: &DocModel, origin: Option<usize>, op: &str) -> String {
    let Some((i, c)) = origin.and_then(|i| m.changes.get(i).map(|c| (i, c))) else {
        return format!("op {op}");
    };
    let who = c.author.clone().unwrap_or_else(|| m.short_actor(&c.actor).to_string());
    format!(
        "set in change #{} ({}@{}) by {who} at {}{}\nop {op}",
        i + 1,
        c.seq,
        m.short_actor(&c.actor),
        format_time(c.time),
        c.message.as_ref().map(|m| format!("\n“{m}”")).unwrap_or_default()
    )
}

// ---------------------------------------------------------------------------
// Page side

type Reply = (Resp, Option<Vec<u8>>);

struct Queued {
    req: Req,
    bytes: Option<Vec<u8>>,
    tx: oneshot::Sender<Reply>,
}

#[derive(Default)]
struct Slot {
    busy: bool,
    queued: Option<Queued>,
}

struct Client {
    worker: web_sys::Worker,
    ready: bool,
    unsent: Vec<Object>,
    next_id: u32,
    pending: HashMap<u32, (Option<String>, oneshot::Sender<Reply>)>,
    slots: HashMap<String, Slot>,
}

thread_local! {
    static CLIENT: Rc<RefCell<Option<Client>>> = Rc::new(RefCell::new(None));
}

/// Starts the worker; `url` is the worker script.
pub fn start(url: &str) {
    let opts = web_sys::WorkerOptions::new();
    opts.set_type(web_sys::WorkerType::Module);
    let worker = web_sys::Worker::new_with_options(url, &opts).expect("worker");
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(on_reply);
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
    CLIENT.with(|c| {
        *c.borrow_mut() = Some(Client {
            worker,
            ready: false,
            unsent: Vec::new(),
            next_id: 0,
            pending: HashMap::new(),
            slots: HashMap::new(),
        })
    });
}

fn on_reply(ev: web_sys::MessageEvent) {
    let data = ev.data();
    CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        let Some(c) = c.as_mut() else { return };
        if Reflect::get(&data, &"ready".into()).ok().is_some_and(|r| r.is_truthy()) {
            c.ready = true;
            for msg in c.unsent.drain(..) {
                let _ = c.worker.post_message(&msg);
            }
            return;
        }
        let id = Reflect::get(&data, &"id".into()).ok().and_then(|v| v.as_f64()).unwrap_or(-1.0) as u32;
        let resp = Reflect::get(&data, &"json".into())
            .ok()
            .and_then(|j| j.as_string())
            .and_then(|j| serde_json::from_str::<Resp>(&j).ok())
            .unwrap_or_else(|| Resp::Error("unreadable reply".into()));
        let bytes = Reflect::get(&data, &"bytes".into())
            .ok()
            .filter(|b| !b.is_undefined())
            .map(|b| Uint8Array::new(&b).to_vec());
        let Some((key, tx)) = c.pending.remove(&id) else { return };
        let _ = tx.send((resp, bytes));
        if let Some(key) = key {
            let next = c.slots.get_mut(&key).and_then(|s| {
                s.busy = false;
                s.queued.take()
            });
            if let Some(q) = next {
                c.send(Some(key), q);
            }
        }
    });
}

impl Client {
    fn send(&mut self, key: Option<String>, q: Queued) {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(k) = &key {
            self.slots.entry(k.clone()).or_default().busy = true;
        }
        self.pending.insert(id, (key, q.tx));
        let msg = message(JsValue::from(id), &q.req, q.bytes.as_deref());
        if self.ready {
            let _ = self.worker.post_message(&msg);
        } else {
            self.unsent.push(msg);
        }
    }
}

/// Sends `req` to the worker. With a `key`, a request made while another
/// with the same key is in flight waits for it, and is dropped (resolving to
/// `None`) if a newer one with that key arrives first.
pub async fn call(key: Option<String>, req: Req, bytes: Option<Vec<u8>>) -> Option<Reply> {
    let (tx, rx) = oneshot::channel();
    CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        let c = c.as_mut().expect("worker not started");
        let q = Queued { req, bytes, tx };
        match &key {
            Some(k) if c.slots.get(k).is_some_and(|s| s.busy) => {
                c.slots.get_mut(k).unwrap().queued = Some(q);
            }
            _ => c.send(key, q),
        }
    });
    rx.await.ok()
}

/// Re-asks the worker whenever `req` changes (`None` asks nothing). Gives the
/// latest answer, kept while a newer one is on its way, and whether one is.
pub fn use_query(
    key: String,
    req: impl Fn() -> Option<Req> + 'static,
) -> (leptos::prelude::RwSignal<Option<Resp>>, leptos::prelude::RwSignal<bool>) {
    use leptos::prelude::*;
    let data = RwSignal::new(None::<Resp>);
    let busy = RwSignal::new(false);
    Effect::new(move |_| {
        let Some(r) = req() else {
            busy.set(false);
            return;
        };
        busy.set(true);
        let key = key.clone();
        leptos::task::spawn_local(async move {
            if let Some((resp, _)) = call(Some(key), r, None).await {
                data.try_set(Some(resp));
                busy.try_set(false);
            }
        });
    });
    (data, busy)
}
