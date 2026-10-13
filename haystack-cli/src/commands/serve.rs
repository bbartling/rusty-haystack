use haystack_app::ApplicationOwner;
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::auth::users::load_users_from_toml;
use haystack_server::cors::CorsPolicy;

#[cfg(feature = "postgres")]
mod postgres;

pub struct ServeConfig<'a> {
    pub port: u16,
    pub file: Option<&'a str>,
    pub users_file: Option<&'a str>,
    pub host: Option<&'a str>,
    pub demo: bool,
    pub cors_origins: Vec<String>,
    pub postgres_config: Option<&'a str>,
}

pub fn run(cfg: ServeConfig<'_>) {
    env_logger::init();
    let result = tokio::runtime::Runtime::new()
        .map_err(|_| "failed to create runtime".to_string())
        .and_then(|rt| {
            let result = rt.block_on(serve(cfg));
            // Drop outside async only after owned work and cleanup terminated.
            drop(rt);
            result
        });
    if let Err(error) = result {
        eprintln!("Server error: {error}");
        std::process::exit(1);
    }
}

async fn serve(cfg: ServeConfig<'_>) -> Result<(), String> {
    #[cfg(not(feature = "postgres"))]
    if cfg.postgres_config.is_some() {
        return Err("PostgreSQL support is not installed; rebuild rusty-haystack-cli with --features postgres".into());
    }
    let auth = if let Some(path) = cfg.users_file {
        // TOML and hash parser diagnostics can include credential material.
        let users = load_users_from_toml(path).map_err(|_| "invalid or unreadable users file")?;
        if cfg.postgres_config.is_some() && users.is_empty() {
            return Err("PostgreSQL serving requires a nonempty --users file".into());
        }
        AuthManager::new(users, std::time::Duration::from_secs(3600))
    } else {
        if cfg.postgres_config.is_some() {
            return Err("PostgreSQL serving requires a nonempty --users file".into());
        }
        AuthManager::empty()
    };
    let cors = if cfg.cors_origins.is_empty() {
        CorsPolicy::Disabled
    } else {
        CorsPolicy::Allow(cfg.cors_origins.clone())
    };
    let owner = start(&cfg, auth, cors).await?;
    let ready = match owner.ready().await {
        Ok(ready) => ready,
        Err(error) => {
            owner.terminated().await;
            return Err(error.to_string());
        }
    };
    println!("Listening on {}", ready.listeners[0].address);
    let signal = tokio::select! {
        result = shutdown_signal() => result,
        report = owner.terminated() => return terminated(report),
    };
    let closed = owner.close().await;
    if let Err(error) = &closed {
        eprintln!("Application close: {error}; waiting for owned cleanup to finish");
    }
    // A close timeout is failure, not completion; keep driving late cleanup.
    let report = owner.terminated().await;
    signal.map_err(|error| error.to_string())?;
    closed.map_err(|error| error.to_string())?;
    terminated(report)
}

fn terminated(report: haystack_app::TerminationReport) -> Result<(), String> {
    if !report.cleanup_errors.is_empty() {
        return Err("application resource cleanup failed".into());
    }
    report.close.map(|_| ()).map_err(|error| error.to_string())
}

async fn start(
    cfg: &ServeConfig<'_>,
    auth: AuthManager,
    cors: CorsPolicy,
) -> Result<ApplicationOwner, String> {
    #[cfg(feature = "postgres")]
    if let Some(path) = cfg.postgres_config {
        return postgres::start(path, cfg.host.unwrap_or("127.0.0.1"), cfg.port, auth, cors).await;
    }
    let ns = std::sync::Arc::new(
        DefNamespace::load_standard().map_err(|e| format!("loading ontology: {e}"))?,
    );
    let graph = if let Some(path) = cfg.file {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("reading entity file: {e}"))?;
        let mime = if path.ends_with(".trio") {
            "text/trio"
        } else if path.ends_with(".json") {
            "application/json"
        } else {
            "text/zinc"
        };
        let codec = haystack_core::codecs::codec_for(mime).ok_or("unsupported entity format")?;
        let grid = codec
            .decode_grid(&content)
            .map_err(|e| format!("decoding entities: {e}"))?;
        EntityGraph::from_grid(&grid, Some(ns.clone()))
            .map_err(|e| format!("building graph: {e}"))?
    } else {
        let mut graph = EntityGraph::with_namespace(ns.clone());
        if cfg.demo {
            for entity in haystack_server::demo::demo_entities() {
                graph
                    .add(entity)
                    .map_err(|e| format!("adding demo entity: {e}"))?;
            }
        }
        graph
    };
    HaystackServer::new(SharedGraph::new(graph))
        .with_namespace((*ns).clone())
        .with_auth(auth)
        .with_cors(cors)
        .host(cfg.host.unwrap_or("127.0.0.1"))
        .port(cfg.port)
        .start()
        .map_err(|error| error.to_string())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
