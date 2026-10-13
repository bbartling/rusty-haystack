//! Standalone selection: authenticated, coarse permissions over one durable dataset.
use haystack_app::{
    ApplicationOwner, CatalogKind, FunctionIdentity, HistoryMutationPolicy,
    HistoryOperationIdentity, HistorySchema, HistoryWriteRequest, MutationPolicy, PolicySnapshot,
    Principal, ReadError, ReadOperation, ReadPolicy,
    postgres::{self, PostgresConfig, PostgresLimits, PostgresPolicies, PostgresTransport},
};
use haystack_core::{
    codecs::entity::{EntityBatchRequest, OperationIdentity},
    graph::{EntityGraph, PreparedChange, SharedGraph},
    kinds::NominalScalar,
    xeto::catalog::Catalog,
};
use haystack_server::{HaystackServer, auth::AuthManager, cors::CorsPolicy};
use serde::Deserialize;
use std::{io::Read, path::Path, sync::Arc};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    host: String,
    port: u16,
    database: String,
    username: String,
    schema: String,
    password_env: String,
    transport: Transport,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
enum Transport {
    VerifyFull { additional_ca_file: Option<String> },
    LoopbackPlaintext,
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| "unreadable PostgreSQL configuration or CA file")?;
    if bytes.len() as u64 > limit {
        return Err("PostgreSQL configuration or CA file exceeds size limit".into());
    }
    Ok(bytes)
}

fn load(path: &Path) -> Result<PostgresConfig, String> {
    let bytes = read_bounded(path, 65_536)?;
    let source =
        std::str::from_utf8(&bytes).map_err(|_| "invalid PostgreSQL TOML configuration")?;
    // Do not return TOML errors: they can quote credentials or complete source lines.
    let config: Configuration =
        toml::from_str(source).map_err(|_| "invalid PostgreSQL TOML configuration")?;
    if config.password_env.is_empty()
        || !config
            .password_env
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("invalid PostgreSQL password environment variable name".into());
    }
    let password = std::env::var(&config.password_env)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or("PostgreSQL password environment variable is missing, empty or non-Unicode")?;
    let transport = match config.transport {
        Transport::LoopbackPlaintext => PostgresTransport::LoopbackPlaintext,
        Transport::VerifyFull { additional_ca_file } => PostgresTransport::VerifyFull {
            additional_ca_pem: additional_ca_file
                .map(|file| {
                    // Relative CA paths belong to this configuration's directory.
                    read_bounded(
                        &path.parent().unwrap_or(Path::new(".")).join(file),
                        1024 * 1024,
                    )
                })
                .transpose()?,
        },
    };
    Ok(PostgresConfig {
        host: config.host,
        port: config.port,
        database: config.database,
        username: config.username,
        password: Some(password),
        schema: config.schema,
        transport,
        limits: PostgresLimits::default(),
    })
}

pub(super) async fn start(
    path: &str,
    host: &str,
    port: u16,
    auth: AuthManager,
    cors: CorsPolicy,
) -> Result<ApplicationOwner, String> {
    let config = load(Path::new(path))?;
    let catalog = Catalog::load_http_pinned().map_err(|_| "cannot load pinned HTTP catalog")?;
    let builder = postgres::application(
        config,
        catalog,
        PostgresPolicies {
            reads: Arc::new(Permissions),
            entities: Arc::new(Permissions),
            history: Arc::new(Permissions),
        },
        Default::default(),
    )
    .await
    .map_err(|error| error.to_string())?;
    // The server replaces this unused graph with the application's read-only view.
    // Do not install a namespace: the durable authority owns its pinned catalog.
    let listener = HaystackServer::new(SharedGraph::new(EntityGraph::new()))
        .with_scoped_reads(builder.handle())
        .with_auth(auth)
        .with_cors(cors)
        .host(host)
        .port(port)
        .into_listener();
    builder
        .owned_resource(listener)
        .start(&tokio::runtime::Handle::current())
        .map_err(|error| error.to_string())
}

struct Permissions;
fn permitted(principal: &Principal, permission: &str) -> bool {
    matches!(principal, Principal::Authenticated { permissions, .. }
        if permissions.iter().any(|p| p == permission || p == "admin"))
}
impl ReadPolicy for Permissions {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        if !permitted(principal, "read") {
            return Err(ReadError::Forbidden);
        }
        Ok(Arc::new(Self))
    }
}
impl PolicySnapshot for Permissions {
    fn scope_key(&self) -> &str {
        "standalone-authenticated-read-v1"
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        true
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        true
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        true
    }
    fn reference(&self, _: &str) -> bool {
        true
    }
    fn reference_display(&self, _: &str) -> bool {
        true
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
impl MutationPolicy for Permissions {
    fn authorize_intent(&self, p: &Principal, _: &EntityBatchRequest) -> bool {
        permitted(p, "write")
    }
    fn authorize_change(&self, p: &Principal, _: &PreparedChange) -> bool {
        permitted(p, "write")
    }
    fn authorize_reconcile(
        &self,
        p: &Principal,
        _: &OperationIdentity,
        _: Option<&EntityBatchRequest>,
    ) -> bool {
        permitted(p, "write")
    }
}
impl HistoryMutationPolicy for Permissions {
    fn authorize_intent(&self, p: &Principal, _: &HistoryWriteRequest) -> bool {
        permitted(p, "write")
    }
    fn authorize_schema(&self, p: &Principal, _: &HistoryWriteRequest, _: &HistorySchema) -> bool {
        permitted(p, "write")
    }
    fn authorize_reconcile(
        &self,
        p: &Principal,
        _: &HistoryOperationIdentity,
        _: Option<&HistoryWriteRequest>,
    ) -> bool {
        permitted(p, "write")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permissions_require_authentication_and_keep_admin_override() {
        for principal in [
            Principal::Anonymous,
            Principal::TrustedEmbedding {
                subject: "owner".into(),
            },
            Principal::authenticated("none", vec![]),
        ] {
            assert!(Permissions.snapshot(&principal).is_err());
            assert!(!permitted(&principal, "write"));
        }
        let viewer = Principal::authenticated("viewer", vec!["read".into()]);
        assert!(Permissions.snapshot(&viewer).is_ok());
        assert!(!permitted(&viewer, "write"));
        let writer = Principal::authenticated("writer", vec!["write".into()]);
        assert!(Permissions.snapshot(&writer).is_err());
        assert!(permitted(&writer, "write"));
        let admin = Principal::authenticated("admin", vec!["admin".into()]);
        assert!(Permissions.snapshot(&admin).is_ok());
        assert!(permitted(&admin, "write"));
    }
    #[test]
    fn mutation_callbacks_authorize_each_stage_independently() {
        use haystack_core::{
            data::HDict,
            graph::EntityOperation,
            kinds::{HRef, Kind},
        };
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("point")));
        let entity = EntityBatchRequest {
            identity: OperationIdentity {
                dataset: [1; 16],
                incarnation: [2; 16],
                operation_id: "entity".into(),
            },
            expected_revision: 0,
            operations: vec![EntityOperation::Add(row)],
        };
        let plan = EntityGraph::new()
            .prepare_batch(0, &entity.operations, Default::default())
            .unwrap();
        let history = HistoryWriteRequest {
            identity: HistoryOperationIdentity {
                authority: [3; 16],
                incarnation: [4; 16],
                point: "point".into(),
                operation_id: "history".into(),
            },
            expected_generation: 0,
            samples: vec![],
        };
        let schema = HistorySchema {
            kind: haystack_core::codecs::history::HistoryKind::Number,
            unit: None,
            timezone: "UTC".into(),
        };
        for (principal, expected) in [
            (Principal::Anonymous, false),
            (
                Principal::TrustedEmbedding {
                    subject: "owner".into(),
                },
                false,
            ),
            (
                Principal::authenticated("viewer", vec!["read".into()]),
                false,
            ),
            (
                Principal::authenticated("operator", vec!["read".into(), "write".into()]),
                true,
            ),
            (
                Principal::authenticated("admin", vec!["admin".into()]),
                true,
            ),
        ] {
            assert_eq!(
                MutationPolicy::authorize_intent(&Permissions, &principal, &entity),
                expected
            );
            assert_eq!(
                Permissions.authorize_change(&principal, &plan.changes()[0]),
                expected
            );
            assert_eq!(
                MutationPolicy::authorize_reconcile(
                    &Permissions,
                    &principal,
                    &entity.identity,
                    Some(&entity)
                ),
                expected
            );
            assert_eq!(
                MutationPolicy::authorize_reconcile(
                    &Permissions,
                    &principal,
                    &entity.identity,
                    None
                ),
                expected
            );
            assert_eq!(
                HistoryMutationPolicy::authorize_intent(&Permissions, &principal, &history),
                expected
            );
            assert_eq!(
                Permissions.authorize_schema(&principal, &history, &schema),
                expected
            );
            assert_eq!(
                HistoryMutationPolicy::authorize_reconcile(
                    &Permissions,
                    &principal,
                    &history.identity,
                    Some(&history)
                ),
                expected
            );
            assert_eq!(
                HistoryMutationPolicy::authorize_reconcile(
                    &Permissions,
                    &principal,
                    &history.identity,
                    None
                ),
                expected
            );
        }
    }
}
