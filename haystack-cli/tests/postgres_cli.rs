//! Product entry-point failures must stay actionable, nonzero and secret-safe.
use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(args)
        .env_remove("HAYSTACK_MISSING_TEST_PASSWORD")
        .output()
        .unwrap()
}

#[test]
fn postgres_selection_is_discoverable_and_conflicts_with_memory_seeding() {
    let help = run(&["serve", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--postgres-config"));
    for extra in [vec!["--demo"], vec!["--file", "entities.trio"]] {
        let mut args = vec!["serve", "--postgres-config", "unused", "--users", "unused"];
        args.extend(extra);
        let output = run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
        assert!(output.stdout.is_empty());
    }
    let output = run(&["serve", "--postgres-config", "unused"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--users"));
}

#[cfg(not(feature = "postgres"))]
#[test]
fn disabled_postgres_is_a_clear_error_before_opening_files() {
    let output = run(&["serve", "--postgres-config", "absent", "--users", "absent"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--features postgres"));
    assert!(output.stdout.is_empty());
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_rejects_invalid_configuration_and_users_without_echoing_secrets() {
    use haystack_server::auth::users::hash_password;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("database with spaces.toml");
    let users = temp.path().join("users.toml");
    let valid_users = format!(
        "[users.reader]\npassword_hash = {:?}\npermissions = [\"read\"]\n",
        hash_password("only-local-test")
    );
    let valid_config = "host = '127.0.0.1'\nport = 5432\ndatabase = 'local'\nusername = 'local'\nschema = 'standalone'\npassword_env = 'HAYSTACK_MISSING_TEST_PASSWORD'\n[transport]\nmode = 'loopback-plaintext'\n";
    for (source, credentials, expected) in [
        (
            "password = 'secret-sentinel'\ninvalid = [",
            valid_users.as_str(),
            "invalid PostgreSQL TOML",
        ),
        (
            "host = 'secret-sentinel'\nunknown_key = true",
            valid_users.as_str(),
            "invalid PostgreSQL TOML",
        ),
        (
            valid_config,
            valid_users.as_str(),
            "password environment variable is missing",
        ),
        (valid_config, "[users]\n", "nonempty --users"),
        (
            valid_config,
            "[users.leak]\npassword_hash = 'secret-sentinel'\n",
            "invalid or unreadable users file",
        ),
    ] {
        std::fs::write(&config, source).unwrap();
        std::fs::write(&users, credentials).unwrap();
        let output = run(&[
            "serve",
            "--postgres-config",
            config.to_str().unwrap(),
            "--users",
            users.to_str().unwrap(),
        ]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(expected),
            "expected bounded category: {expected}"
        );
        assert!(!error.contains("secret-sentinel"));
    }
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_semantic_configuration_failures_do_not_connect_or_echo_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("database.toml");
    let users = temp.path().join("users.toml");
    std::fs::write(
        &users,
        format!(
            "[users.reader]\npassword_hash = {:?}\npermissions = [\"read\"]\n",
            haystack_server::auth::users::hash_password("only-local-test")
        ),
    )
    .unwrap();
    for (host, port, schema) in [
        ("127.0.0.1", 0, "standalone"),
        ("secret-sentinel.invalid", 5432, "standalone"),
        ("127.0.0.1", 5432, "public"),
    ] {
        std::fs::write(&config, format!("host = {host:?}\nport = {port}\ndatabase = 'local'\nusername = 'local'\nschema = {schema:?}\npassword_env = 'HAYSTACK_TEST_DATABASE_PASSWORD'\n[transport]\nmode = 'loopback-plaintext'\n")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
            .args([
                "serve",
                "--postgres-config",
                config.to_str().unwrap(),
                "--users",
                users.to_str().unwrap(),
            ])
            .env("HAYSTACK_TEST_DATABASE_PASSWORD", "secret-sentinel")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("invalid PostgreSQL configuration"));
        assert!(!error.contains("secret-sentinel"));
    }
}
