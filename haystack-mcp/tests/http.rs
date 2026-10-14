#![cfg(feature = "http")]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar},
};
use haystack_mcp::{AuthError, HttpConfig, JwtConfig, JwtVerifier, Report, VerificationKey};
use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
const VERSION: &str = "2026-07-28";
const ISSUER: &str = "https://issuer.example.test";
const SCOPE: &str = "haystack:read";
fn verifier(resource: &str) -> Arc<JwtVerifier> {
    Arc::new(
        JwtVerifier::new(JwtConfig {
            issuer: ISSUER.into(),
            resource: resource.into(),
            required_scope: SCOPE.into(),
            subjects: vec!["alice".into(), "bob".into()],
            keys: vec![VerificationKey {
                kid: "fixture".into(),
                public_key_der: include_bytes!("fixtures/test-public.der").to_vec(),
            }],
            revoked_jti: vec!["revoked".into()],
        })
        .unwrap(),
    )
}
fn claims(resource: &str) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    json!({"iss":ISSUER,"aud":resource,"sub":"alice","scope":SCOPE,"iat":now-1,"exp":now+3600,"client_id":"fixture","jti":"accepted"})
}
fn signed_raw(header: &str, claims: &str) -> String {
    let key = RsaKeyPair::from_pkcs8(include_bytes!("fixtures/test-key.pk8")).unwrap();
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(claims)
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}
fn token(claims: &Value) -> String {
    signed_raw(
        r#"{"alg":"RS256","typ":"at+jwt","kid":"fixture"}"#,
        &claims.to_string(),
    )
}
#[test]
fn offline_verifier_rejects_each_invalid_credential_and_rechecks_revocation() {
    let resource = "http://127.0.0.1:12345/mcp";
    let verifier = verifier(resource);
    let good = claims(resource);
    assert_eq!(
        verifier.verify(&token(&good)),
        Ok(Principal::authenticated("alice", vec![SCOPE.into()]))
    );
    for (field, value) in [
        ("iss", json!("https://wrong.example")),
        ("aud", json!("http://127.0.0.1:1/mcp")),
        ("exp", json!(1)),
        ("exp", json!(1.5)),
        ("iat", json!(u64::MAX)),
        ("nbf", json!(u64::MAX)),
        ("jti", json!("revoked")),
        ("client_id", json!(null)),
        ("sub", json!(7)),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert_eq!(
            verifier.verify(&token(&bad)),
            Err(AuthError::InvalidToken),
            "{field}"
        );
    }
    for field in ["iss", "aud", "exp", "iat", "jti", "sub", "client_id"] {
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert_eq!(
            verifier.verify(&token(&bad)),
            Err(AuthError::InvalidToken),
            "missing {field}"
        );
    }
    for (field, value) in [
        ("alg", "none"),
        ("alg", "HS256"),
        ("typ", "JWT"),
        ("kid", "unknown"),
    ] {
        let mut header = json!({"alg":"RS256","typ":"at+jwt","kid":"fixture"});
        header[field] = json!(value);
        assert_eq!(
            verifier.verify(&signed_raw(&header.to_string(), &good.to_string())),
            Err(AuthError::InvalidToken)
        );
    }
    for field in ["jku", "x5u", "crit"] {
        let mut header = json!({"alg":"RS256","typ":"at+jwt","kid":"fixture"});
        header[field] = json!("https://untrusted.invalid");
        assert_eq!(
            verifier.verify(&signed_raw(&header.to_string(), &good.to_string())),
            Err(AuthError::InvalidToken)
        );
    }
    let duplicate = good.to_string().replacen('{', "{\"sub\":\"bob\",", 1);
    assert_eq!(
        verifier.verify(&signed_raw(
            r#"{"alg":"RS256","typ":"at+jwt","kid":"fixture"}"#,
            &duplicate
        )),
        Err(AuthError::InvalidToken)
    );
    let mut altered = token(&good).into_bytes();
    let i = altered.len() - 10;
    altered[i] = if altered[i] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        verifier.verify(std::str::from_utf8(&altered).unwrap()),
        Err(AuthError::InvalidToken)
    );
    for (field, value) in [("scope", json!("other")), ("sub", json!("mallory"))] {
        let mut bad = good.clone();
        bad[field] = value;
        assert_eq!(
            verifier.verify(&token(&bad)),
            Err(AuthError::InsufficientScope)
        );
    }
    let mut array = good.clone();
    array["aud"] = json!(["other", resource]);
    assert!(verifier.verify(&token(&array)).is_ok());
    verifier.revoke("accepted").unwrap();
    assert_eq!(verifier.verify(&token(&good)), Err(AuthError::InvalidToken));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_scope_grammar_is_validated_before_granting_authority() {
    let fixture = Fixture::start(|_| {}).await;
    let body = request("tools/list", json!({})).to_string();
    for scope in [
        "haystack:read \n",
        "haystack:read bad\0scope",
        "haystack:read bad\"scope",
        "haystack:read bad\\scope",
        "haystack:read café",
        " haystack:read",
        "haystack:read ",
        "haystack:read  other",
        "",
    ] {
        let mut invalid = claims(fixture.verifier.resource());
        invalid["scope"] = json!(scope);
        let signed = token(&invalid);
        assert_eq!(
            fixture.verifier.verify(&signed),
            Err(AuthError::InvalidToken),
            "malformed signed scope {scope:?}"
        );
        let reply = fixture.raw("POST", "/mcp", &signed, &body, &[], &[]).await;
        assert_eq!(reply.status, 401, "malformed signed scope {scope:?}");
        assert!(reply.headers.contains("invalid_token"));
    }
    for scope in [Some("other another"), None] {
        let mut insufficient = claims(fixture.verifier.resource());
        if let Some(scope) = scope {
            insufficient["scope"] = json!(scope);
        } else {
            insufficient.as_object_mut().unwrap().remove("scope");
        }
        let reply = fixture
            .raw("POST", "/mcp", &token(&insufficient), &body, &[], &[])
            .await;
        assert_eq!(reply.status, 403);
        assert!(reply.headers.contains("insufficient_scope"));
    }
    for invalid_scope in [
        json!(null),
        json!(7),
        json!([SCOPE]),
        json!("x".repeat(1025)),
    ] {
        let mut invalid = claims(fixture.verifier.resource());
        invalid["scope"] = invalid_scope;
        let reply = fixture
            .raw("POST", "/mcp", &token(&invalid), &body, &[], &[])
            .await;
        assert_eq!(reply.status, 401);
        assert!(reply.headers.contains("invalid_token"));
    }
    let mut multi = claims(fixture.verifier.resource());
    multi["scope"] = json!("other haystack:read !#$%&'()*+,-./:;<=>?@[]^_`{|}~");
    assert_eq!(
        fixture
            .raw("POST", "/mcp", &token(&multi), &body, &[], &[])
            .await
            .status,
        200
    );
    assert_eq!(fixture.close().await.admitted, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legal_media_negotiation_reaches_sdk_without_losing_header_semantics() {
    let fixture = Fixture::start(|_| {}).await;
    let body = request("tools/list", json!({})).to_string();
    for (replace, extra) in [
        (
            vec![("Accept", "application/json")],
            vec![("Accept", "text/event-stream")],
        ),
        (
            vec![("Accept", "APPLICATION/JSON, TEXT/EVENT-STREAM")],
            vec![],
        ),
        (vec![("Content-Type", "Application/JSON")], vec![]),
        (
            vec![("Accept", "application/json;Q=1, text/event-stream;Q=0.5")],
            vec![],
        ),
    ] {
        let reply = fixture
            .raw("POST", "/mcp", &fixture.token(), &body, &replace, &extra)
            .await;
        assert_eq!(reply.status, 200, "{replace:?} {extra:?}");
        assert_eq!(reply.json()["result"]["tools"].as_array().unwrap().len(), 2);
    }
    for accept in [
        "application/json;Q=0, text/event-stream",
        "application/json;q=0.000, text/event-stream",
        "application/json;q=NaN, text/event-stream",
        "application/json;q=-1, text/event-stream",
        "application/json;q=1.001, text/event-stream",
        "application/json;q=0.1234, text/event-stream",
        "application/json;garbage, text/event-stream",
        "application/json;q=1;q=0, text/event-stream",
        "xapplication/json, text/event-stream",
    ] {
        assert_eq!(
            fixture
                .raw(
                    "POST",
                    "/mcp",
                    &fixture.token(),
                    &body,
                    &[("Accept", accept)],
                    &[]
                )
                .await
                .status,
            406,
            "malformed or unacceptable {accept:?}"
        );
    }
    assert_eq!(fixture.close().await.admitted, 4);
}
struct Gate {
    blocked: AtomicBool,
    entered: AtomicUsize,
    epoch: AtomicUsize,
    evaluated: AtomicUsize,
}
struct Unblock(Arc<Gate>);
impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.blocked.store(false, Ordering::SeqCst);
    }
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            blocked: AtomicBool::new(false),
            entered: AtomicUsize::new(0),
            epoch: AtomicUsize::new(0),
            evaluated: AtomicUsize::new(0),
        })
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.blocked.store(false, Ordering::SeqCst);
    }
}
struct Policy(Arc<Gate>);
struct Scope {
    subject: String,
    epoch: usize,
    gate: Arc<Gate>,
}
impl ReadPolicy for Policy {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        let Principal::Authenticated {
            subject,
            permissions,
        } = principal
        else {
            return Err(ReadError::Forbidden);
        };
        assert_eq!(permissions, &[SCOPE.to_owned()]);
        self.0.entered.fetch_add(1, Ordering::SeqCst);
        while self.0.blocked.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let epoch = self.0.epoch.load(Ordering::SeqCst);
        if epoch == 99 {
            return Err(ReadError::Forbidden);
        }
        Ok(Arc::new(Scope {
            subject: subject.clone(),
            epoch,
            gate: self.0.clone(),
        }))
    }
}
impl PolicySnapshot for Scope {
    fn scope_key(&self) -> &str {
        if self.epoch == 0 {
            "scope-0"
        } else {
            "scope-1"
        }
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        false
    }
    fn operation(&self, operation: ReadOperation) -> bool {
        operation == ReadOperation::Read
    }
    fn entity(&self, id: &str) -> bool {
        self.gate.evaluated.fetch_add(1, Ordering::SeqCst);
        id != "denied" && (self.subject == "alice" || id == "b")
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, target: &str) -> bool {
        self.entity(target)
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
struct Fixture {
    owner: ApplicationOwner,
    gate: Arc<Gate>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<Report, haystack_mcp::McpError>>,
    address: std::net::SocketAddr,
    verifier: Arc<JwtVerifier>,
}
impl Fixture {
    async fn start(configure: impl FnOnce(&mut HttpConfig)) -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        for id in ["a", "b", "denied"] {
            let mut row = HDict::new();
            row.set("id", Kind::Ref(HRef::from_val(id)));
            row.set("site", Kind::Marker);
            row.set("secret", Kind::Str("NEVER_DISCLOSE".into()));
            row.set("target", Kind::Ref(HRef::from_val("denied")));
            graph.add(row).unwrap();
        }
        let gate = Gate::new();
        let owner =
            ApplicationBuilder::new(graph, Arc::new(Policy(gate.clone())), ReadLimits::default())
                .unwrap()
                .start(&tokio::runtime::Handle::current())
                .unwrap();
        owner.ready().await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut config = HttpConfig::local(address);
        configure(&mut config);
        let verifier = verifier(&config.resource);
        let stop = CancellationToken::new();
        let task = tokio::spawn(haystack_mcp::serve_http(
            owner.handle(),
            listener,
            config,
            verifier.clone(),
            stop.clone(),
        ));
        Self {
            owner,
            gate,
            stop,
            task,
            address,
            verifier,
        }
    }
    fn token(&self) -> String {
        token(&claims(self.verifier.resource()))
    }
    async fn close(self) -> Report {
        self.gate.blocked.store(false, Ordering::SeqCst);
        self.stop.cancel();
        let report = tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.admitted, report.released);
        assert_eq!(self.owner.handle().outstanding_tasks(), 0);
        assert!(TcpListener::bind(self.address).await.is_ok());
        let read = self
            .owner
            .handle()
            .read_service()
            .read(
                ReadContext::new(
                    Principal::authenticated("alice", vec![SCOPE.into()]),
                    Instant::now() + Duration::from_secs(2),
                    CancellationToken::new(),
                ),
                ReadRequest {
                    query: ReadQuery::Ids(vec!["a".into()]),
                    projection: vec![],
                    page_size: 1,
                    cursor: None,
                    profile: OutputProfile::Typed,
                },
            )
            .await;
        if self.gate.epoch.load(Ordering::SeqCst) == 99 {
            assert!(matches!(read, Err(ReadError::Forbidden)));
        } else {
            assert!(
                read.is_ok(),
                "borrowed application must remain usable: {read:?}"
            );
        }
        self.owner.close().await.unwrap();
        assert!(self.owner.terminated().await.cleanup_errors.is_empty());
        report
    }
    async fn post(&self, value: Value) -> Reply {
        self.raw("POST", "/mcp", &self.token(), &value.to_string(), &[], &[])
            .await
    }
    async fn raw(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: &str,
        replace: &[(&str, &str)],
        extra: &[(&str, &str)],
    ) -> Reply {
        let mut stream = TcpStream::connect(self.address).await.unwrap();
        stream
            .write_all(&wire(
                self.address,
                method,
                path,
                token,
                body,
                replace,
                extra,
            ))
            .await
            .unwrap();
        read_reply(&mut stream).await
    }
}
fn request(method: &str, params: Value) -> Value {
    let mut params = params;
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":VERSION,"io.modelcontextprotocol/clientCapabilities":{}});
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}
fn wire(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: &str,
    replace: &[(&str, &str)],
    extra: &[(&str, &str)],
) -> Vec<u8> {
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let auth = format!("Bearer {token}");
    let host = address.to_string();
    let length = body.len().to_string();
    let mut headers = vec![
        ("Host", host.as_str()),
        ("Authorization", auth.as_str()),
        ("Content-Type", "application/json"),
        ("Accept", "application/json, text/event-stream"),
        ("MCP-Protocol-Version", VERSION),
        ("Mcp-Method", value["method"].as_str().unwrap_or("ping")),
        ("Content-Length", length.as_str()),
    ];
    if let Some(name) = value["params"]["name"].as_str() {
        headers.push(("Mcp-Name", name));
    }
    for (name, value) in replace {
        headers.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        if !value.is_empty() {
            headers.push((name, value));
        }
    }
    headers.extend_from_slice(extra);
    let mut wire = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        wire.push_str(&format!("{name}: {value}\r\n"));
    }
    wire.push_str("\r\n");
    wire.push_str(body);
    wire.into_bytes()
}
struct Reply {
    status: u16,
    headers: String,
    body: Vec<u8>,
}
impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!(
                "{} {:?} {:?}",
                self.status,
                self.headers,
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}
async fn read_reply(stream: &mut TcpStream) -> Reply {
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let offset = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response headers");
    let headers = String::from_utf8(bytes[..offset].to_vec()).unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    Reply {
        status,
        headers,
        body: bytes[offset + 4..].to_vec(),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovery_tools_scopes_and_principal_bound_cursor() {
    let fixture = Fixture::start(|_| {}).await;
    let discovery = fixture.post(request("server/discover", json!({}))).await;
    assert_eq!(discovery.status, 200);
    assert_eq!(
        discovery.json()["result"]["supportedVersions"],
        json!([VERSION])
    );
    assert!(!discovery.headers.to_lowercase().contains("mcp-session-id"));
    let listed = fixture.post(request("tools/list", json!({}))).await;
    assert_eq!(listed.status, 200);
    assert_eq!(
        listed.json()["result"]["tools"].as_array().unwrap().len(),
        2
    );
    let first = fixture
        .post(request(
            "tools/call",
            json!({"name":"haystack_query","arguments":{"filter":"site","page_size":1}}),
        ))
        .await;
    assert_eq!(first.status, 200);
    let page = first.json()["result"]["structuredContent"].clone();
    assert_eq!(page["ok"], true);
    assert_eq!(page["complete"], false);
    assert!(!String::from_utf8_lossy(&first.body).contains("NEVER_DISCLOSE"));
    let next = request(
        "tools/call",
        json!({"name":"haystack_query","arguments":{"filter":"site","page_size":1,"cursor":page["cursor"]}}),
    );
    let mut bob = claims(fixture.verifier.resource());
    bob["sub"] = json!("bob");
    let cross = fixture
        .raw("POST", "/mcp", &token(&bob), &next.to_string(), &[], &[])
        .await;
    assert_eq!(
        cross.json()["result"]["structuredContent"]["error"]["code"],
        "stale_cursor"
    );
    let next = fixture.post(next).await;
    assert_eq!(next.json()["result"]["structuredContent"]["complete"], true);
    fixture.gate.epoch.store(99, Ordering::SeqCst);
    let denied = fixture
        .post(request(
            "tools/call",
            json!({"name":"haystack_read","arguments":{"ids":["a"]}}),
        ))
        .await;
    assert_eq!(
        denied.json()["result"]["structuredContent"]["error"]["code"],
        "forbidden"
    );
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authentication_metadata_and_http_boundary_matrix() {
    let fixture = Fixture::start(|_| {}).await;
    let body = request("tools/list", json!({})).to_string();
    let good = fixture.token();
    let missing = fixture
        .raw("POST", "/mcp", &good, &body, &[("Authorization", "")], &[])
        .await;
    assert_eq!(missing.status, 401);
    assert!(missing.headers.contains("resource_metadata=\""));
    assert!(!missing.headers.contains("invalid_token"));
    let metadata = fixture
        .raw(
            "GET",
            "/.well-known/oauth-protected-resource/mcp",
            &good,
            "",
            &[],
            &[],
        )
        .await;
    assert_eq!(metadata.status, 200);
    assert_eq!(metadata.json()["resource"], fixture.verifier.resource());
    for (field, value, status) in [
        ("aud", json!("bad"), 401),
        ("iss", json!("bad"), 401),
        ("exp", json!(1), 401),
        ("jti", json!("revoked"), 401),
        ("scope", json!("other"), 403),
        ("sub", json!("unadmitted"), 403),
    ] {
        let mut bad = claims(fixture.verifier.resource());
        bad[field] = value;
        let result = fixture
            .raw("POST", "/mcp", &token(&bad), &body, &[], &[])
            .await;
        assert_eq!(result.status, status, "{field}");
        assert!(result.body.is_empty());
        assert!(result.headers.contains(if status == 401 {
            "invalid_token"
        } else {
            "insufficient_scope"
        }));
    }
    for (method, path, replace, extra, status) in [
        ("GET", "/mcp", vec![], vec![], 405),
        ("DELETE", "/mcp", vec![], vec![], 405),
        ("POST", "/mcp?access_token=x", vec![], vec![], 400),
        ("POST", "/mcp", vec![("Host", "evil.test")], vec![], 403),
        (
            "POST",
            "/mcp",
            vec![],
            vec![("Forwarded", "host=evil.test")],
            403,
        ),
        ("POST", "/mcp", vec![], vec![("Origin", "null")], 403),
        (
            "POST",
            "/mcp",
            vec![],
            vec![("Origin", "https://evil.test")],
            403,
        ),
        (
            "POST",
            "/mcp",
            vec![],
            vec![("Authorization", "Bearer x")],
            400,
        ),
        (
            "POST",
            "/mcp",
            vec![],
            vec![("MCP-Protocol-Version", VERSION)],
            400,
        ),
        ("POST", "/mcp", vec![("Mcp-Method", "ping")], vec![], 400),
        ("POST", "/mcp", vec![("Mcp-Method", "")], vec![], 400),
        (
            "POST",
            "/mcp",
            vec![("Accept", "xapplication/json, xtext/event-stream")],
            vec![],
            406,
        ),
        (
            "POST",
            "/mcp",
            vec![("Accept", "application/json;q=0, text/event-stream")],
            vec![],
            406,
        ),
        (
            "POST",
            "/mcp",
            vec![("Content-Type", "text/plain")],
            vec![],
            415,
        ),
    ] {
        let result = fixture
            .raw(method, path, &good, &body, &replace, &extra)
            .await;
        assert_eq!(
            result.status, status,
            "{method} {path} {replace:?} {extra:?}"
        );
    }
    assert_eq!(fixture.gate.entered.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.post(request("tools/list", json!({}))).await.status,
        200
    );
    fixture.verifier.revoke("accepted").unwrap();
    assert_eq!(
        fixture.post(request("tools/list", json!({}))).await.status,
        401
    );
    fixture.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_envelopes_metadata_names_and_input_bounds() {
    let fixture = Fixture::start(|config| config.limits.max_frame_bytes = 1024).await;
    let good = fixture.token();
    for value in [
        json!([]),
        json!({"jsonrpc":"2.0","id":1,"result":{}}),
        json!({"jsonrpc":"2.0","id":{},"method":"ping"}),
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled"}),
    ] {
        let reply = fixture
            .raw("POST", "/mcp", &good, &value.to_string(), &[], &[])
            .await;
        assert_eq!(reply.status, 400);
        assert_eq!(reply.json()["error"]["code"], -32600);
        if value["id"] != 1 {
            assert!(reply.json().get("id").is_none());
        }
    }
    let mut value = request("tools/list", json!({}));
    value["params"] = json!({});
    let reply = fixture.post(value).await;
    assert_eq!(reply.json()["error"]["code"], -32602);
    let mut value = request("tools/list", json!({}));
    value["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2025-11-25");
    let reply = fixture
        .raw(
            "POST",
            "/mcp",
            &good,
            &value.to_string(),
            &[("MCP-Protocol-Version", "2025-11-25")],
            &[],
        )
        .await;
    assert_eq!(reply.json()["error"]["code"], -32022);
    assert_eq!(reply.json()["error"]["data"]["supported"], json!([VERSION]));
    let unknown = fixture.post(request("initialize", json!({}))).await;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json()["error"]["code"], -32601);
    let call = request(
        "tools/call",
        json!({"name":"haystack_read","arguments":{"ids":["a"]}}),
    );
    let mismatch = fixture
        .raw(
            "POST",
            "/mcp",
            &good,
            &call.to_string(),
            &[("Mcp-Name", "wrong")],
            &[],
        )
        .await;
    assert_eq!(mismatch.json()["error"]["code"], -32020);
    let encoded = format!(
        "=?base64?{}?=",
        base64::engine::general_purpose::STANDARD.encode("haystack_read")
    );
    let reply = fixture
        .raw(
            "POST",
            "/mcp",
            &good,
            &call.to_string(),
            &[("Mcp-Name", &encoded)],
            &[],
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["result"]["structuredContent"]["ok"], true);
    let duplicate = call.to_string().replacen('{', "{\"id\":2,", 1);
    assert_eq!(
        fixture
            .raw("POST", "/mcp", &good, &duplicate, &[], &[])
            .await
            .status,
        400
    );
    assert_eq!(
        fixture
            .raw("POST", "/mcp", &good, &"x".repeat(1025), &[], &[])
            .await
            .status,
        413
    );
    let deep = format!("{}0{}", "[".repeat(33), "]".repeat(33));
    assert_eq!(
        fixture
            .raw("POST", "/mcp", &good, &deep, &[], &[])
            .await
            .status,
        400
    );
    fixture.close().await;
}
async fn wait_for(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("fixture state never reached");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_deadline_and_stop_retain_real_worker_until_joined() {
    for event in ["disconnect", "deadline", "stop"] {
        let fixture = Fixture::start(|config| {
            config.limits.max_in_flight = 1;
            config.limits.request_timeout = Duration::from_millis(600);
        })
        .await;
        fixture.gate.blocked.store(true, Ordering::SeqCst);
        let _unblock = Unblock(fixture.gate.clone());
        let mut peer = TcpStream::connect(fixture.address).await.unwrap();
        let body = request(
            "tools/call",
            json!({"name":"haystack_read","arguments":{"ids":["a"]}}),
        )
        .to_string();
        peer.write_all(&wire(
            fixture.address,
            "POST",
            "/mcp",
            &fixture.token(),
            &body,
            &[],
            &[],
        ))
        .await
        .unwrap();
        wait_for(|| fixture.gate.entered.load(Ordering::SeqCst) == 1).await;
        assert_eq!(fixture.owner.handle().read_service().load().admitted, 1);
        let saturated = fixture.post(request("tools/list", json!({}))).await;
        assert_eq!(saturated.status, 503);
        if event == "disconnect" {
            drop(peer);
        } else if event == "stop" {
            fixture.stop.cancel();
            let mut byte = [0; 1];
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), peer.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        } else {
            tokio::time::sleep(Duration::from_millis(700)).await;
            let mut byte = [0; 1];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
        }
        if event == "stop" {
            tokio::time::sleep(Duration::from_millis(2300)).await;
        }
        // Even cancellation and peer EOF cannot release a physically held worker.
        assert_eq!(fixture.owner.handle().read_service().load().admitted, 1);
        assert!(!fixture.task.is_finished());
        fixture.gate.blocked.store(false, Ordering::SeqCst);
        wait_for(|| fixture.owner.handle().read_service().load().admitted == 0).await;
        assert_eq!(
            fixture.gate.evaluated.load(Ordering::SeqCst),
            0,
            "cancelled worker evaluated data: {event}"
        );
        let report = fixture.close().await;
        assert_eq!(report.admitted, 1);
        assert_eq!(report.released, 1);
        assert_eq!(report.suppressed, 1, "{event}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn official_sdk_client_uses_modern_http_without_sessions() {
    for profile in [
        haystack_mcp::OutputProfile::H4,
        haystack_mcp::OutputProfile::Jeto,
    ] {
        sdk_profile(profile).await;
    }
}
async fn sdk_profile(profile: haystack_mcp::OutputProfile) {
    use rmcp::{
        model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
        service::{ClientLifecycleMode, serve_client_with_lifecycle},
        transport::{
            StreamableHttpClientTransport,
            streamable_http_client::StreamableHttpClientTransportConfig,
        },
    };
    let fixture = Fixture::start(|config| config.output_profile = profile).await;
    if profile == haystack_mcp::OutputProfile::Jeto {
        let graph = fixture
            .owner
            .handle()
            .read_service()
            .native_graph()
            .unwrap();
        let mut row = graph.get("a").unwrap();
        row.set("large", Kind::Int(i64::MIN));
        row.set("buf", Kind::Buf(vec![0, 255, 251]));
        graph.update("a", row).unwrap();
    }
    let transport = StreamableHttpClientTransport::with_client(
        reqwest::Client::new(),
        StreamableHttpClientTransportConfig::with_uri(fixture.verifier.resource().to_owned())
            .auth_header(fixture.token()),
    );
    let client = serve_client_with_lifecycle(
        ClientConfig::default(),
        transport,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .unwrap();
    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 2);
    for tool in &tools.tools {
        assert_eq!(
            tool.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert!(tool.output_schema.is_some());
    }
    let mut arguments = json!({"ids":["a"]});
    if profile == haystack_mcp::OutputProfile::Jeto {
        arguments["profile"] = json!(haystack_mcp::JETO_PROFILE);
    }
    let result = client
        .call_tool(
            CallToolRequestParams::new("haystack_read")
                .with_arguments(arguments.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let value = result.structured_content.unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["row_count"], 1);
    if profile == haystack_mcp::OutputProfile::Jeto {
        assert_eq!(
            value["grid"]["rows"][0]["large"]["val"],
            "-9223372036854775808"
        );
        assert_eq!(value["grid"]["rows"][0]["buf"]["val"], "AP_7");
        assert_eq!(
            value["catalog"]["revision"],
            "873b922451d3ef4c0c9c08ef3daa542f352d69f3"
        );
    }
    client.cancel().await.unwrap();
    let report = fixture.close().await;
    assert_eq!(report.admitted, 3);
}

#[tokio::test]
async fn rich_http_acknowledgment_precedes_read_admission() {
    let fixture =
        Fixture::start(|config| config.output_profile = haystack_mcp::OutputProfile::Jeto).await;
    for arguments in [
        json!({"ids":["a"]}),
        json!({"ids":["a"],"profile":"haystack-h4-json-v1"}),
        json!({"ids":["a"],"profile":null}),
    ] {
        let reply = fixture
            .post(request(
                "tools/call",
                json!({"name":"haystack_read","arguments":arguments}),
            ))
            .await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["error"]["code"], -32602);
    }
    assert_eq!(fixture.gate.entered.load(Ordering::SeqCst), 0);
    let reply = fixture.post(request("tools/call", json!({"name":"haystack_query","arguments":{"filter":"site","profile":haystack_mcp::JETO_PROFILE}}))).await;
    let response = reply.json();
    let page = &response["result"]["structuredContent"];
    assert_eq!(page["ok"], true);
    assert_eq!(page["row_count"], 2);
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap(),
        *page
    );
    assert!(!page.to_string().contains("NEVER_DISCLOSE"));
    assert!(!page.to_string().contains("denied"));
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_headers_body_and_connection_admission_have_absolute_bounds() {
    for partial_body in [false, true] {
        let fixture = Fixture::start(|config| {
            config.max_connections = 1;
            config.limits.request_timeout = Duration::from_millis(180);
        })
        .await;
        let mut held = TcpStream::connect(fixture.address).await.unwrap();
        if partial_body {
            let body = request("tools/list", json!({})).to_string();
            let bytes = wire(
                fixture.address,
                "POST",
                "/mcp",
                &fixture.token(),
                &body,
                &[],
                &[],
            );
            held.write_all(&bytes[..bytes.len() - 1]).await.unwrap();
        } else {
            held.write_all(b"POST /mcp HTTP/1.1\r\nHost:")
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut excess = TcpStream::connect(fixture.address).await.unwrap();
        let mut byte = [0; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), excess.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), held.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(fixture.gate.entered.load(Ordering::SeqCst), 0);
        let report = fixture.close().await;
        assert_eq!(report.admitted, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_socket_delivery_keeps_native_lease_until_deadline_or_stop() {
    for event in ["deadline", "stop"] {
        let fixture = Fixture::start(|config| {
            config.limits.max_response_bytes = 16 * 1024 * 1024;
            config.limits.request_timeout = Duration::from_millis(1200);
        })
        .await;
        let graph = fixture
            .owner
            .handle()
            .read_service()
            .native_graph()
            .unwrap();
        let mut row = graph.get("a").unwrap();
        row.set("large", Kind::Str("\u{1}".repeat(150_000)));
        graph.update("a", row).unwrap();
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(1024).unwrap();
        let mut peer = socket.connect(fixture.address).await.unwrap();
        let body = request(
            "tools/call",
            json!({"name":"haystack_read","arguments":{"ids":["a"]}}),
        )
        .to_string();
        peer.write_all(&wire(
            fixture.address,
            "POST",
            "/mcp",
            &fixture.token(),
            &body,
            &[],
            &[],
        ))
        .await
        .unwrap();
        let mut prefix = [0; 32];
        tokio::time::timeout(Duration::from_secs(1), peer.read_exact(&mut prefix))
            .await
            .unwrap()
            .unwrap();
        assert!(prefix.starts_with(b"HTTP/1.1 200"));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            fixture.owner.handle().read_service().load().admitted,
            0,
            "worker already finished; only physical response is blocked"
        );
        if fixture.owner.handle().outstanding_tasks() <= 1 {
            let mut rest = Vec::new();
            peer.read_to_end(&mut rest).await.unwrap();
            let mut all = prefix.to_vec();
            all.extend(rest);
            let offset = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let value: Value = serde_json::from_slice(&all[offset..]).unwrap();
            panic!(
                "physical output not blocked: bytes={}, ok={}, error={}",
                all.len(),
                value["result"]["structuredContent"]["ok"],
                value["result"]["structuredContent"]["error"]
            );
        }
        if event == "stop" {
            fixture.stop.cancel();
        } else {
            wait_for(|| fixture.owner.handle().outstanding_tasks() == 1).await;
        }
        let report = fixture.close().await;
        assert_eq!(report.admitted, 1);
        assert_eq!(report.suppressed, 1);
        let mut remainder = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), peer.read_to_end(&mut remainder))
            .await
            .unwrap()
            .unwrap();
        assert!(
            remainder.len() < 1_900_000,
            "response unexpectedly completed through undrained socket"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_close_stops_listener_and_waits_for_held_work() {
    let fixture = Fixture::start(|_| {}).await;
    fixture.gate.blocked.store(true, Ordering::SeqCst);
    let _unblock = Unblock(fixture.gate.clone());
    let mut peer = TcpStream::connect(fixture.address).await.unwrap();
    let body = request(
        "tools/call",
        json!({"name":"haystack_read","arguments":{"ids":["a"]}}),
    )
    .to_string();
    peer.write_all(&wire(
        fixture.address,
        "POST",
        "/mcp",
        &fixture.token(),
        &body,
        &[],
        &[],
    ))
    .await
    .unwrap();
    wait_for(|| fixture.gate.entered.load(Ordering::SeqCst) == 1).await;
    let closing = fixture.owner.close();
    tokio::pin!(closing);
    tokio::select! {result=&mut closing=>panic!("close completed before held worker: {result:?}"),_=tokio::time::sleep(Duration::from_millis(80))=>{}}
    assert!(!fixture.task.is_finished());
    assert_eq!(fixture.owner.handle().read_service().load().admitted, 1);
    let mut byte = [0; 1];
    assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
    fixture.gate.blocked.store(false, Ordering::SeqCst);
    closing.await.unwrap();
    assert!(fixture.owner.terminated().await.cleanup_errors.is_empty());
    let report = fixture.task.await.unwrap().unwrap();
    assert_eq!(report.admitted, report.released);
    assert_eq!(report.suppressed, 1);
    assert_eq!(fixture.owner.handle().outstanding_tasks(), 0);
    assert!(TcpListener::bind(fixture.address).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_callers_with_the_same_wire_id_keep_independent_authority() {
    let fixture = Fixture::start(|_| {}).await;
    fixture.gate.blocked.store(true, Ordering::SeqCst);
    let _unblock = Unblock(fixture.gate.clone());
    let mut alice = TcpStream::connect(fixture.address).await.unwrap();
    let mut bob = TcpStream::connect(fixture.address).await.unwrap();
    let body = request(
        "tools/call",
        json!({"name":"haystack_read","arguments":{"ids":["a","b"]}}),
    )
    .to_string();
    let mut bob_claims = claims(fixture.verifier.resource());
    bob_claims["sub"] = json!("bob");
    alice
        .write_all(&wire(
            fixture.address,
            "POST",
            "/mcp",
            &fixture.token(),
            &body,
            &[],
            &[],
        ))
        .await
        .unwrap();
    bob.write_all(&wire(
        fixture.address,
        "POST",
        "/mcp",
        &token(&bob_claims),
        &body,
        &[],
        &[],
    ))
    .await
    .unwrap();
    wait_for(|| fixture.gate.entered.load(Ordering::SeqCst) == 2).await;
    fixture.gate.blocked.store(false, Ordering::SeqCst);
    let (alice, bob) = tokio::join!(read_reply(&mut alice), read_reply(&mut bob));
    assert_eq!(alice.status, 200);
    assert_eq!(bob.status, 200);
    assert_eq!(alice.json()["id"], 1);
    assert_eq!(bob.json()["id"], 1);
    assert_eq!(alice.json()["result"]["structuredContent"]["row_count"], 2);
    assert_eq!(bob.json()["result"]["structuredContent"]["row_count"], 1);
    let report = fixture.close().await;
    assert_eq!(report.admitted, 2);
    assert_eq!(report.peak_in_flight, 2);
}
