//! Fetching documents from Patchwork's subduction sync server.

use std::collections::BTreeSet;

use future_form::Local;
use futures::channel::{mpsc, oneshot};
use futures::future::LocalBoxFuture;
use futures::{FutureExt, StreamExt};
use sedimentree_core::crypto::fingerprint::FingerprintSeed;
use sedimentree_core::id::SedimentreeId;
use sedimentree_core::sedimentree::FingerprintSummary;
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

/// Downloads every fragment and then every loose commit the server stores
/// for the document, each as the automerge chunk it was stored as.
pub async fn fetch(doc_id: &str) -> Result<Vec<Vec<u8>>, String> {
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
    match result {
        SyncResult::Ok(diff) => Ok(diff
            .missing_fragments
            .into_iter()
            .map(|(_, blob)| blob.into_contents())
            .chain(diff.missing_commits.into_iter().map(|(_, blob)| blob.into_contents()))
            .collect()),
        SyncResult::NotFound => Err("the sync server doesn't have this document".into()),
        SyncResult::Unauthorized => Err("the sync server refused access to this document".into()),
    }
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
