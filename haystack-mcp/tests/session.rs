#![cfg(feature = "stdio")]
use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar},
};
use haystack_mcp::{Limits, McpError, Report};
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
    service::{ClientLifecycleMode, serve_client_with_lifecycle},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
};

#[derive(Clone)]
struct Policy(Arc<AtomicUsize>);
struct Scope(usize, String);
impl ReadPolicy for Policy {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        assert_eq!(*principal, principal_for_test());
        let epoch = self.0.load(Ordering::SeqCst);
        if epoch == 99 {
            return Err(ReadError::Forbidden);
        }
        Ok(Arc::new(Scope(epoch, format!("scope-{epoch}"))))
    }
}
impl PolicySnapshot for Scope {
    fn scope_key(&self) -> &str {
        &self.1
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        false
    }
    fn operation(&self, operation: ReadOperation) -> bool {
        operation == ReadOperation::Read && self.0 != 99
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied"
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, target: &str) -> bool {
        target != "denied"
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        false
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        false
    }
}
fn principal_for_test() -> Principal {
    Principal::TrustedEmbedding {
        subject: "fixture".into(),
    }
}
fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    for id in ["a", "b", "denied"] {
        let mut row = HDict::new();
        row.set(
            "id",
            Kind::Ref(HRef::new(id, Some("hidden display".into()))),
        );
        row.set("site", Kind::Marker);
        row.set("dis", Kind::Str(format!("entity {id}")));
        row.set("secret", Kind::Str("NEVER_DISCLOSE".into()));
        row.set("target", Kind::Ref(HRef::from_val("denied")));
        graph.add(row).unwrap();
    }
    graph
}
async fn app(graph: SharedGraph, limits: ReadLimits) -> (ApplicationOwner, Arc<AtomicUsize>) {
    let epoch = Arc::new(AtomicUsize::new(0));
    let owner = ApplicationBuilder::new(graph, Arc::new(Policy(epoch.clone())), limits)
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    (owner, epoch)
}
async fn close(owner: ApplicationOwner) {
    owner.close().await.unwrap();
    assert!(owner.terminated().await.cleanup_errors.is_empty());
    assert_eq!(owner.handle().outstanding_tasks(), 0);
}
struct Wire {
    input: BufReader<ReadHalf<DuplexStream>>,
    output: WriteHalf<DuplexStream>,
}
impl Wire {
    async fn send(&mut self, value: Value) {
        self.output
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), self.input.read_line(&mut line))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        serde_json::from_str(&line).unwrap()
    }
    async fn call(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
        self.receive().await
    }
}
fn wire(
    owner: &ApplicationOwner,
    limits: Limits,
    bytes: usize,
) -> (Wire, tokio::task::JoinHandle<Result<Report, McpError>>) {
    let (client, server) = tokio::io::duplex(bytes);
    let (input, output) = tokio::io::split(server);
    let application = owner.handle();
    let task = tokio::spawn(haystack_mcp::serve(
        application,
        principal_for_test(),
        input,
        output,
        limits,
        CancellationToken::new(),
    ));
    let (input, output) = tokio::io::split(client);
    (
        Wire {
            input: BufReader::new(input),
            output,
        },
        task,
    )
}
async fn initialize(wire: &mut Wire) {
    wire.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire","version":"1"}}})).await;
    let result = wire.receive().await;
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(result["result"]["capabilities"], json!({"tools":{}}));
    wire.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
}
fn envelope(response: &Value) -> Value {
    let result = &response["result"];
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
    assert_eq!(text["profile"], "haystack-h4-json-v1");
    text
}
async fn finished(wire: Wire, task: tokio::task::JoinHandle<Result<Report, McpError>>) -> Report {
    drop(wire);
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.admitted, report.released);
    report
}

#[tokio::test]
async fn released_sdk_client_discovers_and_reads_modern_profile() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (client, server) = tokio::io::duplex(1024 * 1024);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        input,
        output,
        Limits::default(),
        CancellationToken::new(),
    ));
    let client = serve_client_with_lifecycle(
        ClientConfig::default(),
        client,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .unwrap();
    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 2);
    assert_eq!(tools.tools[0].name, "haystack_query");
    assert_eq!(tools.tools[1].name, "haystack_read");
    for tool in tools.tools {
        assert_eq!(tool.annotations.unwrap().read_only_hint, Some(true));
        assert!(tool.output_schema.is_some());
    }
    let result = client
        .call_tool(
            CallToolRequestParams::new("haystack_read")
                .with_arguments(json!({"ids":["a"]}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let value = result.structured_content.unwrap();
    assert_eq!(value["row_count"], 1);
    assert_eq!(value["ok"], true);
    assert!(
        client
            .peer_info()
            .is_some_and(|info| info.protocol_version == ProtocolVersion::V_2026_07_28)
    );
    client.cancel().await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.admitted, report.released);
    owner.ready().await.unwrap();
    assert_eq!(
        owner
            .handle()
            .read_service()
            .read(
                ReadContext::with_timeout(principal_for_test(), Duration::from_secs(1)),
                ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
            )
            .await
            .unwrap()
            .row_count,
        1
    );
    close(owner).await;
}

#[tokio::test]
async fn raw_legacy_scope_paging_reauthorization_and_strict_errors() {
    let graph = graph();
    let (owner, epoch) = app(graph.clone(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 1024 * 1024);
    initialize(&mut wire).await;
    let first = envelope(
        &wire
            .call(1, "haystack_query", json!({"filter":"site","page_size":1}))
            .await,
    );
    assert_eq!(first["row_count"], 1);
    assert_eq!(first["complete"], false);
    let cursor = first["cursor"].clone();
    assert!(!first.to_string().contains("NEVER_DISCLOSE"));
    assert!(!first.to_string().contains("hidden display"));
    assert!(!first.to_string().contains("denied"));
    let second = envelope(
        &wire
            .call(
                2,
                "haystack_query",
                json!({"filter":"site","page_size":1,"cursor":cursor}),
            )
            .await,
    );
    assert_eq!(second["complete"], true);
    assert_eq!(second["row_count"], 1);
    let missing = envelope(
        &wire
            .call(3, "haystack_read", json!({"ids":["denied","missing"]}))
            .await,
    );
    assert_eq!(missing["row_count"], 0);
    assert_eq!(missing["ok"], true);
    for args in [
        json!({"ids":["a"],"principal":"admin"}),
        json!({"ids":["a"],"path":"/private"}),
        json!({"ids":"a"}),
    ] {
        assert_eq!(
            wire.call(4, "haystack_read", args).await["error"]["code"],
            -32602
        );
    }
    assert_eq!(
        wire.call(4, "write", json!({})).await["error"]["code"],
        -32602
    );
    wire.send(json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}))
        .await;
    assert_eq!(wire.receive().await["error"]["code"], -32601);
    let page = envelope(
        &wire
            .call(5, "haystack_query", json!({"filter":"site","page_size":1}))
            .await,
    );
    epoch.store(1, Ordering::SeqCst);
    assert_eq!(
        envelope(
            &wire
                .call(
                    6,
                    "haystack_query",
                    json!({"filter":"site","page_size":1,"cursor":page["cursor"]})
                )
                .await
        )["error"]["code"],
        "stale_cursor"
    );
    let page = envelope(
        &wire
            .call(7, "haystack_query", json!({"filter":"site","page_size":1}))
            .await,
    );
    let mut row = graph.get("a").unwrap();
    row.set("dis", Kind::Str("changed".into()));
    graph.update("a", row).unwrap();
    assert_eq!(
        envelope(
            &wire
                .call(
                    8,
                    "haystack_query",
                    json!({"filter":"site","page_size":1,"cursor":page["cursor"]})
                )
                .await
        )["error"]["code"],
        "stale_cursor"
    );
    epoch.store(99, Ordering::SeqCst);
    assert_eq!(
        envelope(&wire.call(9, "haystack_read", json!({"ids":["a"]})).await)["error"]["code"],
        "forbidden"
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn rich_nested_projection_and_application_input_output_limits() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("rich", Kind::List(vec![Kind::List(vec![Kind::Int(42)])]));
    graph.update("a", row).unwrap();
    let (owner, _) = app(
        graph,
        ReadLimits {
            max_ids: 2,
            max_rows: 2,
            ..ReadLimits::default()
        },
    )
    .await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            max_response_bytes: 8192,
            ..Limits::default()
        },
        64 * 1024,
    );
    initialize(&mut wire).await;
    assert_eq!(
        envelope(&wire.call(1, "haystack_read", json!({"ids":["a"]})).await)["error"]["code"],
        "projection"
    );
    assert_eq!(
        envelope(
            &wire
                .call(2, "haystack_read", json!({"ids":["a"],"projection":["id"]}))
                .await
        )["ok"],
        true
    );
    assert_eq!(
        envelope(
            &wire
                .call(3, "haystack_read", json!({"ids":["a","b","c"]}))
                .await
        )["error"]["code"],
        "budget"
    );
    assert_eq!(
        envelope(
            &wire
                .call(4, "haystack_read", json!({"ids":["a"],"page_size":3}))
                .await
        )["error"]["code"],
        "budget"
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn framing_depth_ids_and_slow_output_fail_closed_and_release_work() {
    for attack in [
        vec![b'x'; 1025],
        format!("{}0{}\n", "[".repeat(10), "]".repeat(10)).into_bytes(),
    ] {
        let (owner, _) = app(graph(), ReadLimits::default()).await;
        let (mut wire, task) = wire(
            &owner,
            Limits {
                max_frame_bytes: 1024,
                max_json_depth: 4,
                ..Limits::default()
            },
            8192,
        );
        wire.output.write_all(&attack).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
            McpError::Framing
        );
        assert_eq!(owner.handle().outstanding_tasks(), 0);
        close(owner).await;
    }
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            request_timeout: Duration::from_millis(100),
            ..Limits::default()
        },
        64,
    );
    wire.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire","version":"1"}}})).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(owner.handle().outstanding_tasks() > 0);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        McpError::Deadline
    );
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[test]
fn cancelled_work_keeps_quota_and_id_until_real_cleanup_then_allows_reuse() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (owner, _) = runtime.block_on(app(graph(), ReadLimits::default()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime.block_on(async {
        let (mut wire,task)=wire(&owner,Limits {max_in_flight:1,..Limits::default()},64*1024); initialize(&mut wire).await;
        wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
        tokio::time::timeout(Duration::from_secs(1),async { while owner.handle().read_service().load().admitted==0 {tokio::task::yield_now().await;} }).await.unwrap();
        wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}})).await;
        let duplicate=wire.call(7,"haystack_read",json!({"ids":["a"]})).await;
        let pressure=wire.call(8,"haystack_read",json!({"ids":["a"]})).await;
        let retained=(owner.handle().read_service().load().admitted,owner.handle().outstanding_tasks());
        release_tx.send(()).unwrap(); blocker.await.unwrap();
        assert_eq!(duplicate["error"]["data"]["reason"],"duplicate_request_id"); assert_eq!(pressure["error"]["data"]["reason"],"capacity"); assert_eq!(retained.0,1); assert!(retained.1>=2);
        tokio::time::timeout(Duration::from_secs(2),async { while owner.handle().outstanding_tasks()>0 {tokio::task::yield_now().await;} }).await.unwrap();
        let reused=envelope(&wire.call(7,"haystack_read",json!({"ids":["a"]})).await); assert_eq!(reused["row_count"],1);
        let report=finished(wire,task).await; assert_eq!(report.peak_in_flight,1); assert_eq!(report.suppressed,1); close(owner).await;
    });
}

#[tokio::test]
async fn oversized_ids_and_full_result_budget_are_explicit_and_bounded() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("large", Kind::Str("x".repeat(4000)));
    graph.update("a", row).unwrap();
    let (owner, _) = app(graph, ReadLimits::default()).await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            max_response_bytes: 8192,
            ..Limits::default()
        },
        64 * 1024,
    );
    initialize(&mut wire).await;
    wire.send(json!({"jsonrpc":"2.0","id":"x".repeat(129),"method":"tools/list"}))
        .await;
    let rejected = wire.receive().await;
    assert!(rejected.get("id").is_none());
    assert_eq!(rejected["error"]["data"]["reason"], "request_id_too_large");
    assert_eq!(
        envelope(&wire.call(1, "haystack_read", json!({"ids":["a"]})).await)["error"]["code"],
        "output_budget"
    );
    assert_eq!(
        envelope(
            &wire
                .call(2, "haystack_read", json!({"ids":["a"],"projection":["id"]}))
                .await
        )["row_count"],
        1
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn cancellation_interrupts_a_partially_delivered_blocked_response() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("large", Kind::Str("x".repeat(4000)));
    graph.update("a", row).unwrap();
    let (owner, _) = app(graph, ReadLimits::default()).await;
    let (mut wire, mut task) = wire(&owner, Limits::default(), 512);
    initialize(&mut wire).await;
    wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
    let mut prefix = [0u8; 32];
    tokio::time::timeout(Duration::from_secs(2), wire.input.read_exact(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    // A received prefix proves physical delivery began. Leave the remaining
    // output blocked and cancel over the independent input half.
    wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}))
        .await;
    let prompt = tokio::time::timeout(Duration::from_millis(300), &mut task).await;
    drop(wire);
    if prompt.is_err() {
        let _ = task.await;
    }
    assert!(
        prompt.is_ok(),
        "cancellation must interrupt blocked physical delivery"
    );
    assert!(
        prompt.unwrap().unwrap().is_err(),
        "a partially delivered frame cannot leave a reusable connection"
    );
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[derive(Default)]
struct WriteGate {
    blocked: std::sync::atomic::AtomicBool,
    polled: tokio::sync::Notify,
    waker: std::sync::Mutex<Option<std::task::Waker>>,
}
impl WriteGate {
    fn release(&self) {
        self.blocked.store(false, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}
struct GatedWriter {
    inner: WriteHalf<DuplexStream>,
    gate: Arc<WriteGate>,
}
impl tokio::io::AsyncWrite for GatedWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.gate.blocked.load(Ordering::SeqCst) {
            *self.gate.waker.lock().unwrap() = Some(cx.waker().clone());
            self.gate.polled.notify_one();
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
fn gated_wire(
    owner: &ApplicationOwner,
    cancelled: CancellationToken,
) -> (
    Wire,
    Arc<WriteGate>,
    tokio::task::JoinHandle<Result<Report, McpError>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(server);
    let gate = Arc::new(WriteGate::default());
    let task = tokio::spawn(haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        input,
        GatedWriter {
            inner: output,
            gate: gate.clone(),
        },
        Limits {
            max_in_flight: 1,
            ..Limits::default()
        },
        cancelled,
    ));
    let (input, output) = tokio::io::split(client);
    (
        Wire {
            input: BufReader::new(input),
            output,
        },
        gate,
        task,
    )
}
async fn block_response(wire: &mut Wire, gate: &WriteGate) {
    gate.blocked.store(true, Ordering::SeqCst);
    wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
    tokio::time::timeout(Duration::from_secs(2), gate.polled.notified())
        .await
        .unwrap();
}
#[tokio::test]
async fn cancellation_after_zero_byte_pending_suppresses_and_reuses_id() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, gate, mut task) = gated_wire(&owner, CancellationToken::new());
    initialize(&mut wire).await;
    block_response(&mut wire, &gate).await;
    wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}))
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while owner.handle().outstanding_tasks() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(30), &mut task).await {
        close(owner).await;
        panic!("zero-byte cancellation closed the reusable session: {result:?}");
    }
    gate.release();
    assert_eq!(
        envelope(&wire.call(7, "haystack_read", json!({"ids":["a"]})).await)["row_count"],
        1
    );
    let report = finished(wire, task).await;
    assert_eq!(report.suppressed, 1);
    assert_eq!(report.delivered, 2);
    close(owner).await;
}
#[tokio::test]
async fn clean_eof_and_external_stop_join_a_blocked_response_successfully() {
    for external in [false, true] {
        let (owner, _) = app(graph(), ReadLimits::default()).await;
        let cancelled = CancellationToken::new();
        let (mut wire, gate, task) = gated_wire(&owner, cancelled.clone());
        initialize(&mut wire).await;
        block_response(&mut wire, &gate).await;
        if external {
            cancelled.cancel();
        } else {
            wire.output.shutdown().await.unwrap();
        }
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner.handle().outstanding_tasks(), 0);
        owner.ready().await.unwrap();
        let usable = owner
            .handle()
            .read_service()
            .read(
                ReadContext::with_timeout(principal_for_test(), Duration::from_secs(1)),
                ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed),
            )
            .await
            .unwrap();
        assert_eq!(usable.row_count, 1);
        close(owner).await;
        let report = outcome.expect("a requested stop is successful after joined cleanup");
        assert_eq!(report.admitted, report.released);
        assert_eq!(
            report.delivered, 1,
            "interrupted response was not delivered"
        );
    }
}
#[tokio::test]
async fn jsonrpc_error_classes_echo_valid_ids_and_leave_session_usable() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
    initialize(&mut wire).await;
    let mut mismatches = Vec::new();
    for (frame, id, code) in [
        ("{\n".to_owned(), Value::Null, -32700),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":4}\n".to_owned(),
            json!(4),
            -32600,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":42}}\n"
                .to_owned(),
            json!(5),
            -32602,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{}}\n".to_owned(),
            json!(6),
            -32602,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"unavailable\",\"params\":{}}\n".to_owned(),
            json!(8),
            -32601,
        ),
    ] {
        wire.output.write_all(frame.as_bytes()).await.unwrap();
        let response = wire.receive().await;
        let expected_id = if id.is_null() { None } else { Some(&id) };
        if response["error"]["code"] != code || response.get("id") != expected_id {
            mismatches.push((id, code, response));
        }
    }
    assert_eq!(
        envelope(&wire.call(9, "haystack_read", json!({"ids":["a"]})).await)["row_count"],
        1
    );
    finished(wire, task).await;
    close(owner).await;
    assert!(
        mismatches.is_empty(),
        "wrong JSON-RPC error classes or IDs: {mismatches:?}"
    );
}
#[tokio::test]
async fn an_unfinished_input_frame_keeps_its_real_deadline() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            request_timeout: Duration::from_millis(100),
            ..Limits::default()
        },
        64 * 1024,
    );
    initialize(&mut wire).await;
    wire.output.write_all(b"{\"jsonrpc\":").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        McpError::Deadline
    );
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[tokio::test]
async fn a_real_framing_failure_survives_blocked_writer_cleanup() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, gate, task) = gated_wire(&owner, CancellationToken::new());
    initialize(&mut wire).await;
    block_response(&mut wire, &gate).await;
    wire.output
        .write_all(&vec![b'x'; Limits::default().max_frame_bytes + 1])
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err(), McpError::Framing);
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[tokio::test]
async fn invalid_present_ids_are_rejected_before_notification_fallback() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
    initialize(&mut wire).await;
    let mut rejected = 0;
    let mut silently_ignored = Vec::new();
    for (index, id) in [
        json!(true),
        json!({}),
        json!([]),
        Value::Null,
        json!(1.5),
        json!(1.0),
        json!(u64::MAX),
        json!(-9223372036854775809.0),
    ]
    .into_iter()
    .enumerate()
    {
        wire.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
        // A later valid request is a processing barrier, so the RED case
        // observes silent consumption without waiting for a guessed timeout.
        let probe = format!("probe-{index}");
        wire.send(json!({"jsonrpc":"2.0","id":probe,"method":"ping"}))
            .await;
        let first = wire.receive().await;
        if first["id"] == probe {
            silently_ignored.push(id);
        } else {
            assert_eq!(first["error"]["code"], -32600, "{first}");
            assert!(
                first.get("id").is_none(),
                "MCP unidentified errors omit the ID: {first}"
            );
            rejected += 1;
            assert_eq!(wire.receive().await["id"], probe);
        }
    }
    // Truly absent IDs remain notifications: initialized, unselected, and
    // cancellation of an unknown request are consumed without a response.
    for notification in [
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","method":"fixture/notification","params":{"value":true}}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"unknown"}}),
    ] {
        wire.send(notification).await;
    }
    let response = wire.call(99, "haystack_read", json!({"ids":["a"]})).await;
    assert_eq!(response["id"], 99);
    assert_eq!(envelope(&response)["row_count"], 1);
    let report = finished(wire, task).await;
    assert_eq!(
        report.admitted, 10,
        "invalid IDs and notifications must not be admitted"
    );
    assert_eq!(
        report.delivered,
        10 + rejected,
        "genuine notifications must not produce responses"
    );
    close(owner).await;
    assert!(
        silently_ignored.is_empty(),
        "invalid present IDs were consumed as notifications: {silently_ignored:?}"
    );
}

#[tokio::test]
async fn unidentified_errors_omit_ids_before_and_after_each_selected_lifecycle() {
    for modern in [false, true] {
        let (owner, _) = app(graph(), ReadLimits::default()).await;
        let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
        wire.output.write_all(b"{\n").await.unwrap();
        let early = wire.receive().await;
        assert_eq!(early["error"]["code"], -32700);
        assert!(early.get("id").is_none());
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{},
            "io.modelcontextprotocol/clientInfo":{"name":"wire","version":"1"},
        });
        if modern {
            wire.send(json!({"jsonrpc":"2.0","id":"discover","method":"server/discover","params":{"_meta":meta}})).await;
            let discover = wire.receive().await;
            assert_eq!(discover["id"], "discover");
            assert!(
                discover["result"]["supportedVersions"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("2026-07-28"))
            );
        } else {
            initialize(&mut wire).await;
        }
        let mut params = json!({"name":"haystack_read","arguments":{"ids":["a"]}});
        if modern {
            params["_meta"] = meta;
        }
        // A valid call selects the modern lifecycle after its bootstrap probe.
        wire.send(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}))
            .await;
        let first = wire.receive().await;
        assert_eq!(first["id"], 1);
        assert_eq!(envelope(&first)["row_count"], 1);
        wire.send(json!({"jsonrpc":"2.0","id":null,"method":"tools/call","params":params}))
            .await;
        let invalid = wire.receive().await;
        assert_eq!(invalid["error"]["code"], -32600);
        assert!(invalid.get("id").is_none());
        wire.send(
            json!({"jsonrpc":"2.0","id":"after-error","method":"tools/call","params":params}),
        )
        .await;
        let recovered = wire.receive().await;
        assert_eq!(recovered["id"], "after-error");
        assert_eq!(envelope(&recovered)["row_count"], 1);
        finished(wire, task).await;
        close(owner).await;
    }
}
