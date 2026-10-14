// Runs the inspector's wasm as the document worker; see crates/app/src/worker.rs.
import init from './pkg/automerge-inspector.js?v=__VERSION__';
init({ module_or_path: './pkg/automerge-inspector_bg.wasm?v=__VERSION__' });
