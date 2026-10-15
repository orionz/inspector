use std::collections::{BTreeSet, HashMap};

use automerge::{
    Automerge, ChangeHash, ExpandedChange, LoadOptions, ObjId, ObjType, OnPartialLoad, ReadDoc,
    ScalarValue, Value, ROOT,
};

use crate::chunk::{Chunk, ChunkKind};
use crate::columns::{action_label, truncate};

#[derive(Debug, Clone)]
pub struct ChangeMeta {
    pub hash: ChangeHash,
    pub actor: String,
    pub actor_idx: usize,
    pub author: Option<String>,
    pub seq: u64,
    pub start_op: u64,
    pub max_op: u64,
    pub time: i64,
    pub message: Option<String>,
    /// Indexes into [`DocModel::changes`].
    pub deps: Vec<usize>,
}

impl ChangeMeta {
    pub fn num_ops(&self) -> u64 {
        (self.max_op + 1).saturating_sub(self.start_op)
    }
}

#[derive(Debug, Clone)]
pub struct ActorSummary {
    pub actor: String,
    pub author: Option<String>,
    pub changes: usize,
    pub ops: u64,
}

/// The whole file loaded as an automerge document, with its history in causal order.
pub struct DocModel {
    pub doc: Automerge,
    pub changes: Vec<ChangeMeta>,
    pub actors: Vec<ActorSummary>,
    by_hash: HashMap<ChangeHash, usize>,
    actor_prefix: usize,
    ops_by_obj: std::cell::OnceCell<ObjIndex>,
    history: std::cell::RefCell<Option<(String, std::rc::Rc<History>)>>,
}

#[derive(Default)]
struct ObjIndex {
    rows: HashMap<String, Vec<ObjOp>>,
    raw: HashMap<String, Vec<RawOp>>,
    actors: Vec<String>,
}

/// Counter and the actor's rank among the document's actors, so ids order like automerge's.
type OpKey = (u64, u32);

struct RawOp {
    id: OpKey,
    key: RawKey,
    insert: bool,
    kind: RawKind,
    pred: Vec<OpKey>,
    change: Option<usize>,
}

enum RawKey {
    Map(String),
    Head,
    Elem(OpKey),
}

enum RawKind {
    Put(ScalarValue),
    Make(ObjType),
    Del,
    Inc,
    Mark,
}

/// A text object's characters, live or deleted, in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpan {
    pub text: String,
    pub deleted: bool,
}

/// One op on an object, formatted like the inspector's logical rows.
#[derive(Debug, Clone)]
pub struct ObjOp {
    /// `ctr@abcd`, with the actor's first four hex digits.
    pub id: String,
    pub key: String,
    pub insert: bool,
    /// The action with its value, e.g. `Make Map`, `Insert "cow"` or `Inc 2`.
    pub action: String,
    pub pred: String,
    /// Index into [`DocModel::changes`] of the change holding the op.
    pub change: Option<usize>,
}

fn id4(s: impl ToString) -> String {
    let s = s.to_string();
    match s.split_once('@') {
        Some((ctr, actor)) => format!("{ctr}@{}", &actor[..actor.len().min(4)]),
        None if s == "_root" => "ROOT".into(),
        None => s,
    }
}

pub fn author_label(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) if !s.is_empty() && s.chars().all(|c| !c.is_control()) => s.to_string(),
        _ => hex::encode(bytes),
    }
}

fn unique_prefix_len<'a>(ids: impl Iterator<Item = &'a str> + Clone) -> usize {
    let longest = ids.clone().map(str::len).max().unwrap_or(0);
    (8..longest)
        .find(|&n| {
            let mut seen = std::collections::HashSet::new();
            ids.clone().all(|id| seen.insert(&id[..id.len().min(n)]))
        })
        .unwrap_or(longest.max(8))
}

pub fn short_hex(s: &str) -> &str {
    &s[..s.len().min(8)]
}

#[derive(Debug, Clone)]
pub enum NodeValue {
    Scalar { text: String, kind: &'static str },
    Object { id: ObjId, ty: ObjType, len: usize },
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub value: NodeValue,
    pub conflict: bool,
    /// Index of the change whose op produced this value.
    pub origin: Option<usize>,
    pub op_id: String,
}

#[derive(Debug, Clone)]
pub struct ObjectPage {
    pub entries: Vec<Entry>,
    pub total: usize,
}

fn scalar(s: &ScalarValue) -> (String, &'static str) {
    match s {
        ScalarValue::Str(v) => (format!("{:?}", truncate(v, 200)), "str"),
        ScalarValue::Int(v) => (v.to_string(), "int"),
        ScalarValue::Uint(v) => (v.to_string(), "uint"),
        ScalarValue::F64(v) => (v.to_string(), "f64"),
        ScalarValue::Counter(c) => (format!("{c}"), "counter"),
        ScalarValue::Timestamp(t) => (t.to_string(), "timestamp"),
        ScalarValue::Boolean(b) => (b.to_string(), "bool"),
        ScalarValue::Bytes(b) => (
            format!("0x{}{}", hex::encode(&b[..b.len().min(24)]), if b.len() > 24 { "…" } else { "" }),
            "bytes",
        ),
        ScalarValue::Unknown { type_code, bytes } => {
            (format!("type {type_code}, {} bytes", bytes.len()), "unknown")
        }
        ScalarValue::Null => ("null".into(), "null"),
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_scalar(s: &ScalarValue) -> String {
    match s {
        ScalarValue::Str(v) => json_string(v),
        ScalarValue::Int(v) => v.to_string(),
        ScalarValue::Uint(v) => v.to_string(),
        ScalarValue::F64(v) if v.is_finite() => v.to_string(),
        ScalarValue::F64(_) => "null".into(),
        ScalarValue::Counter(c) => format!("{c}"),
        ScalarValue::Timestamp(t) => t.to_string(),
        ScalarValue::Boolean(b) => b.to_string(),
        ScalarValue::Bytes(b) => json_string(&hex::encode(b)),
        ScalarValue::Unknown { bytes, .. } => json_string(&hex::encode(bytes)),
        ScalarValue::Null => "null".into(),
    }
}

impl DocModel {
    /// Loads every chunk of the file into one document; chunks whose
    /// dependencies are missing are ignored rather than failing the load.
    pub fn load(bytes: &[u8]) -> Result<Self, String> {
        let doc = Automerge::load_with_options(
            bytes,
            LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self::from_doc(doc))
    }

    pub fn from_doc(doc: Automerge) -> Self {
        let metas = doc.get_changes_meta(&[]);
        let by_hash_raw: HashMap<ChangeHash, usize> =
            metas.iter().enumerate().map(|(i, m)| (m.hash, i)).collect();

        // topological order, ties broken by time so the timeline reads naturally
        let n = metas.len();
        let mut indegree = vec![0usize; n];
        let mut children: Vec<Vec<usize>> = vec![vec![]; n];
        for (i, m) in metas.iter().enumerate() {
            for d in &m.deps {
                if let Some(&j) = by_hash_raw.get(d) {
                    indegree[i] += 1;
                    children[j].push(i);
                }
            }
        }
        let mut ready: BTreeSet<(i64, usize)> = (0..n)
            .filter(|&i| indegree[i] == 0)
            .map(|i| (metas[i].timestamp, i))
            .collect();
        let mut order = Vec::with_capacity(n);
        while let Some(first) = ready.pop_first() {
            let i = first.1;
            order.push(i);
            for &c in &children[i] {
                indegree[c] -= 1;
                if indegree[c] == 0 {
                    ready.insert((metas[c].timestamp, c));
                }
            }
        }

        let mut actor_index: HashMap<String, usize> = HashMap::new();
        let mut actors: Vec<ActorSummary> = Vec::new();
        let by_hash: HashMap<ChangeHash, usize> = order
            .iter()
            .enumerate()
            .map(|(pos, &i)| (metas[i].hash, pos))
            .collect();
        let changes: Vec<ChangeMeta> = order
            .iter()
            .map(|&i| {
                let m = &metas[i];
                let actor = m.actor.to_hex_string();
                let author = m.author.as_ref().map(|a| author_label(a.as_bytes()));
                let idx = *actor_index.entry(actor.clone()).or_insert_with(|| {
                    actors.push(ActorSummary {
                        actor: actor.clone(),
                        author: author.clone(),
                        changes: 0,
                        ops: 0,
                    });
                    actors.len() - 1
                });
                let num_ops = (m.max_op + 1).saturating_sub(m.start_op);
                actors[idx].changes += 1;
                actors[idx].ops += num_ops;
                if actors[idx].author.is_none() {
                    actors[idx].author = author.clone();
                }
                ChangeMeta {
                    hash: m.hash,
                    actor,
                    actor_idx: idx,
                    author,
                    seq: m.seq,
                    start_op: m.start_op,
                    max_op: m.max_op,
                    time: m.timestamp,
                    message: m.message.as_ref().map(|s| s.to_string()),
                    deps: m.deps.iter().filter_map(|d| by_hash.get(d).copied()).collect(),
                }
            })
            .collect();
        drop(metas);
        let actor_prefix = unique_prefix_len(actors.iter().map(|a| a.actor.as_str()));
        Self {
            doc,
            changes,
            actors,
            by_hash,
            actor_prefix,
            ops_by_obj: Default::default(),
            history: Default::default(),
        }
    }

    /// Every op whose object is `obj`, in op id order. The first call
    /// decodes every change in the document, so it is slow on large documents.
    pub fn object_ops(&self, obj: &ObjId) -> &[ObjOp] {
        self.index().rows.get(&obj.to_string()).map(Vec::as_slice).unwrap_or_default()
    }

    fn index(&self) -> &ObjIndex {
        self.ops_by_obj.get_or_init(|| {
            use automerge::legacy::{ElementId, Key, OpType};
            let changes = self.doc.get_changes(&[]);
            let decoded: Vec<_> = changes.iter().map(|c| c.decode()).collect();
            let mut actors: Vec<String> = decoded.iter().map(|c| c.actor_id.to_hex_string()).collect();
            actors.sort();
            actors.dedup();
            let rank = |a: &automerge::ActorId| {
                actors.binary_search(&a.to_hex_string()).map_or(u32::MAX, |i| i as u32)
            };
            let mut rows: HashMap<String, Vec<(OpKey, ObjOp)>> = HashMap::new();
            let mut raw: HashMap<String, Vec<RawOp>> = HashMap::new();
            for c in &decoded {
                let actor = c.actor_id.to_hex_string();
                let me = rank(&c.actor_id);
                let change_idx = c.hash.and_then(|h| self.by_hash.get(&h).copied());
                for (i, op) in c.operations.iter().enumerate() {
                    let ctr = c.start_op.get() + i as u64;
                    let action = match &op.action {
                        OpType::Make(t) => action_label(&format!("make{t:?}"), op.insert, None),
                        OpType::Delete => action_label("del", op.insert, None),
                        OpType::Increment(n) => action_label("inc", op.insert, Some(&n.to_string())),
                        OpType::Put(v) => action_label("set", op.insert, Some(&scalar(v).0)),
                        OpType::MarkBegin(m) => format!("Mark {}={}", m.name, scalar(&m.value).0),
                        OpType::MarkEnd(_) => "Mark end".into(),
                    };
                    let row = ObjOp {
                        id: format!("{ctr}@{}", &actor[..actor.len().min(4)]),
                        key: match &op.key {
                            Key::Map(s) => truncate(s, 60),
                            Key::Seq(e) => id4(e),
                        },
                        insert: op.insert,
                        action,
                        pred: op.pred.iter().map(id4).collect::<Vec<_>>().join(", "),
                        change: change_idx,
                    };
                    let obj = op.obj.to_string();
                    rows.entry(obj.clone()).or_default().push(((ctr, me), row));
                    raw.entry(obj).or_default().push(RawOp {
                        id: (ctr, me),
                        key: match &op.key {
                            Key::Map(s) => RawKey::Map(s.to_string()),
                            Key::Seq(ElementId::Head) => RawKey::Head,
                            Key::Seq(ElementId::Id(e)) => RawKey::Elem((e.counter(), rank(e.actor()))),
                        },
                        insert: op.insert,
                        kind: match &op.action {
                            OpType::Make(t) => RawKind::Make((*t).into()),
                            OpType::Delete => RawKind::Del,
                            OpType::Increment(_) => RawKind::Inc,
                            OpType::Put(v) => RawKind::Put(v.clone()),
                            OpType::MarkBegin(_) | OpType::MarkEnd(_) => RawKind::Mark,
                        },
                        pred: op.pred.iter().map(|p| (p.counter(), rank(p.actor()))).collect(),
                        change: change_idx,
                    });
                }
            }
            let rows = rows
                .into_iter()
                .map(|(k, mut v)| {
                    v.sort_by_key(|(id, _)| *id);
                    (k, v.into_iter().map(|(_, r)| r).collect())
                })
                .collect();
            ObjIndex { rows, raw, actors }
        })
    }

    fn history(&self, obj: &ObjId, ops: &[RawOp]) -> std::rc::Rc<History> {
        let key = obj.to_string();
        let mut slot = self.history.borrow_mut();
        match &*slot {
            Some((k, h)) if *k == key => h.clone(),
            _ => {
                let h = std::rc::Rc::new(History::new(ops));
                *slot = Some((key, h.clone()));
                h
            }
        }
    }

    fn op_exid(&self, actors: &[String], id: OpKey) -> ObjId {
        let actor = actors.get(id.1 as usize).map_or("", String::as_str);
        self.doc.import_obj(&format!("{}@{actor}", id.0)).unwrap_or(ROOT)
    }

    /// Like [`page`](Self::page), with the entries deleted by `step`
    /// interleaved where they stood, flagged `true`. Slow on large objects:
    /// the first call decodes every change in the document.
    pub fn page_with_tombstones(&self, obj: &ObjId, step: usize, limit: usize) -> (Vec<(Entry, bool)>, usize) {
        let heads = self.heads_at(step);
        let heads = heads.as_deref();
        let page = self.page(obj, heads, 0, limit);
        let total = page.total;
        let all_shown = page.entries.len() >= total;
        let index = self.index();
        let Some(ops) = index.raw.get(&obj.to_string()) else {
            return (page.entries.into_iter().map(|e| (e, false)).collect(), total);
        };
        let history = self.history(obj, ops);
        let life = Life { step, ops, h: &history };
        let gone = |op: &RawOp| {
            let (key, value) = match &op.kind {
                RawKind::Put(v) => (op.id, Value::Scalar(std::borrow::Cow::Owned(v.clone()))),
                RawKind::Make(t) => (op.id, Value::Object(*t)),
                _ => return None,
            };
            let id = self.op_exid(&index.actors, key);
            let mut e = self.entry(String::new(), value, id, false, heads);
            e.origin = op.change;
            Some(e)
        };
        let mut out = Vec::new();
        if matches!(self.doc.object_type(obj), Ok(ObjType::Map | ObjType::Table)) {
            let mut by_key: std::collections::BTreeMap<&str, Vec<&RawOp>> = Default::default();
            for op in ops.iter().filter(|o| life.exists(o) && life.is_value(o)) {
                if let RawKey::Map(k) = &op.key {
                    by_key.entry(k).or_default().push(op);
                }
            }
            let last_key = page.entries.last().map(|e| e.key.clone());
            let mut dead = by_key
                .into_iter()
                .filter(|(_, ops)| !ops.iter().any(|o| life.alive(o)))
                .filter(|(k, _)| all_shown || last_key.as_deref().is_some_and(|l| *k < l))
                .filter_map(|(k, ops)| {
                    let mut e = gone(ops.iter().max_by_key(|o| o.id)?)?;
                    e.key = k.to_string();
                    Some(e)
                })
                .peekable();
            for e in page.entries {
                while let Some(d) = dead.next_if(|d| d.key < e.key) {
                    out.push((d, true));
                }
                out.push((e, false));
            }
            out.extend(dead.map(|d| (d, true)));
            return (out, total);
        }
        let seq: Vec<_> = life.sequence().collect();
        if seq.iter().filter(|(_, live)| *live).count() != total {
            return (page.entries.into_iter().map(|e| (e, false)).collect(), total);
        }
        let mut live = page.entries.into_iter();
        let mut pending = Vec::new();
        for (elem, is_live) in seq {
            if is_live {
                let Some(e) = live.next() else { break };
                out.append(&mut pending);
                out.push((e, false));
            } else if let Some(mut e) = life.last_value(elem).and_then(gone) {
                e.key = "×".into();
                pending.push((e, true));
            }
        }
        if all_shown {
            out.append(&mut pending);
        }
        (out, total)
    }

    /// A text object's characters at `step` with the deleted ones between
    /// them, up to `limit_chars` live characters; `None` if the ops could not
    /// be replayed to match automerge. Slow on large objects.
    pub fn text_with_tombstones(&self, obj: &ObjId, step: usize, limit_chars: usize) -> Option<Vec<TextSpan>> {
        let heads = self.heads_at(step);
        let expect: String = self.text(obj, heads.as_deref()).chars().take(limit_chars).collect();
        let ops = self.index().raw.get(&obj.to_string())?;
        let history = self.history(obj, ops);
        let life = Life { step, ops, h: &history };
        let mut spans: Vec<TextSpan> = Vec::new();
        let mut live_text = String::new();
        let mut live_chars = 0;
        for (elem, is_live) in life.sequence() {
            if is_live && live_chars >= limit_chars {
                break;
            }
            let value = if is_live { life.live_value(elem) } else { life.last_value(elem) };
            let Some(RawOp { kind: RawKind::Put(ScalarValue::Str(s)), .. }) = value else {
                if is_live {
                    return None;
                }
                continue;
            };
            if is_live {
                live_text.push_str(s);
                live_chars += s.chars().count();
            }
            match spans.last_mut() {
                Some(last) if last.deleted != is_live => last.text.push_str(s),
                _ => spans.push(TextSpan { text: s.to_string(), deleted: !is_live }),
            }
        }
        let live_text: String = live_text.chars().take(limit_chars).collect();
        (live_text == expect).then_some(spans)
    }

    /// The shortest prefix (at least 8 hex digits) that tells this document's actors apart.
    pub fn short_actor<'a>(&self, actor: &'a str) -> &'a str {
        &actor[..actor.len().min(self.actor_prefix)]
    }

    /// Whether the document holds the change with this hex hash.
    pub fn has_change(&self, hex: &str) -> bool {
        hex.parse::<ChangeHash>().is_ok_and(|h| self.by_hash.contains_key(&h))
    }

    pub fn change_index(&self, hash: &ChangeHash) -> Option<usize> {
        self.by_hash.get(hash).copied()
    }

    /// Heads after applying the first `step` changes of the timeline, or
    /// `None` when `step` covers the whole history.
    pub fn heads_at(&self, step: usize) -> Option<Vec<ChangeHash>> {
        if step >= self.changes.len() {
            return None;
        }
        let mut heads: BTreeSet<usize> = BTreeSet::new();
        for (i, c) in self.changes[..step].iter().enumerate() {
            for d in &c.deps {
                heads.remove(d);
            }
            heads.insert(i);
        }
        Some(heads.into_iter().map(|i| self.changes[i].hash).collect())
    }

    fn origin(&self, id: &ObjId) -> Option<usize> {
        self.doc
            .hash_for_opid(id)
            .and_then(|h| self.change_index(&h))
    }

    fn node(&self, value: Value<'_>, id: &ObjId, heads: Option<&[ChangeHash]>) -> NodeValue {
        match value {
            Value::Object(ty) => NodeValue::Object {
                id: id.clone(),
                ty,
                len: match heads {
                    Some(h) => self.doc.length_at(id, h),
                    None => self.doc.length(id),
                },
            },
            Value::Scalar(s) => {
                let (text, kind) = scalar(&s);
                NodeValue::Scalar { text, kind }
            }
        }
    }

    fn entry(&self, key: String, value: Value<'_>, id: ObjId, conflict: bool, heads: Option<&[ChangeHash]>) -> Entry {
        Entry {
            key,
            value: self.node(value, &id, heads),
            conflict,
            origin: self.origin(&id),
            op_id: id.to_string(),
        }
    }

    pub fn root(&self) -> ObjId {
        ROOT
    }

    /// The object and everything below it as indented JSON, cut short once
    /// it passes `limit` bytes; the flag says whether it was cut.
    pub fn json(&self, obj: &ObjId, heads: Option<&[ChangeHash]>, limit: usize) -> (String, bool) {
        let ty = self.doc.object_type(obj).unwrap_or(ObjType::Map);
        let mut out = String::new();
        self.write_json(&mut out, obj, ty, heads, 0, limit);
        let cut = out.len() > limit;
        (out, cut)
    }

    fn write_json(&self, out: &mut String, obj: &ObjId, ty: ObjType, heads: Option<&[ChangeHash]>, depth: usize, limit: usize) {
        if out.len() > limit {
            return;
        }
        let pad = |out: &mut String, d: usize| out.extend(std::iter::repeat_n("  ", d));
        let value = |out: &mut String, v: Value<'_>, id: ObjId, d: usize| match v {
            Value::Object(t) => self.write_json(out, &id, t, heads, d, limit),
            Value::Scalar(s) => out.push_str(&json_scalar(&s)),
        };
        match ty {
            ObjType::Text => out.push_str(&json_string(&self.text(obj, heads))),
            ObjType::Map | ObjType::Table => {
                let items: Vec<_> = match heads {
                    Some(h) => self.doc.map_range_at(obj, .., h).map(|i| (i.key.to_string(), i.id(), i.value.into_value())).collect(),
                    None => self.doc.map_range(obj, ..).map(|i| (i.key.to_string(), i.id(), i.value.into_value())).collect(),
                };
                if items.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                let n = items.len();
                for (k, (key, id, v)) in items.into_iter().enumerate() {
                    if out.len() > limit {
                        return;
                    }
                    pad(out, depth + 1);
                    out.push_str(&json_string(&key));
                    out.push_str(": ");
                    value(out, v, id, depth + 1);
                    out.push_str(if k + 1 < n { ",\n" } else { "\n" });
                }
                pad(out, depth);
                out.push('}');
            }
            ObjType::List => {
                let items: Vec<_> = match heads {
                    Some(h) => self.doc.list_range_at(obj, .., h).map(|i| (i.id(), i.value.into_value())).collect(),
                    None => self.doc.list_range(obj, ..).map(|i| (i.id(), i.value.into_value())).collect(),
                };
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                let n = items.len();
                for (k, (id, v)) in items.into_iter().enumerate() {
                    if out.len() > limit {
                        return;
                    }
                    pad(out, depth + 1);
                    value(out, v, id, depth + 1);
                    out.push_str(if k + 1 < n { ",\n" } else { "\n" });
                }
                pad(out, depth);
                out.push(']');
            }
        }
    }

    pub fn text(&self, obj: &ObjId, heads: Option<&[ChangeHash]>) -> String {
        match heads {
            Some(h) => self.doc.text_at(obj, h),
            None => self.doc.text(obj),
        }
        .unwrap_or_default()
    }

    pub fn marks(&self, obj: &ObjId, heads: Option<&[ChangeHash]>) -> Vec<String> {
        let marks = match heads {
            Some(h) => self.doc.marks_at(obj, h),
            None => self.doc.marks(obj),
        };
        marks
            .unwrap_or_default()
            .iter()
            .map(|m| format!("{} = {} [{}..{})", m.name(), m.value(), m.start, m.end))
            .collect()
    }

    /// A page of an object's children (map entries or list items).
    pub fn page(
        &self,
        obj: &ObjId,
        heads: Option<&[ChangeHash]>,
        start: usize,
        limit: usize,
    ) -> ObjectPage {
        let ty = self.doc.object_type(obj).unwrap_or(ObjType::Map);
        let total = match heads {
            Some(h) => self.doc.length_at(obj, h),
            None => self.doc.length(obj),
        };
        let entries = match ty {
            ObjType::Map | ObjType::Table => {
                let iter = match heads {
                    Some(h) => self.doc.map_range_at(obj, .., h),
                    None => self.doc.map_range(obj, ..),
                };
                iter.skip(start)
                    .take(limit)
                    .map(|item| {
                        let id = item.id();
                        let conflict = item.conflict;
                        self.entry(item.key.to_string(), item.value.into_value(), id, conflict, heads)
                    })
                    .collect()
            }
            ObjType::List | ObjType::Text => {
                let iter = match heads {
                    Some(h) => self.doc.list_range_at(obj, start..start + limit, h),
                    None => self.doc.list_range(obj, start..start + limit),
                };
                iter.map(|item| {
                    let id = item.id();
                    let conflict = item.conflict;
                    self.entry(item.index.to_string(), item.value.into_value(), id, conflict, heads)
                })
                .collect()
            }
        };
        ObjectPage { entries, total }
    }
}

/// What an object's ops did to each other, independent of the version shown.
struct History {
    /// For each op, the earliest change that overwrote or deleted it.
    ended: HashMap<OpKey, usize>,
    /// The value ops of each list element, the inserting op first.
    values: HashMap<OpKey, Vec<usize>>,
    /// Every list element ever inserted, in list order.
    order: Vec<OpKey>,
}

impl History {
    fn new(ops: &[RawOp]) -> Self {
        let mut ended: HashMap<OpKey, usize> = HashMap::new();
        let mut values: HashMap<OpKey, Vec<usize>> = HashMap::new();
        let mut children: HashMap<Option<OpKey>, Vec<(OpKey, bool)>> = HashMap::new();
        for (i, op) in ops.iter().enumerate() {
            if !matches!(op.kind, RawKind::Inc) {
                let at = op.change.unwrap_or(0);
                for p in &op.pred {
                    ended.entry(*p).and_modify(|e| *e = (*e).min(at)).or_insert(at);
                }
            }
            if matches!(op.kind, RawKind::Put(_) | RawKind::Make(_)) {
                match (&op.key, op.insert) {
                    (_, true) => values.entry(op.id).or_default().insert(0, i),
                    (RawKey::Elem(e), false) => values.entry(*e).or_default().push(i),
                    _ => {}
                }
            }
            if op.insert {
                let parent = match op.key {
                    RawKey::Elem(e) => Some(e),
                    _ => None,
                };
                children.entry(parent).or_default().push((op.id, matches!(op.kind, RawKind::Mark)));
            }
        }
        for c in children.values_mut() {
            c.sort_by(|a, b| b.0.cmp(&a.0));
        }
        let mut order = Vec::new();
        let mut stack: Vec<(OpKey, bool)> = children.get(&None).map(|c| c.iter().rev().copied().collect()).unwrap_or_default();
        while let Some((id, mark)) = stack.pop() {
            if !mark {
                order.push(id);
            }
            if let Some(c) = children.get(&Some(id)) {
                stack.extend(c.iter().rev().copied());
            }
        }
        Self { ended, values, order }
    }
}

/// Which of an object's ops exist and are still in effect after the first `step` changes.
struct Life<'a> {
    step: usize,
    ops: &'a [RawOp],
    h: &'a History,
}

impl<'a> Life<'a> {
    fn exists(&self, op: &RawOp) -> bool {
        op.change.unwrap_or(0) < self.step
    }

    fn is_value(&self, op: &RawOp) -> bool {
        matches!(op.kind, RawKind::Put(_) | RawKind::Make(_))
    }

    fn alive(&self, op: &RawOp) -> bool {
        self.exists(op) && self.is_value(op) && self.h.ended.get(&op.id).is_none_or(|&e| e >= self.step)
    }

    fn values(&self, elem: OpKey) -> impl Iterator<Item = &'a RawOp> + '_ {
        self.h.values.get(&elem).into_iter().flatten().map(|&i| &self.ops[i])
    }

    fn live_value(&self, elem: OpKey) -> Option<&'a RawOp> {
        self.values(elem).filter(|o| self.alive(o)).max_by_key(|o| o.id)
    }

    fn last_value(&self, elem: OpKey) -> Option<&'a RawOp> {
        self.values(elem).filter(|o| self.exists(o)).max_by_key(|o| o.id)
    }

    /// Every element inserted by `step`, in list order, with whether it is still there.
    fn sequence(&self) -> impl Iterator<Item = (OpKey, bool)> + '_ {
        self.h.order.iter().filter_map(|&id| {
            let first = self.values(id).next()?;
            self.exists(first).then(|| (id, self.live_value(id).is_some()))
        })
    }
}

/// One op of a decoded change, formatted for display.
#[derive(Debug, Clone)]
pub struct OpRow {
    pub id: String,
    pub action: String,
    pub obj: String,
    pub key: String,
    pub insert: bool,
    pub pred: String,
}

#[derive(Debug, Clone)]
pub struct DecodedChange {
    pub hash: String,
    pub actor: String,
    pub author: Option<String>,
    pub seq: u64,
    pub start_op: u64,
    pub time: i64,
    pub message: Option<String>,
    pub deps: Vec<String>,
    pub ops: Vec<OpRow>,
    pub extra_bytes: usize,
}

fn short_id(s: impl ToString) -> String {
    let s = s.to_string();
    match s.split_once('@') {
        Some((ctr, actor)) => format!("{ctr}@{}", short_hex(actor)),
        None => s,
    }
}

/// Expands a change into its ops for display.
pub fn decode_change(change: &automerge::Change) -> DecodedChange {
    expand(&change.decode())
}

fn expand(c: &ExpandedChange) -> DecodedChange {
    use automerge::legacy::{Key, OpType};
    let ops = c
        .operations
        .iter()
        .enumerate()
        .map(|(i, op)| {
            let action = match &op.action {
                OpType::Make(t) => format!("make {t:?}"),
                OpType::Delete => "del".into(),
                OpType::Increment(n) => format!("inc {n}"),
                OpType::Put(v) => format!("set {}", scalar(v).0),
                OpType::MarkBegin(m) => format!("mark {}={}", m.name, scalar(&m.value).0),
                OpType::MarkEnd(_) => "mark end".into(),
            };
            OpRow {
                id: format!("{}@{}", c.start_op.get() + i as u64, short_hex(&c.actor_id.to_hex_string())),
                action,
                obj: short_id(&op.obj),
                key: match &op.key {
                    Key::Map(s) => truncate(s, 40),
                    Key::Seq(e) => short_id(e),
                },
                insert: op.insert,
                pred: op.pred.iter().map(short_id).collect::<Vec<_>>().join(", "),
            }
        })
        .collect();
    DecodedChange {
        hash: c.hash.map(|h| h.to_string()).unwrap_or_default(),
        actor: c.actor_id.to_hex_string(),
        author: c.author.as_ref().map(|a| author_label(a.as_bytes())),
        seq: c.seq,
        start_op: c.start_op.get(),
        time: c.time,
        message: c.message.clone(),
        deps: c.deps.iter().map(|d| d.to_string()).collect(),
        ops,
        extra_bytes: c.extra_bytes.len(),
    }
}

/// The changes stored in one chunk, as parsed by automerge itself.
pub fn chunk_changes(file: &[u8], chunk: &Chunk) -> Result<Vec<automerge::Change>, String> {
    let bytes = &file[chunk.range.clone()];
    let changes = match chunk.kind {
        ChunkKind::Document => Automerge::load(bytes)
            .map_err(|e| e.to_string())?
            .get_changes(&[]),
        ChunkKind::Change | ChunkKind::Compressed => {
            vec![automerge::Change::from_bytes(bytes.to_vec()).map_err(|e| e.to_string())?]
        }
        ChunkKind::Bundle => automerge::Bundle::try_from(bytes)
            .map_err(|e| e.to_string())?
            .to_changes()
            .map_err(|e| e.to_string())?,
        ChunkKind::BundleV0 => {
            let mut doc = Automerge::new();
            doc.load_incremental(bytes).map_err(|e| e.to_string())?;
            doc.get_changes(&[])
        }
        ChunkKind::Unknown(_) => return Err("unknown chunk type".into()),
    };
    Ok(changes)
}

/// Formats an automerge timestamp as UTC. Values small enough to be seconds
/// (older JS automerge) are treated as seconds, the rest as milliseconds.
pub fn format_time(t: i64) -> String {
    if t == 0 {
        return "—".into();
    }
    let secs = if t.abs() < 100_000_000_000 { t } else { t.div_euclid(1000) };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil-from-days, Howard Hinnant's algorithm
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_time() {
        assert_eq!(super::format_time(1_767_225_600_000), "2026-01-01 00:00:00");
        assert_eq!(super::format_time(1_767_225_661), "2026-01-01 00:01:01");
    }
}
