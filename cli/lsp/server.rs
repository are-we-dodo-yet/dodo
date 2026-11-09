// Copyright 2018-2026 the Deno authors. MIT license.

use std::io;
use std::io::BufRead;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use bytes::Bytes;
use dashmap::DashMap;
use deno_core::error::AnyError;
use deno_core::futures::FutureExt;
use deno_core::futures::StreamExt;
use deno_core::futures::future::BoxFuture;
use deno_core::futures::future::LocalBoxFuture;
use deno_core::futures::stream;
use deno_core::futures::stream::FuturesUnordered;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::io::StreamReader;
use tokio_util::sync::CancellationToken;
use tower::Service;
use tower_lsp_server::LspService;
use tower_lsp_server::Server;
use tower_lsp_server::jsonrpc::Error as LspError;
use tower_lsp_server::jsonrpc::Id;
use tower_lsp_server::jsonrpc::Request;
use tower_lsp_server::jsonrpc::Response;
use tower_lsp_server::jsonrpc::Result as LspResult;
use tower_lsp_server::ls_types::request::GotoImplementationParams;
use tower_lsp_server::ls_types::request::GotoImplementationResponse;
use tower_lsp_server::ls_types::request::GotoTypeDefinitionParams;
use tower_lsp_server::ls_types::request::GotoTypeDefinitionResponse;
use tower_lsp_server::ls_types::*;

use super::client::Client;
use super::language_server::LanguageServer;
use super::lsp_custom;
use super::lsp_custom::TaskDefinition;
use super::testing;

tokio::task_local! {
  static REQUEST_TOKEN: CancellationToken;
}

pub async fn serve() -> Result<(), AnyError> {
  let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
  let mut server = None;
  let (service, socket) = LspService::build(|client| {
    server = Some(LanguageServer::new(Client::from_tower(client)));
    LanguageServerProxy { jobs: jobs_tx }
  })
  .custom_method(
    lsp_custom::PERFORMANCE_REQUEST,
    LanguageServerProxy::performance_request,
  )
  .custom_method(
    lsp_custom::INFERRED_TYPE_REQUEST,
    LanguageServerProxy::inferred_type,
  )
  .custom_method(
    lsp_custom::TASK_REQUEST,
    LanguageServerProxy::task_definitions,
  )
  .custom_method(
    testing::TEST_RUN_REQUEST,
    LanguageServerProxy::test_run_request,
  )
  .custom_method(
    testing::TEST_RUN_CANCEL_REQUEST,
    LanguageServerProxy::test_run_cancel_request,
  )
  .custom_method(
    lsp_custom::VIRTUAL_TEXT_DOCUMENT,
    LanguageServerProxy::virtual_text_document,
  )
  .finish();
  let server = server.expect("LspService::build calls init");

  let pending = Arc::new(PendingRequests::default());
  let stdin = spawn_stdin_reader(pending.clone())?;
  let service = CancellableService {
    inner: service,
    pending,
  };
  let serve = Server::new(stdin, tokio::io::stdout(), socket)
    .concurrency_level(32)
    .serve(service);

  tokio::select! {
    () = serve => {}
    () = run_jobs(&server, jobs_rx) => {}
  }
  Ok(())
}

type Job =
  Box<dyn for<'a> FnOnce(&'a LanguageServer) -> LocalBoxFuture<'a, ()> + Send>;

fn job<F>(f: F) -> Job
where
  F: for<'a> FnOnce(&'a LanguageServer) -> LocalBoxFuture<'a, ()>
    + Send
    + 'static,
{
  Box::new(f)
}

async fn run_jobs(
  server: &LanguageServer,
  mut jobs: mpsc::UnboundedReceiver<Job>,
) {
  let mut running = FuturesUnordered::new();
  loop {
    tokio::select! {
      job = jobs.recv() => match job {
        Some(job) => running.push(job(server)),
        None => break,
      },
      Some(()) = running.next() => {}
    }
  }
  while running.next().await.is_some() {}
}

trait CancelledResponse {
  fn cancelled() -> Self;
}

impl CancelledResponse for () {
  fn cancelled() {}
}

impl<T> CancelledResponse for LspResult<T> {
  fn cancelled() -> Self {
    Err(LspError::request_cancelled())
  }
}

struct LanguageServerProxy {
  jobs: mpsc::UnboundedSender<Job>,
}

impl LanguageServerProxy {
  async fn call<R, F>(&self, f: F) -> R
  where
    R: CancelledResponse + Send + 'static,
    F: for<'a> FnOnce(
        &'a LanguageServer,
        CancellationToken,
      ) -> LocalBoxFuture<'a, R>
      + Send
      + 'static,
  {
    let token = REQUEST_TOKEN
      .try_with(CancellationToken::clone)
      .unwrap_or_default();
    let guard = token.clone().drop_guard();
    let (tx, rx) = oneshot::channel();
    let job = job(move |server| {
      async move {
        tokio::select! {
          biased;
          () = token.cancelled() => {}
          response = f(server, token.clone()) => {
            let _ = tx.send(response);
          }
        }
      }
      .boxed_local()
    });
    let response = if self.jobs.send(job).is_ok() {
      rx.await.unwrap_or_else(|_| R::cancelled())
    } else {
      R::cancelled()
    };
    guard.disarm();
    response
  }

  async fn performance_request(&self) -> LspResult<Option<Value>> {
    self
      .call(|server, token| server.performance_request(token).boxed_local())
      .await
  }

  async fn inferred_type(
    &self,
    params: Option<Value>,
  ) -> LspResult<Option<Value>> {
    self
      .call(move |server, token| {
        server.inferred_type(params, token).boxed_local()
      })
      .await
  }

  async fn task_definitions(&self) -> LspResult<Vec<TaskDefinition>> {
    self
      .call(|server, token| server.task_definitions(token).boxed_local())
      .await
  }

  async fn test_run_request(
    &self,
    params: Option<Value>,
  ) -> LspResult<Option<Value>> {
    self
      .call(move |server, token| {
        server.test_run_request(params, token).boxed_local()
      })
      .await
  }

  async fn test_run_cancel_request(
    &self,
    params: Option<Value>,
  ) -> LspResult<Option<Value>> {
    self
      .call(move |server, token| {
        server.test_run_cancel_request(params, token).boxed_local()
      })
      .await
  }

  async fn virtual_text_document(
    &self,
    params: Option<Value>,
  ) -> LspResult<Option<Value>> {
    self
      .call(move |server, token| {
        server.virtual_text_document(params, token).boxed_local()
      })
      .await
  }
}

macro_rules! forward_requests {
  ($($name:ident($params:ty) -> $result:ty;)*) => {$(
    async fn $name(&self, params: $params) -> LspResult<$result> {
      self
        .call(move |server, token| server.$name(params, token).boxed_local())
        .await
    }
  )*};
}

macro_rules! forward_notifications {
  ($($name:ident($params:ty);)*) => {$(
    async fn $name(&self, params: $params) {
      self
        .call(move |server, _| server.$name(params).boxed_local())
        .await
    }
  )*};
}

impl tower_lsp_server::LanguageServer for LanguageServerProxy {
  async fn initialize(
    &self,
    params: InitializeParams,
  ) -> LspResult<InitializeResult> {
    self
      .call(move |server, _| server.initialize(params).boxed_local())
      .await
  }

  async fn shutdown(&self) -> LspResult<()> {
    self
      .call(|server, _| async move { server.shutdown() }.boxed_local())
      .await
  }

  async fn symbol(
    &self,
    params: WorkspaceSymbolParams,
  ) -> LspResult<Option<WorkspaceSymbolResponse>> {
    self
      .call(move |server, token| {
        async move {
          let symbols = server.symbol(params, token).await?;
          Ok(symbols.map(WorkspaceSymbolResponse::Flat))
        }
        .boxed_local()
      })
      .await
  }

  forward_notifications! {
    initialized(InitializedParams);
    did_open(DidOpenTextDocumentParams);
    did_change(DidChangeTextDocumentParams);
    did_save(DidSaveTextDocumentParams);
    did_close(DidCloseTextDocumentParams);
    notebook_did_open(DidOpenNotebookDocumentParams);
    notebook_did_change(DidChangeNotebookDocumentParams);
    notebook_did_save(DidSaveNotebookDocumentParams);
    notebook_did_close(DidCloseNotebookDocumentParams);
    did_change_configuration(DidChangeConfigurationParams);
    did_change_watched_files(DidChangeWatchedFilesParams);
    did_change_workspace_folders(DidChangeWorkspaceFoldersParams);
  }

  forward_requests! {
    execute_command(ExecuteCommandParams) -> Option<Value>;
    document_symbol(DocumentSymbolParams) -> Option<DocumentSymbolResponse>;
    formatting(DocumentFormattingParams) -> Option<Vec<TextEdit>>;
    hover(HoverParams) -> Option<Hover>;
    inlay_hint(InlayHintParams) -> Option<Vec<InlayHint>>;
    code_action(CodeActionParams) -> Option<CodeActionResponse>;
    code_action_resolve(CodeAction) -> CodeAction;
    code_lens(CodeLensParams) -> Option<Vec<CodeLens>>;
    code_lens_resolve(CodeLens) -> CodeLens;
    document_highlight(DocumentHighlightParams)
      -> Option<Vec<DocumentHighlight>>;
    references(ReferenceParams) -> Option<Vec<Location>>;
    goto_definition(GotoDefinitionParams) -> Option<GotoDefinitionResponse>;
    goto_type_definition(GotoTypeDefinitionParams)
      -> Option<GotoTypeDefinitionResponse>;
    completion(CompletionParams) -> Option<CompletionResponse>;
    completion_resolve(CompletionItem) -> CompletionItem;
    diagnostic(DocumentDiagnosticParams) -> DocumentDiagnosticReportResult;
    goto_implementation(GotoImplementationParams)
      -> Option<GotoImplementationResponse>;
    folding_range(FoldingRangeParams) -> Option<Vec<FoldingRange>>;
    incoming_calls(CallHierarchyIncomingCallsParams)
      -> Option<Vec<CallHierarchyIncomingCall>>;
    outgoing_calls(CallHierarchyOutgoingCallsParams)
      -> Option<Vec<CallHierarchyOutgoingCall>>;
    prepare_call_hierarchy(CallHierarchyPrepareParams)
      -> Option<Vec<CallHierarchyItem>>;
    rename(RenameParams) -> Option<WorkspaceEdit>;
    selection_range(SelectionRangeParams) -> Option<Vec<SelectionRange>>;
    semantic_tokens_full(SemanticTokensParams) -> Option<SemanticTokensResult>;
    semantic_tokens_range(SemanticTokensRangeParams)
      -> Option<SemanticTokensRangeResult>;
    signature_help(SignatureHelpParams) -> Option<SignatureHelp>;
    will_rename_files(RenameFilesParams) -> Option<WorkspaceEdit>;
  }
}

#[derive(Default)]
struct PendingRequests(DashMap<Id, CancellationToken>);

impl PendingRequests {
  fn token(&self, id: &Id) -> CancellationToken {
    self.0.entry(id.clone()).or_default().clone()
  }

  fn observe(&self, body: &[u8]) {
    #[derive(Deserialize)]
    struct Message {
      method: Option<String>,
      id: Option<Id>,
    }

    #[derive(Deserialize)]
    struct CancelRequest {
      params: CancelRequestParams,
    }

    #[derive(Deserialize)]
    struct CancelRequestParams {
      id: Id,
    }

    let Ok(message) = serde_json::from_slice::<Message>(body) else {
      return;
    };
    match (message.method.as_deref(), message.id) {
      (Some("$/cancelRequest"), _) => {
        if let Ok(cancel) = serde_json::from_slice::<CancelRequest>(body)
          && let Some(token) = self.0.get(&cancel.params.id)
        {
          token.cancel();
        }
      }
      (Some(_), Some(id)) => {
        self.0.entry(id).or_default();
      }
      _ => {}
    }
  }
}

struct PendingGuard {
  pending: Arc<PendingRequests>,
  id: Id,
}

impl Drop for PendingGuard {
  fn drop(&mut self) {
    self.pending.0.remove(&self.id);
  }
}

struct CancellableService<S> {
  inner: S,
  pending: Arc<PendingRequests>,
}

impl<S> Service<Request> for CancellableService<S>
where
  S: Service<Request, Response = Option<Response>>,
  S::Future: Send + 'static,
{
  type Response = Option<Response>;
  type Error = S::Error;
  type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request) -> Self::Future {
    let Some(id) = request.id().cloned() else {
      return self.inner.call(request).boxed();
    };
    let token = self.pending.token(&id);
    let guard = PendingGuard {
      pending: self.pending.clone(),
      id,
    };
    let future =
      REQUEST_TOKEN.sync_scope(token.clone(), || self.inner.call(request));
    REQUEST_TOKEN
      .scope(token, async move {
        let _guard = guard;
        future.await
      })
      .boxed()
  }
}

struct RawMessage {
  bytes: Vec<u8>,
  body_start: Option<usize>,
}

impl RawMessage {
  fn body(&self) -> Option<&[u8]> {
    self.body_start.map(|start| &self.bytes[start..])
  }
}

fn read_message(reader: &mut impl BufRead) -> io::Result<Option<RawMessage>> {
  let mut bytes = Vec::new();
  let mut content_length = None;
  loop {
    let line_start = bytes.len();
    if reader.read_until(b'\n', &mut bytes)? == 0 {
      if bytes.is_empty() {
        return Ok(None);
      }
      return Ok(Some(RawMessage {
        bytes,
        body_start: None,
      }));
    }
    let line = bytes[line_start..].trim_ascii();
    if line.is_empty() {
      break;
    }
    if let Some(colon) = line.iter().position(|b| *b == b':')
      && line[..colon].eq_ignore_ascii_case(b"content-length")
    {
      content_length = std::str::from_utf8(line[colon + 1..].trim_ascii())
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    }
  }
  let Some(content_length) = content_length else {
    return Ok(Some(RawMessage {
      bytes,
      body_start: None,
    }));
  };
  let body_start = bytes.len();
  bytes.resize(body_start + content_length, 0);
  reader.read_exact(&mut bytes[body_start..])?;
  Ok(Some(RawMessage {
    bytes,
    body_start: Some(body_start),
  }))
}

fn spawn_stdin_reader(
  pending: Arc<PendingRequests>,
) -> io::Result<impl tokio::io::AsyncRead + Unpin> {
  let (tx, mut rx) = mpsc::unbounded_channel::<io::Result<Bytes>>();
  std::thread::Builder::new()
    .name("lsp-stdin".to_string())
    .spawn(move || {
      let mut stdin = io::stdin().lock();
      loop {
        let message = match read_message(&mut stdin) {
          Ok(Some(message)) => message,
          Ok(None) => break,
          Err(err) => {
            let _ = tx.send(Err(err));
            break;
          }
        };
        if let Some(body) = message.body() {
          pending.observe(body);
        }
        if tx.send(Ok(Bytes::from(message.bytes))).is_err() {
          break;
        }
      }
    })?;
  Ok(StreamReader::new(stream::poll_fn(move |cx| {
    rx.poll_recv(cx)
  })))
}

#[cfg(test)]
mod tests {
  use std::io::Cursor;

  use super::*;

  fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
  }

  #[test]
  fn reads_framed_messages() {
    let first = r#"{"jsonrpc":"2.0","id":1,"method":"a"}"#;
    let second = r#"{"jsonrpc":"2.0","method":"b"}"#;
    let input = format!(
      "{}Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n{}",
      frame(first),
      frame(second)
    );
    let mut reader = Cursor::new(input.clone().into_bytes());
    let mut output = Vec::new();
    let mut bodies = Vec::new();
    while let Some(message) = read_message(&mut reader).unwrap() {
      bodies.push(message.body().map(|b| b.to_vec()));
      output.extend_from_slice(&message.bytes);
    }
    assert_eq!(output, input.into_bytes());
    assert_eq!(
      bodies,
      vec![
        Some(first.as_bytes().to_vec()),
        Some(second.as_bytes().to_vec())
      ]
    );
  }

  #[test]
  fn forwards_headers_without_content_length() {
    let mut reader = Cursor::new(b"Foo: bar\r\n\r\n".to_vec());
    let message = read_message(&mut reader).unwrap().unwrap();
    assert_eq!(message.bytes, b"Foo: bar\r\n\r\n");
    assert!(message.body().is_none());
    assert!(read_message(&mut reader).unwrap().is_none());
  }

  #[test]
  fn cancels_observed_requests() {
    let pending = PendingRequests::default();
    pending
      .observe(br#"{"jsonrpc":"2.0","id":7,"method":"textDocument/hover"}"#);
    let token = pending.token(&Id::Number(7));
    assert!(!token.is_cancelled());
    pending.observe(
      br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":7}}"#,
    );
    assert!(token.is_cancelled());
  }

  #[test]
  fn ignores_client_responses() {
    let pending = PendingRequests::default();
    pending.observe(br#"{"jsonrpc":"2.0","id":3,"result":null}"#);
    assert!(pending.0.is_empty());
  }
}
