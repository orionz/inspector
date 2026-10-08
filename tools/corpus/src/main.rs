//! Generates sample files covering every chunk layout the inspector understands.
//!
//! Usage: `cargo run -p corpus --release -- <out-dir>`

use std::path::{Path, PathBuf};

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, Author, ObjType, ReadDoc, ScalarValue, ROOT};

const T0: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z
const MINUTE: i64 = 60_000;

fn actor(name: &str) -> ActorId {
    ActorId::from(name.as_bytes())
}

fn author(name: &str) -> Option<Author<'static>> {
    Some(Author::from(name.as_bytes().to_vec()))
}

fn commit(doc: &mut AutoCommit, minute: i64, msg: &str) {
    doc.commit_with(
        CommitOptions::default()
            .with_time(T0 + minute * MINUTE)
            .with_message(msg),
    );
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    println!("{:>9}  {}", bytes.len(), path.display());
}

/// A todo-list app: maps, lists, counters, nested objects, several authors.
fn todo_doc() -> AutoCommit {
    let mut doc = AutoCommit::new()
        .with_actor(actor("alice-laptop"))
        .with_author(author("alice"));
    doc.put(ROOT, "title", "Groceries").unwrap();
    let items = doc.put_object(ROOT, "items", ObjType::List).unwrap();
    doc.put(ROOT, "visits", ScalarValue::counter(0)).unwrap();
    commit(&mut doc, 0, "create list");

    for (i, what) in ["milk", "eggs", "bread", "coffee"].iter().enumerate() {
        let item = doc.insert_object(&items, i, ObjType::Map).unwrap();
        doc.put(&item, "text", *what).unwrap();
        doc.put(&item, "done", false).unwrap();
        doc.put(&item, "qty", (i + 1) as i64).unwrap();
        commit(&mut doc, 1 + i as i64, &format!("add {what}"));
    }

    let mut bob = doc
        .fork()
        .with_actor(actor("bob-phone"))
        .with_author(author("bob"));
    let bob_items = bob.get(ROOT, "items").unwrap().unwrap().1;
    let first = bob.get(&bob_items, 0).unwrap().unwrap().1;
    bob.put(&first, "done", true).unwrap();
    bob.increment(ROOT, "visits", 1).unwrap();
    commit(&mut bob, 10, "bought milk");

    doc.put(ROOT, "title", "Weekly groceries").unwrap();
    doc.increment(ROOT, "visits", 1).unwrap();
    commit(&mut doc, 11, "rename");
    doc.merge(&mut bob).unwrap();

    let meta = doc.put_object(ROOT, "meta", ObjType::Map).unwrap();
    doc.put(&meta, "created", ScalarValue::Timestamp(T0)).unwrap();
    doc.put(&meta, "tags", "home").unwrap();
    doc.put(&meta, "pi", 3.14159).unwrap();
    doc.put(&meta, "blob", ScalarValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef]))
        .unwrap();
    doc.put(&meta, "nothing", ScalarValue::Null).unwrap();
    commit(&mut doc, 12, "metadata");
    doc.delete(&items, 2).unwrap();
    commit(&mut doc, 13, "remove bread");
    doc
}

/// Collaborative rich text: concurrent typing by three authors plus marks.
fn text_doc(rounds: usize) -> AutoCommit {
    let mut doc = AutoCommit::new()
        .with_actor(actor("ann-desktop"))
        .with_author(author("ann"));
    let text = doc.put_object(ROOT, "content", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, "The quick brown fox jumps over the lazy dog.\n")
        .unwrap();
    commit(&mut doc, 0, "initial text");

    let mut minute = 1;
    for round in 0..rounds {
        let mut forks = Vec::new();
        for (who, dev) in [("ben", "ben-tablet"), ("cat", "cat-laptop")] {
            let mut f = doc
                .fork()
                .with_actor(actor(&format!("{dev}-{round}")))
                .with_author(author(who));
            let len = f.length(&text);
            let at = (round * 7 + who.len() * 3) % len.max(1);
            f.splice_text(&text, at, 0, &format!("[{who}{round}]")).unwrap();
            commit(&mut f, minute, &format!("{who} edit {round}"));
            minute += 1;
            forks.push(f);
        }
        for f in &mut forks {
            doc.merge(f).unwrap();
        }
        let len = doc.length(&text);
        doc.splice_text(&text, len, 0, &format!("Paragraph {round}. ")).unwrap();
        if len > 10 {
            doc.mark(
                &text,
                Mark::new("bold".into(), true, 4, 9),
                ExpandMark::After,
            )
            .unwrap();
        }
        if round % 3 == 2 {
            doc.splice_text(&text, 0, 4, "").unwrap();
        }
        commit(&mut doc, minute, &format!("ann round {round}"));
        minute += 1;
    }
    doc
}

/// Saves the base, then appends `save_incremental` output after each batch of edits.
fn save_plus_incrementals(mut doc: AutoCommit, batches: usize) -> Vec<u8> {
    let mut out = doc.save();
    let text = doc
        .get(ROOT, "content")
        .unwrap()
        .map(|(_, id)| id)
        .unwrap_or_else(|| doc.put_object(ROOT, "content", ObjType::Text).unwrap());
    for b in 0..batches {
        for e in 0..(b + 1) * 3 {
            let len = doc.length(&text);
            doc.splice_text(&text, len, 0, &format!("inc{b}.{e} ")).unwrap();
            commit(&mut doc, 1_000 + (b * 10 + e) as i64, &format!("batch {b} edit {e}"));
        }
        out.extend(doc.save_incremental());
    }
    out
}

fn concat_changes(doc: &mut AutoCommit) -> Vec<u8> {
    doc.get_changes(&[])
        .into_iter()
        .flat_map(|c| c.raw_bytes().to_vec())
        .collect()
}

fn main() {
    let dir: PathBuf = std::env::args().nth(1).unwrap_or("corpus".into()).into();
    std::fs::create_dir_all(&dir).unwrap();

    let mut todo = todo_doc();
    write(&dir, "todo-doc.automerge", &todo.save());
    write(&dir, "todo-doc-nocompress.automerge", &todo.save_nocompress());
    write(&dir, "todo-changes.automerge", &concat_changes(&mut todo));

    let mut text = text_doc(40);
    write(&dir, "text-doc.automerge", &text.save());
    write(&dir, "text-changes.automerge", &concat_changes(&mut text));

    write(
        &dir,
        "todo-save-plus-incrementals.automerge",
        &save_plus_incrementals(todo_doc(), 4),
    );
    write(
        &dir,
        "text-save-plus-incrementals.automerge",
        &save_plus_incrementals(text_doc(25), 6),
    );

    let mut next = text_doc(40).into_next();
    write(&dir, "text-next-changeset.automerge", &next.save());

    write(
        &dir,
        "text-next-save-plus-incrementals.automerge",
        &next_api::save_plus_incrementals(text_doc(10).into_next(), 3),
    );
}

mod next_api {
    use automerge::next::transaction::Transactable;
    use automerge::next::{AutoCommit, ReadDoc};
    use automerge::ROOT;

    pub fn save_plus_incrementals(mut doc: AutoCommit, batches: usize) -> Vec<u8> {
        let mut bytes = doc.save();
        let content = doc.get(ROOT, "content").unwrap().unwrap().1;
        for b in 0..batches {
            let len = doc.length(&content);
            doc.splice_text(&content, len, 0, &format!(" next-inc-{b}"))
                .unwrap();
            doc.commit();
            bytes.extend(doc.save_incremental());
        }
        bytes
    }
}
