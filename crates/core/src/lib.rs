//! Structural parsing and storage analysis for automerge files.

pub mod chunk;
pub mod columns;
pub mod leb;

pub use chunk::{parse_file, parse_structure, Chunk, ChunkKind, DepOrder, FileReport, Section, SectionKind};
pub use columns::{ColumnGroup, ColumnInfo};
pub mod doc;

pub use doc::{chunk_changes, decode_change, format_time, DocModel};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_loads_as_documents() {
        let dir = format!("{}/../../corpus", env!("CARGO_MANIFEST_DIR"));
        let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        names.sort();
        for path in names {
            let bytes = std::fs::read(&path).unwrap();
            let report = parse_file(&bytes);
            let model = DocModel::load(&bytes);
            let changes_in_chunks: Vec<String> = report
                .chunks
                .iter()
                .map(|c| match chunk_changes(&bytes, c) {
                    Ok(cs) => format!("{}", cs.len()),
                    Err(e) => format!("ERR({e})"),
                })
                .collect();
            let summary = match &model {
                Ok(m) => {
                    let mid = m.heads_at(m.changes.len() / 2);
                    let page = m.page(&m.root(), mid.as_deref(), 0, 50);
                    format!("{} changes, {} actors, mid-root keys {}", m.changes.len(), m.actors.len(), page.total)
                }
                Err(e) => format!("LOAD ERR {e}"),
            };
            if let Ok(m) = &model {
                let root_ops = m.object_ops(&automerge::ROOT).len();
                let root_keys = m.page(&m.root(), None, 0, usize::MAX).total;
                assert!(root_keys == 0 || root_ops > 0, "{path:?}: ROOT has keys but no ops");
                let (json, _) = m.json(&automerge::ROOT, None, 1 << 20);
                assert!(json.starts_with('{') && (json.len() > 1 << 20 || json.ends_with('}')), "{path:?}: bad JSON");
            }
            let shown: Vec<_> = changes_in_chunks.iter().take(6).collect();
            println!("{:?}: {summary}; per-chunk {:?}", path.file_name().unwrap(), shown);
        }
    }
}
