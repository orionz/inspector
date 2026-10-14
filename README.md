# Automerge Inspector

A Rust → WebAssembly PWA for looking inside automerge files: which chunks they
contain, where every byte goes, and what the document looked like at any point
in its history. Everything runs locally in the browser.

## What it shows

The three views sit on folder tabs above the file area.

**Load tab**
- The file as a vertical stack of chunks, coloured by type (document, change,
  compressed change, change set, v0 bundle). Block height follows chunk size,
  on a log scale when sizes differ by more than 8×; runs of chunks around 1%
  of the file or smaller are drawn as 3px bands, one per chunk. Hovering
  anywhere zooms out a panel for the chunk under the cursor (bytes, share of
  file, changes, ops, time, actor, message) with its neighbours half-sized
  behind it. Changes whose dependencies
  only appear later in the file are orange; changes with a dependency missing
  from the file entirely (so they can't apply) are red.
- Clicking a chunk shows its parts: Header (actor count), Changes and Ops
  (row count and bytes), and Extra when a change carries bytes after its
  columns. Clicking a part opens its detail and shrinks the parts into a
  second band beside the chunk band. In either band, click another chunk or
  part to switch to it, or the open one to go back; clicking a band of small
  chunks also goes back to the stack.
- Header: facts (actors, heads, deps, seq/time/message, change-set fragment
  prefix counts, checksum) and a byte-layout bar of the whole chunk.
- Changes and Ops get Physical / Logical tabs on the left edge:
  - **Physical**: the columns side by side, coloured by type, hanging from
    their names with length proportional to stored bytes (hover for share of
    group/file and bytes per row)
  - **Logical**: the columns recombined into rows: id first, op ids as
    `ctr@abcd` with the actor's first four hex digits, `ROOT` for the root
    object, insert as `t`/`f`, the action and value as one column
    (`makeMap`, `Set "cow"`, `Inc 27`) and pred/succ as comma-separated ids.
    The table runs to the bottom of the window and scrolls with the page
- Columns that share an id (`obj.actor` + `obj.ctr`, `value.meta` +
  `value.raw`, …) highlight and open together. Clicking one shrinks the
  columns into a third band (each sized by its bytes) and opens their runs
  beside it, one over the other; click another column in the band to
  switch, or the open one to go back. The runs show the repeat / literal /
  null byte split, every run with its row range, decoded values (deltas shown
  with their absolute span) and encoded bytes. Raw value columns show bytes per value type.

**Document tab**
- The document as a lazy, collapsible tree of its maps, lists and text. Each
  value shows the author/actor and time of the op that set it.

**Save tab**
- The Load tab's view of what `doc.save()` writes after loading the file, with
  its size against the original. Changes that couldn't be applied are written
  after the document, so they show up here too.

## Layout

```
crates/core    am-inspect: structural parser + analysis (native-testable)
  chunk.rs     splits a file into chunks, parses every chunk type into byte sections + column groups
  columns.rs   column specs and names, run-level RLE/bool/delta decoding, row reconstruction
  doc.rs       automerge-backed history timeline, object pages, per-chunk change decoding
crates/app     Leptos (CSR) UI compiled to wasm
tools/corpus   generator for the sample corpus
corpus/        sample files, also served as in-app samples
web/           index.html, styles, service worker, manifest, icons
```

The structural parser is independent of automerge's internals, so it reports
exact byte offsets for every section, column and run, and still shows the
structure of files automerge itself refuses to load. Document loading, history
and op decoding use automerge's public API.

## Building

The app builds against automerge from source, expected at `../automerge`
(currently the `hashless` branch).

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127   # must match the wasm-bindgen crate
./build.sh            # release build into dist/ (runs wasm-opt if it's on PATH)
./build.sh --dev      # faster debug build
python3 -m http.server -d dist 8000
```

Tests (native): `cargo test -p am-inspect`. They parse and load every file in
`corpus/`.

Dump a file's structure from the command line:
`cargo run -p am-inspect --example dump -- corpus/essay.amrg`

## Corpus

`corpus/` mixes generated files with real-world documents and fixtures from
the automerge repository:

| file | chunks |
|------|--------|
| `todo-save-plus-incrementals` | `save()` then 4 × `save_incremental()`: document + change chunks |
| `text-save-plus-incrementals` | the same pattern on collaborative rich text |
| `text-next-save-plus-incrementals` | `next` API: change-set save + incremental change sets |
| `text-next-changeset` | a whole document as one change set |
| `todo-doc`, `text-doc` | single document chunks |
| `todo-changes`, `text-changes` | concatenated change chunks |
| `two_change_chunks_compressed` | deflate-compressed change chunks |
| `bundle-v0-3.3.2` | the frozen bundle format written by automerge 3.3.x |
| `essay.amrg` | real doc chunk + 449 change chunks (263k changes) |
| `monday-meeting-notes`, `godot-modeled` | real-world documents with deflated columns |
| `64bit_obj_id_change` | invalid change (error handling) |

Regenerate the generated ones with `cargo run -p corpus --release -- corpus`.

## Deploying to GitHub Pages

`.github/workflows/pages.yml` checks out this repo next to
`automerge/automerge` (ref from the `AUTOMERGE_REF` repository variable,
default `hashless`), runs the tests, builds, and publishes `dist/`. In the
repository settings, set **Pages → Source** to **GitHub Actions**. All asset
paths are relative, so the site works under `https://<user>.github.io/<repo>/`.
