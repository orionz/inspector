//! Fetching documents from Patchwork's subduction sync server.

use std::collections::{BTreeSet, HashMap, HashSet};

use automerge::{Change, ChangeHash};

use future_form::Local;
use futures::channel::{mpsc, oneshot};
use futures::future::LocalBoxFuture;
use futures::{FutureExt, StreamExt};
use sedimentree_core::loose_commit::id::CommitId;
use sedimentree_core::crypto::fingerprint::FingerprintSeed;
use sedimentree_core::fragment::Fragment;
use sedimentree_core::loose_commit::LooseCommit;
use sedimentree_core::sedimentree::{FingerprintSummary, Sedimentree, SedimentreeItem};
use sedimentree_core::id::SedimentreeId;
use subduction_core::connection::message::{BatchSyncRequest, RequestId, SyncMessage, SyncResult};
use subduction_core::handshake::{self, audience::Audience, Handshake};
use subduction_core::peer::id::PeerId;
use subduction_core::timestamp::TimestampSeconds;
use subduction_crypto::{nonce::Nonce, signer::memory::MemorySigner};
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{BinaryType, MessageEvent, WebSocket};

pub const SERVER: &str = "wss://subduction.sync.inkandswitch.com";

/// The `automerge:…` document id inside a Patchwork link or a bare automerge URL, if any.
pub fn doc_id(link: &str) -> Option<String> {
    let start = link.find("automerge:")? + "automerge:".len();
    let id: String = link[start..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (!id.is_empty()).then_some(id)
}

/// A readable name for a Patchwork link: its `title=` parameter if present.
pub fn title(link: &str) -> Option<String> {
    let start = link.find("title=")? + "title=".len();
    let raw: String = link[start..].chars().take_while(|&c| c != '&').collect();
    js_sys::decode_uri_component(&raw.replace('+', " ")).ok()?.as_string()
}

/// What the server stores for a document, reduced to the pieces a reader needs.
pub struct Fetched {
    /// The fragments and then the kept loose commits, each in causal order
    /// and each the automerge chunk it was stored as.
    pub chunks: Vec<Vec<u8>>,
    pub fragments: usize,
    /// Loose commits kept, of those sent; the rest only repeat changes inside fragments.
    pub commits: (usize, usize),
}

/// Downloads everything the server stores for the document, leaving out
/// loose commits whose change a fragment already holds.
pub async fn fetch(doc_id: &str) -> Result<Fetched, String> {
    let uuid = bs58::decode(doc_id)
        .with_check(None)
        .into_vec()
        .map_err(|e| format!("not an automerge document id: {e}"))?;
    let mut id = [0u8; 32];
    id[..uuid.len().min(32)].copy_from_slice(&uuid[..uuid.len().min(32)]);

    let socket = Socket::open(SERVER).await?;
    let signer = MemorySigner::generate();
    let host = SERVER.trim_start_matches("wss://");
    let now = TimestampSeconds::new((js_sys::Date::now() / 1000.0) as u64);
    let (_, mut socket) = handshake::initiate::<Local, _, _, _, _>(
        socket,
        |s, _| ((), s),
        &signer,
        Audience::discover(host.as_bytes()),
        now,
        Nonce::random(),
    )
    .await
    .map_err(|e| format!("handshake with {host} failed: {e:?}"))?;

    let req = SyncMessage::BatchSyncRequest(BatchSyncRequest {
        id: SedimentreeId::new(id),
        req_id: RequestId { requestor: PeerId::from(signer.verifying_key()), nonce: 1 },
        fingerprint_summary: FingerprintSummary::new(FingerprintSeed::random(), BTreeSet::new(), BTreeSet::new()),
        subscribe: false,
    });
    socket.send_bytes(&req.encode())?;
    let result = loop {
        let bytes = socket.next().await.ok_or("the server closed the connection")?;
        if !bytes.starts_with(b"SUM\0") {
            continue;
        }
        if let Ok(SyncMessage::BatchSyncResponse(resp)) = SyncMessage::try_decode(&bytes) {
            break resp.result;
        }
    };
    socket.close();
    let diff = match result {
        SyncResult::Ok(diff) => diff,
        SyncResult::NotFound => return Err("the sync server doesn't have this document".into()),
        SyncResult::Unauthorized => return Err("the sync server refused access to this document".into()),
    };
    let mut blobs: HashMap<CommitId, Vec<u8>> = HashMap::new();
    let mut fragments = vec![];
    let mut commits = vec![];
    for (signed, blob) in diff.missing_fragments {
        let f: Fragment = signed.try_decode_trusted_payload().map_err(|e| format!("bad fragment: {e}"))?;
        blobs.insert(f.head(), blob.into_contents());
        fragments.push(f);
    }
    for (signed, blob) in diff.missing_commits {
        let c: LooseCommit = signed.try_decode_trusted_payload().map_err(|e| format!("bad commit: {e}"))?;
        blobs.entry(c.head()).or_insert_with(|| blob.into_contents());
        commits.push(c);
    }
    let tree = Sedimentree::new(fragments, commits);
    let kept_fragments: Vec<&Fragment> = tree.fragments().collect();
    let kept_commits: Vec<&LooseCommit> = tree.loose_commits().collect();
    let order = tree.topsorted_blob_order().map_err(|e| format!("{e:?}"))?;
    let mut fragment_chunks = vec![];
    let mut commit_chunks = vec![];
    for item in order {
        match item {
            SedimentreeItem::Fragment(i) => fragment_chunks.extend(blobs.remove(&kept_fragments[i].head())),
            SedimentreeItem::LooseCommit(i) => commit_chunks.extend(blobs.remove(&kept_commits[i].head())),
        }
    }
    let mut in_fragments = automerge::Automerge::new();
    let _ = in_fragments.load_incremental(&fragment_chunks.concat());
    let covered: HashSet<ChangeHash> = in_fragments.get_changes_meta(&[]).iter().map(|m| m.hash).collect();
    let sent_commits = commit_chunks.len();
    commit_chunks.retain(|c| Change::try_from(&c[..]).map_or(true, |c| !covered.contains(&c.hash())));
    Ok(Fetched {
        fragments: fragment_chunks.len(),
        commits: (commit_chunks.len(), sent_commits),
        chunks: fragment_chunks.into_iter().chain(commit_chunks).collect(),
    })
}

struct Socket {
    ws: WebSocket,
    inbox: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Socket {
    async fn open(url: &str) -> Result<Self, String> {
        let ws = WebSocket::new(url).map_err(|e| format!("could not open {url}: {e:?}"))?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let (tx, inbox) = mpsc::unbounded();
        let onmessage = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
            if let Ok(buf) = ev.data().dyn_into::<js_sys::ArrayBuffer>() {
                let _ = tx.unbounded_send(js_sys::Uint8Array::new(&buf).to_vec());
            }
        });
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();
        let (opened, wait) = oneshot::channel::<bool>();
        let opened = std::rc::Rc::new(std::cell::Cell::new(Some(opened)));
        let (o2, o3) = (opened.clone(), opened);
        let onopen = Closure::once_into_js(move || {
            if let Some(s) = o2.take() {
                let _ = s.send(true);
            }
        });
        let onclose = Closure::<dyn FnMut()>::new(move || {
            if let Some(s) = o3.take() {
                let _ = s.send(false);
            }
        });
        ws.set_onopen(Some(onopen.unchecked_ref()));
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();
        if !wait.await.unwrap_or(false) {
            return Err(format!("could not connect to {url}"));
        }
        Ok(Self { ws, inbox })
    }

    fn send_bytes(&self, bytes: &[u8]) -> Result<(), String> {
        self.ws.send_with_u8_array(bytes).map_err(|e| format!("send failed: {e:?}"))
    }

    async fn next(&mut self) -> Option<Vec<u8>> {
        self.inbox.next().await
    }

    fn close(&self) {
        let _ = self.ws.close();
    }
}

impl Handshake<Local> for Socket {
    type Error = String;

    fn send(&mut self, bytes: Vec<u8>) -> LocalBoxFuture<'_, Result<(), String>> {
        let r = self.send_bytes(&bytes);
        async move { r }.boxed_local()
    }

    fn recv(&mut self) -> LocalBoxFuture<'_, Result<Vec<u8>, String>> {
        async move { self.next().await.ok_or_else(|| "connection closed".to_string()) }.boxed_local()
    }
}
