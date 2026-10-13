#[derive(clap::Subcommand)]
pub enum Action {
    /// Serve one bounded MCP session over Unix pipe stdin/stdout
    Stdio {
        /// Version 1 TOML containing the dataset, principal and entity scope
        #[arg(long, value_name = "FILE")]
        config: std::path::PathBuf,
    },
}
pub fn run(action: Action) {
    #[cfg(feature = "mcp")]
    let result = match action {
        Action::Stdio { config } => selected::run(&config),
    };
    #[cfg(not(feature = "mcp"))]
    let result: Result<(), String> = {
        let _ = action;
        Err("MCP support is not installed; rebuild rusty-haystack-cli with --features mcp".into())
    };
    if let Err(error) = result {
        eprintln!("MCP error: {error}");
        std::process::exit(1);
    }
}

#[cfg(feature = "mcp")]
mod selected {
    use haystack_app::*;
    use haystack_core::{
        graph::{EntityGraph, SharedGraph},
        kinds::NominalScalar,
    };
    use serde::Deserialize;
    use std::{
        collections::BTreeSet,
        io::Read,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        version: u32,
        dataset_file: PathBuf,
        dataset_format: String,
        principal: String,
        entity_ids: Vec<String>,
        #[serde(default)]
        hidden_tags: Vec<String>,
        #[serde(default = "timeout")]
        request_timeout_ms: u64,
        #[serde(default = "response_bytes")]
        max_response_bytes: usize,
    }
    fn timeout() -> u64 {
        10_000
    }
    fn response_bytes() -> usize {
        1024 * 1024
    }
    #[derive(Clone)]
    struct Scope {
        principal: Principal,
        ids: BTreeSet<String>,
        hidden: BTreeSet<String>,
    }
    impl ReadPolicy for Scope {
        fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            if *principal != self.principal {
                return Err(ReadError::Forbidden);
            }
            Ok(Arc::new(self.clone()))
        }
    }
    impl PolicySnapshot for Scope {
        fn scope_key(&self) -> &str {
            "immutable-launch-scope-v1"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            false
        }
        fn operation(&self, operation: ReadOperation) -> bool {
            operation == ReadOperation::Read
        }
        fn entity(&self, id: &str) -> bool {
            self.ids.contains(id)
        }
        fn tag(&self, _: &str, tag: &str) -> bool {
            !self.hidden.contains(tag)
        }
        fn reference(&self, target: &str) -> bool {
            self.ids.contains(target)
        }
        fn reference_display(&self, target: &str) -> bool {
            self.ids.contains(target)
        }
        fn catalog(&self, _: CatalogKind, _: &str) -> bool {
            false
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            false
        }
    }
    fn bounded(path: &Path, max: usize) -> Result<String, String> {
        if !std::fs::metadata(path)
            .map_err(|_| "configuration or dataset is unreadable")?
            .is_file()
        {
            return Err("configuration and dataset must be regular files".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A path replaced with a FIFO between metadata and open must not
            // block before descriptor validation. Regular files ignore this.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| "configuration or dataset is unreadable")?;
        if !file
            .metadata()
            .map_err(|_| "configuration or dataset is unreadable")?
            .is_file()
        {
            return Err("configuration and dataset must be regular files".into());
        }
        let mut bytes = Vec::new();
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "configuration or dataset is unreadable")?;
        if bytes.len() > max {
            return Err("configuration or dataset exceeds its byte limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "configuration or dataset is not UTF-8".into())
    }
    pub fn run(path: &Path) -> Result<(), String> {
        let config: Config =
            toml::from_str(&bounded(path, 65_536)?).map_err(|_| "invalid MCP configuration")?;
        if config.version != 1
            || config.principal.is_empty()
            || config.principal.len() > 256
            || config.entity_ids.len() > 512
            || config.entity_ids.is_empty()
            || config
                .entity_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 256)
            || config.hidden_tags.len() > 512
            || config
                .hidden_tags
                .iter()
                .any(|tag| tag.is_empty() || tag.len() > 256)
            || !(10..=60_000).contains(&config.request_timeout_ms)
            || !(8192..=16 * 1024 * 1024).contains(&config.max_response_bytes)
        {
            return Err("invalid MCP configuration".into());
        }
        let mime = match config.dataset_format.as_str() {
            "zinc" => "text/zinc",
            "trio" => "text/trio",
            "json" => "application/json",
            _ => return Err("unsupported MCP dataset format".into()),
        };
        let file = if config.dataset_file.is_absolute() {
            config.dataset_file.clone()
        } else {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&config.dataset_file)
        };
        let content = bounded(&file, 4 * 1024 * 1024)?;
        // Conservative pre-parser container bound also counts delimiters in
        // strings. A launch file can choose a different spelling if necessary.
        let mut depth = 0usize;
        for byte in content.bytes() {
            match byte {
                b'[' | b'{' | b'<' => {
                    depth += 1;
                    if depth > 64 {
                        return Err("dataset nesting exceeds its limit".into());
                    }
                }
                b']' | b'}' | b'>' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let grid = haystack_core::codecs::codec_for(mime)
            .expect("selected codec")
            .decode_grid(&content)
            .map_err(|_| "invalid MCP dataset")?;
        if grid.rows.len() > 10_000 {
            return Err("dataset row count exceeds its limit".into());
        }
        let graph = EntityGraph::from_grid(&grid, None).map_err(|_| "invalid MCP entity graph")?;
        let principal = Principal::TrustedEmbedding {
            subject: config.principal,
        };
        let scope = Arc::new(Scope {
            principal: principal.clone(),
            ids: config.entity_ids.into_iter().collect(),
            hidden: config.hidden_tags.into_iter().collect(),
        });
        let limits = haystack_mcp::Limits {
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            max_response_bytes: config.max_response_bytes,
            ..Default::default()
        };
        let builder =
            ApplicationBuilder::new(SharedGraph::new(graph), scope, ReadLimits::default())
                .map_err(|_| "invalid MCP application configuration")?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .enable_all()
            .build()
            .map_err(|_| "cannot create MCP runtime")?;
        let result = runtime.block_on(session(builder, principal, limits));
        drop(runtime);
        result
    }
    #[cfg(unix)]
    async fn session(
        builder: ApplicationBuilder,
        principal: Principal,
        limits: haystack_mcp::Limits,
    ) -> Result<(), String> {
        use std::os::fd::AsFd;
        use tokio::{
            net::unix::pipe::{Receiver, Sender},
            signal::unix::{SignalKind, signal},
        };
        let input = std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .and_then(Receiver::from_owned_fd)
            .map_err(|_| "MCP stdio requires Unix pipe stdin")?;
        let output = std::io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .and_then(Sender::from_owned_fd)
            .map_err(|_| "MCP stdio requires Unix pipe stdout")?;
        let mut terminate =
            signal(SignalKind::terminate()).map_err(|_| "cannot register termination signal")?;
        let mut interrupt =
            signal(SignalKind::interrupt()).map_err(|_| "cannot register interrupt signal")?;
        let owner = builder
            .start(&tokio::runtime::Handle::current())
            .map_err(|_| "MCP application startup failed")?;
        if owner.ready().await.is_err() {
            owner.terminated().await;
            return Err("MCP application startup failed".into());
        }
        let stop = CancellationToken::new();
        let serving = haystack_mcp::serve(
            owner.handle(),
            principal,
            input,
            output,
            limits,
            stop.clone(),
        );
        tokio::pin!(serving);
        let result = tokio::select! {
            result = &mut serving => result,
            _ = terminate.recv() => { stop.cancel(); serving.await },
            _ = interrupt.recv() => { stop.cancel(); serving.await },
        };
        let close = owner.close().await;
        let terminated = owner.terminated().await;
        if close.is_err() || terminated.close.is_err() || !terminated.cleanup_errors.is_empty() {
            return Err("MCP application cleanup failed".into());
        }
        let report = result.map_err(|error| error.to_string())?;
        if report.admitted != report.released || owner.handle().outstanding_tasks() != 0 {
            return Err("MCP work cleanup incomplete".into());
        }
        eprintln!("MCP session terminated; all admitted work released");
        Ok(())
    }
    #[cfg(not(unix))]
    async fn session(
        _: ApplicationBuilder,
        _: Principal,
        _: haystack_mcp::Limits,
    ) -> Result<(), String> {
        Err("MCP stdio requires the Unix pipe platform profile".into())
    }
}
