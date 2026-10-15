use std::collections::HashMap;
use std::ops::Range;

use sha2::{Digest, Sha256};

use crate::columns::{build_columns, inflate, parse_column_meta, ColContext, ColumnGroup};
use crate::leb::{write_uleb, ParseError, Reader, Result};

pub const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkKind {
    Document,
    Change,
    Compressed,
    BundleV0,
    Bundle,
    Unknown(u8),
}

impl ChunkKind {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Document,
            1 => Self::Change,
            2 => Self::Compressed,
            3 => Self::BundleV0,
            4 => Self::Bundle,
            x => Self::Unknown(x),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Document => "Document".into(),
            Self::Change => "Change".into(),
            Self::Compressed => "Compressed change".into(),
            Self::BundleV0 => "Bundle v0".into(),
            Self::Bundle => "Bundle".into(),
            Self::Unknown(b) => format!("Unknown ({b})"),
        }
    }

    /// Stable short key, used for CSS classes and legends.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Document => "doc",
            Self::Change => "change",
            Self::Compressed => "compressed",
            Self::BundleV0 => "bundle-v0",
            Self::Bundle => "bundle",
            Self::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Header,
    Metadata,
    ColumnMeta,
    ChangeColumns,
    OpColumns,
    Suffix,
    Compressed,
}

impl SectionKind {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Metadata => "metadata",
            Self::ColumnMeta => "colmeta",
            Self::ChangeColumns => "changecols",
            Self::OpColumns => "opcols",
            Self::Suffix => "suffix",
            Self::Compressed => "deflated",
        }
    }
}

/// A labelled byte range of the file.
#[derive(Debug, Clone)]
pub struct Section {
    pub label: String,
    pub kind: SectionKind,
    pub range: Range<usize>,
}

impl Section {
    pub fn len(&self) -> usize {
        self.range.len()
    }

    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

/// Header-level facts about a chunk, whichever kind it is.
#[derive(Debug, Clone, Default)]
pub struct ChunkFacts {
    pub actors: Vec<String>,
    pub heads: Vec<String>,
    pub deps: Vec<String>,
    /// Change chunks only.
    pub change: Option<ChangeHeader>,
    /// Bundle prefix entries, as `(label, count)`.
    pub bundle_prefix: Vec<(&'static str, usize)>,
    pub extra_bytes: usize,
    pub head_indexes: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct ChangeHeader {
    pub actor: String,
    pub seq: u64,
    pub start_op: u64,
    pub time: i64,
    pub message: Option<String>,
    pub other_actors: Vec<String>,
}

/// Where a change chunk's dependencies sit relative to it in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DepOrder {
    /// Every dependency appears earlier in the file (or there are none).
    #[default]
    InOrder,
    /// Some dependency only appears later in the file.
    Late,
    /// Some dependency appears nowhere in the file, so the change can't apply.
    Missing,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub index: usize,
    pub kind: ChunkKind,
    pub range: Range<usize>,
    pub header_len: usize,
    pub checksum: [u8; 4],
    pub hash: [u8; 32],
    pub checksum_ok: bool,
    pub sections: Vec<Section>,
    pub groups: Vec<ColumnGroup>,
    pub facts: ChunkFacts,
    /// Bytes of the inflated body, for compressed chunks.
    pub inflated_len: Option<usize>,
    pub error: Option<String>,
    /// Change and compressed change chunks only.
    pub dep_order: DepOrder,
    /// Set by whoever supplied the file: the chunk only repeats changes another chunk holds.
    pub redundant: bool,
}

impl Chunk {
    pub fn len(&self) -> usize {
        self.range.len()
    }

    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    pub fn hash_hex(&self) -> String {
        hex::encode(self.hash)
    }

    pub fn group(&self, ops: bool) -> Option<&ColumnGroup> {
        self.groups.iter().find(|g| g.context.is_ops() == ops)
    }

    pub fn num_ops(&self) -> usize {
        self.group(true).map(|g| g.rows).unwrap_or(0)
    }

    pub fn num_changes(&self) -> usize {
        match self.kind {
            ChunkKind::Change | ChunkKind::Compressed => 1,
            _ => self.group(false).map(|g| g.rows).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileReport {
    pub len: usize,
    pub chunks: Vec<Chunk>,
    /// Bytes that couldn't be read as a chunk, starting at this offset.
    pub trailing: Option<(usize, String)>,
}

pub fn chunk_hash(kind: u8, data: &[u8]) -> [u8; 32] {
    let mut header = vec![kind];
    write_uleb(&mut header, data.len() as u64);
    let mut h = Sha256::new();
    h.update(&header);
    h.update(data);
    h.finalize().into()
}

/// Splits a file into chunks and parses each one's structure. Never fails:
/// unreadable data is reported in `trailing` or a chunk's `error`.
/// Splits a file into chunks and parses each one. Working out where a
/// change's dependencies sit can mean a full automerge parse of the
/// document chunks; [`parse_structure`] skips that.
pub fn parse_file(bytes: &[u8]) -> FileReport {
    let mut report = parse_structure(bytes);
    let inside = report.holders(bytes);
    report.resolve_deps(|h| inside.get(h).copied());
    report.mark_redundant(|h| inside.get(h).copied());
    report
}

/// Like [`parse_file`], but a change depending on a change stored inside a
/// document chunk (rather than at its heads) is marked in order until
/// [`FileReport::resolve_deps`] says otherwise.
pub fn parse_structure(bytes: &[u8]) -> FileReport {
    let mut chunks = Vec::new();
    let mut pos = 0;
    let mut trailing = None;
    while pos < bytes.len() {
        match parse_chunk(bytes, pos, chunks.len()) {
            Ok(chunk) => {
                pos = chunk.range.end;
                chunks.push(chunk);
            }
            Err(e) => {
                trailing = Some((pos, e.to_string()));
                break;
            }
        }
    }
    let mut report = FileReport {
        len: bytes.len(),
        chunks,
        trailing,
    };
    report.resolve_deps(|_| Some(0));
    report
}

fn is_change(c: &Chunk) -> bool {
    matches!(c.kind, ChunkKind::Change | ChunkKind::Compressed) && c.error.is_none()
}

fn has_deps(c: &Chunk) -> bool {
    (is_change(c) || c.kind == ChunkKind::Bundle) && c.error.is_none()
}

/// The hashes that say what a chunk delivers: a change chunk's own, a bundle's heads.
fn delivers(c: &Chunk) -> Vec<String> {
    match c.kind {
        _ if is_change(c) => vec![c.hash_hex()],
        ChunkKind::Bundle => c.facts.heads.clone(),
        _ => vec![],
    }
}

impl FileReport {
    /// Where each change hash first appears, from change chunks and the
    /// heads of other chunks.
    fn visible_hashes(&self) -> HashMap<String, usize> {
        let mut first_seen: HashMap<String, usize> = HashMap::new();
        for c in &self.chunks {
            if is_change(c) {
                first_seen.entry(c.hash_hex()).or_insert(c.index);
            } else {
                for h in &c.facts.heads {
                    first_seen.entry(h.clone()).or_insert(c.index);
                }
            }
        }
        first_seen
    }

    /// Whether some change depends on a hash that only a full parse of the
    /// document chunks could find.
    pub fn has_hidden_deps(&self) -> bool {
        !self.hidden_deps().is_empty()
    }

    /// The dependencies that aren't a change chunk or another chunk's head.
    pub fn hidden_deps(&self) -> Vec<String> {
        let seen = self.visible_hashes();
        let mut out: Vec<String> = self
            .chunks
            .iter()
            .filter(|c| has_deps(c))
            .flat_map(|c| c.facts.deps.iter())
            .filter(|d| !seen.contains_key(*d))
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Recomputes each change's [`DepOrder`]; `inside` gives the chunk
    /// holding a dependency that isn't a change chunk or a chunk's head, or
    /// `None` when it's nowhere in the file.
    pub fn resolve_deps(&mut self, inside: impl Fn(&str) -> Option<usize>) {
        let seen = self.visible_hashes();
        for c in self.chunks.iter_mut().filter(|c| has_deps(c)) {
            c.dep_order = c
                .facts
                .deps
                .iter()
                .map(|d| match seen.get(d).copied().or_else(|| inside(d)) {
                    None => DepOrder::Missing,
                    Some(j) if j > c.index => DepOrder::Late,
                    Some(_) => DepOrder::InOrder,
                })
                .max()
                .unwrap_or_default();
        }
    }

    /// For each change stored in a chunk other than a change chunk, the
    /// first such chunk. Runs automerge over every one of those chunks.
    pub fn holders(&self, bytes: &[u8]) -> HashMap<String, usize> {
        let mut inside: HashMap<String, usize> = HashMap::new();
        for c in self.chunks.iter().filter(|c| !is_change(c) && c.error.is_none()) {
            for change in crate::doc::chunk_changes(bytes, c).unwrap_or_default() {
                inside.entry(change.hash().to_string()).or_insert(c.index);
            }
        }
        inside
    }

    /// Every hash [`Self::mark_redundant`] and [`Self::resolve_deps`] may ask about.
    pub fn probe_hashes(&self) -> Vec<String> {
        let mut out = self.hidden_deps();
        out.extend(self.chunks.iter().filter(|c| has_deps(c)).flat_map(delivers));
        out.sort();
        out.dedup();
        out
    }

    /// Flags the change and bundle chunks that deliver nothing new:
    /// `holder` gives the first chunk other than a change chunk holding a change.
    pub fn mark_redundant(&mut self, holder: impl Fn(&str) -> Option<usize>) {
        let seen = self.visible_hashes();
        for c in self.chunks.iter_mut().filter(|c| has_deps(c)) {
            let earlier = |h: &String| {
                let dup = is_change(c) && seen.get(h).is_some_and(|&i| i < c.index);
                dup || holder(h).is_some_and(|i| i < c.index)
            };
            let hashes = delivers(c);
            if !hashes.is_empty() && hashes.iter().all(earlier) {
                c.redundant = true;
            }
        }
    }

    /// The first chunk that isn't a change chunk, where changes found only by
    /// loading the whole file must be stored.
    pub fn first_bulk_chunk(&self) -> Option<usize> {
        self.chunks.iter().find(|c| !is_change(c)).map(|c| c.index)
    }
}

fn hex_actor(b: &[u8]) -> String {
    hex::encode(b)
}

fn parse_chunk(file: &[u8], start: usize, index: usize) -> Result<Chunk> {
    let mut r = Reader::new(&file[start..], start);
    if r.take(4)? != MAGIC {
        return r.err("missing magic bytes");
    }
    let checksum: [u8; 4] = r.take(4)?.try_into().unwrap();
    let type_byte = r.byte()?;
    let len = r.uleb()? as usize;
    let header_len = r.pos() - start;
    let body_start = r.pos();
    let body = r.take(len)?;
    let kind = ChunkKind::from_byte(type_byte);
    let hash = chunk_hash(type_byte, body);
    let mut chunk = Chunk {
        index,
        kind,
        range: start..body_start + len,
        header_len,
        checksum,
        hash,
        checksum_ok: hash[..4] == checksum,
        sections: vec![Section {
            label: "chunk header".into(),
            kind: SectionKind::Header,
            range: start..body_start,
        }],
        groups: vec![],
        facts: ChunkFacts::default(),
        inflated_len: None,
        error: None,
        dep_order: DepOrder::InOrder,
        redundant: false,
    };
    let result = match kind {
        ChunkKind::Document => parse_document(&mut chunk, body, body_start),
        ChunkKind::Change => parse_change(&mut chunk, body, Some(body_start)),
        ChunkKind::Compressed => parse_compressed(&mut chunk, body, body_start),
        ChunkKind::BundleV0 => {
            parse_column_section(&mut chunk, body, body_start, ColContext::BundleV0Changes)
        }
        ChunkKind::Bundle => parse_bundle(&mut chunk, body, body_start),
        ChunkKind::Unknown(_) => Ok(()),
    };
    if let Err(e) = result {
        chunk.error = Some(e.to_string());
    }
    Ok(chunk)
}

fn section(chunk: &mut Chunk, label: impl Into<String>, kind: SectionKind, range: Range<usize>) {
    if !range.is_empty() {
        chunk.sections.push(Section {
            label: label.into(),
            kind,
            range,
        });
    }
}

fn read_actors(r: &mut Reader<'_>) -> Result<Vec<String>> {
    let n = r.count(1)?;
    (0..n).map(|_| Ok(hex_actor(r.len_prefixed()?))).collect()
}

fn read_hashes(r: &mut Reader<'_>) -> Result<Vec<String>> {
    let n = r.count(32)?;
    (0..n).map(|_| Ok(hex::encode(r.hash()?))).collect()
}

fn parse_document(chunk: &mut Chunk, body: &[u8], base: usize) -> Result<()> {
    let mut r = Reader::new(body, base);
    let s = r.pos();
    chunk.facts.actors = read_actors(&mut r)?;
    section(chunk, "actors", SectionKind::Metadata, s..r.pos());
    let s = r.pos();
    chunk.facts.heads = read_hashes(&mut r)?;
    section(chunk, "heads", SectionKind::Metadata, s..r.pos());
    let s = r.pos();
    let change_meta = parse_column_meta(&mut r)?;
    let op_meta = parse_column_meta(&mut r)?;
    let meta_end = r.pos();
    section(chunk, "column metadata", SectionKind::ColumnMeta, s..meta_end);

    let change_len: usize = change_meta.iter().map(|(_, l)| l).sum();
    let op_len: usize = op_meta.iter().map(|(_, l)| l).sum();
    let s = r.pos();
    let change_data = r.take(change_len)?;
    section(chunk, "change columns", SectionKind::ChangeColumns, s..r.pos());
    let op_start = r.pos();
    let op_data = r.take(op_len)?;
    section(chunk, "op columns", SectionKind::OpColumns, op_start..r.pos());

    let s = r.pos();
    while !r.is_empty() {
        chunk.facts.head_indexes.push(r.uleb()?);
    }
    section(chunk, "head indexes", SectionKind::Suffix, s..r.pos());

    chunk.groups.push(ColumnGroup::new(
        "Change columns",
        ColContext::DocChanges,
        build_columns(ColContext::DocChanges, &change_meta, change_data, Some(op_start - change_len))?,
        meta_entry_len(&change_meta),
    ));
    chunk.groups.push(ColumnGroup::new(
        "Op columns",
        ColContext::DocOps,
        build_columns(ColContext::DocOps, &op_meta, op_data, Some(op_start))?,
        meta_entry_len(&op_meta),
    ));
    Ok(())
}

/// Encoded size of a column metadata table.
fn meta_entry_len(meta: &[(crate::columns::ColumnSpec, usize)]) -> usize {
    let mut out = Vec::new();
    write_uleb(&mut out, meta.len() as u64);
    for (spec, len) in meta {
        write_uleb(&mut out, spec.0 as u64);
        write_uleb(&mut out, *len as u64);
    }
    out.len()
}

/// Parses a change chunk body. `base` is `None` when the body isn't
/// verbatim in the file (it was inflated from a compressed chunk).
fn parse_change(chunk: &mut Chunk, body: &[u8], base: Option<usize>) -> Result<()> {
    let mut r = Reader::new(body, base.unwrap_or(0));
    let in_file = base.is_some();
    let s = r.pos();
    chunk.facts.deps = read_hashes(&mut r)?;
    let actor = hex_actor(r.len_prefixed()?);
    let seq = r.uleb()?;
    let start_op = r.uleb()?;
    let time = r.sleb()?;
    let message = String::from_utf8_lossy(r.len_prefixed()?).into_owned();
    let other_actors = read_actors(&mut r)?;
    if in_file {
        section(chunk, "change header", SectionKind::Metadata, s..r.pos());
    }
    let s = r.pos();
    let meta = parse_column_meta(&mut r)?;
    if in_file {
        section(chunk, "column metadata", SectionKind::ColumnMeta, s..r.pos());
    }
    let op_len: usize = meta.iter().map(|(_, l)| l).sum();
    let op_start = r.pos();
    let data = r.take(op_len)?;
    if in_file {
        section(chunk, "op columns", SectionKind::OpColumns, op_start..r.pos());
    }
    chunk.facts.extra_bytes = r.remaining();
    if in_file {
        section(chunk, "extra bytes", SectionKind::Suffix, r.pos()..r.pos() + r.remaining());
    }
    let mut actors = vec![actor.clone()];
    actors.extend(other_actors.iter().cloned());
    chunk.facts.actors = actors;
    chunk.facts.change = Some(ChangeHeader {
        actor,
        seq,
        start_op,
        time,
        message: (!message.is_empty()).then_some(message),
        other_actors,
    });
    chunk.groups.push(ColumnGroup::new(
        "Op columns",
        ColContext::ChangeOps,
        build_columns(ColContext::ChangeOps, &meta, data, base.map(|_| op_start))?,
        meta_entry_len(&meta),
    ));
    Ok(())
}

fn parse_compressed(chunk: &mut Chunk, body: &[u8], base: usize) -> Result<()> {
    section(chunk, "deflated change", SectionKind::Compressed, base..base + body.len());
    let inflated = inflate(body).map_err(|e| ParseError {
        offset: base,
        what: format!("inflating chunk: {e}"),
    })?;
    chunk.inflated_len = Some(inflated.len());
    let inner_hash = chunk_hash(1, &inflated);
    chunk.checksum_ok = inner_hash[..4] == chunk.checksum;
    chunk.hash = inner_hash;
    parse_change(chunk, &inflated, None)
}

/// The column section shared by bundles and v0 bundles: deps, actors,
/// change columns and op columns.
fn parse_column_section(
    chunk: &mut Chunk,
    body: &[u8],
    base: usize,
    change_ctx: ColContext,
) -> Result<()> {
    let mut r = Reader::new(body, base);
    let s = r.pos();
    let deps = read_hashes(&mut r)?;
    let actors = read_actors(&mut r)?;
    section(chunk, "deps + actors", SectionKind::Metadata, s..r.pos());
    chunk.facts.deps = deps;
    chunk.facts.actors = actors;

    let s = r.pos();
    let change_meta = parse_column_meta(&mut r)?;
    section(chunk, "change column metadata", SectionKind::ColumnMeta, s..r.pos());
    let change_len: usize = change_meta.iter().map(|(_, l)| l).sum();
    let change_start = r.pos();
    let change_data = r.take(change_len)?;
    section(chunk, "change columns", SectionKind::ChangeColumns, change_start..r.pos());

    let s = r.pos();
    let op_meta = parse_column_meta(&mut r)?;
    section(chunk, "op column metadata", SectionKind::ColumnMeta, s..r.pos());
    let op_len: usize = op_meta.iter().map(|(_, l)| l).sum();
    let op_start = r.pos();
    let op_data = r.take(op_len)?;
    section(chunk, "op columns", SectionKind::OpColumns, op_start..r.pos());
    if !r.is_empty() {
        section(chunk, "unparsed tail", SectionKind::Suffix, r.pos()..r.pos() + r.remaining());
    }

    chunk.groups.push(ColumnGroup::new(
        "Change columns",
        change_ctx,
        build_columns(change_ctx, &change_meta, change_data, Some(change_start))?,
        meta_entry_len(&change_meta),
    ));
    chunk.groups.push(ColumnGroup::new(
        "Op columns",
        ColContext::BundleOps,
        build_columns(ColContext::BundleOps, &op_meta, op_data, Some(op_start))?,
        meta_entry_len(&op_meta),
    ));
    Ok(())
}

fn parse_bundle(chunk: &mut Chunk, body: &[u8], base: usize) -> Result<()> {
    let mut r = Reader::new(body, base);
    let s = r.pos();
    let prefix_actors = read_actors(&mut r)?;
    let n_heads = r.count(33)?;
    for _ in 0..n_heads {
        chunk.facts.heads.push(hex::encode(r.hash()?));
        r.uleb()?;
    }
    let mut counts = vec![];
    for label in ["checkpoints", "retained"] {
        let n = r.count(33)?;
        for _ in 0..n {
            r.uleb()?;
            r.hash()?;
        }
        counts.push((label, n));
    }
    let n_boundary = r.count(34)?;
    for _ in 0..n_boundary {
        r.hash()?;
        r.uleb()?;
        r.uleb()?;
    }
    counts.push(("boundary", n_boundary));
    let n_deps = r.count(2)?;
    for _ in 0..n_deps {
        r.uleb()?;
        r.uleb()?;
    }
    counts.push(("dep ids", n_deps));
    counts.push(("prefix actors", prefix_actors.len()));
    chunk.facts.bundle_prefix = counts;
    section(chunk, "fragment prefix", SectionKind::Metadata, s..r.pos());
    let rest_start = r.pos();
    let rest = r.rest();
    parse_column_section(chunk, rest, rest_start, ColContext::BundleChanges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/../../corpus/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn every_corpus_file_parses_cleanly() {
        let dir = format!("{}/../../corpus", env!("CARGO_MANIFEST_DIR"));
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            let report = parse_file(&bytes);
            assert!(report.trailing.is_none(), "{path:?}: {:?}", report.trailing);
            for c in &report.chunks {
                assert!(c.error.is_none(), "{path:?} chunk {}: {:?}", c.index, c.error);
                assert!(c.checksum_ok, "{path:?} chunk {} checksum", c.index);
                let covered: usize = c.sections.iter().map(|s| s.len()).sum();
                if c.kind != ChunkKind::Compressed {
                    assert_eq!(covered, c.len(), "{path:?} chunk {} sections", c.index);
                }
                for g in &c.groups {
                    for col in &g.columns {
                        let runs = crate::columns::decode_runs(col);
                        assert!(runs.error.is_none(), "{path:?} {}: {:?}", col.name, runs.error);
                    }
                }
            }
        }
    }

    #[test]
    fn save_plus_incrementals_layout() {
        let report = parse_file(&corpus("todo-save-plus-incrementals.automerge"));
        assert_eq!(report.chunks[0].kind, ChunkKind::Document);
        assert!(report.chunks[1..].iter().all(|c| c.kind == ChunkKind::Change));
        let count = |o| report.chunks.iter().filter(|c| c.dep_order == o).count();
        assert_eq!((count(DepOrder::Late), count(DepOrder::Missing)), (2, 2));
        let report = parse_file(&corpus("text-next-save-plus-incrementals.automerge"));
        assert!(report.chunks.iter().all(|c| c.kind == ChunkKind::Bundle));
    }
}
