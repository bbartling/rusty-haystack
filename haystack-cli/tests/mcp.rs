use std::process::Command;

#[test]
fn mcp_help_explains_the_explicit_stdio_config() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--config <FILE>"));
    assert!(help.contains("Unix pipe"));
}

#[cfg(not(feature = "mcp"))]
#[test]
fn mcp_disabled_build_fails_before_configuration_access() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--config", "/not-an-existing-mcp-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--features mcp")
    );
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_rejects_unknown_config_without_echoing_parser_input() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mcp.toml");
    std::fs::write(&path, "version = 77\ncredential = 'do-not-echo-this'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "stdio", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("invalid MCP configuration"));
    assert!(!diagnostic.contains("do-not-echo-this"));
}

#[cfg(all(feature = "mcp", unix))]
#[test]
fn fifo_config_and_dataset_without_a_writer_fail_promptly() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut blocked = Vec::new();
    for config_fifo in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("do-not-echo-this.fifo");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let config = if config_fifo {
            fifo.clone()
        } else {
            let config = directory.path().join("mcp.toml");
            std::fs::write(&config,"version = 1\ndataset_file = 'do-not-echo-this.fifo'\ndataset_format = 'trio'\nprincipal = 'fixture'\nentity_ids = ['a']\n").unwrap();
            config
        };
        let mut child = Command::new(env!("CARGO_BIN_EXE_haystack"))
            .args(["mcp", "stdio", "--config"])
            .arg(config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let prompt = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if started.elapsed() > Duration::from_secs(2) {
                child.kill().unwrap();
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        if !prompt {
            blocked.push(config_fifo);
            continue;
        }
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("must be regular files"));
        assert!(!diagnostic.contains("do-not-echo-this"));
    }
    assert!(
        blocked.is_empty(),
        "FIFO launch blocked before regular-file validation (config_fifo): {blocked:?}"
    );
}

#[test]
fn mcp_http_help_describes_explicit_loopback_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "http", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--config <FILE>"));
    assert!(help.contains("loopback"));
}
#[cfg(not(feature = "mcp-http"))]
#[test]
fn mcp_http_disabled_fails_before_configuration_access() {
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "http", "--config", "/not-an-existing-http-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--features mcp-http")
    );
}
#[cfg(feature = "mcp-http")]
#[test]
fn mcp_http_invalid_configuration_is_sanitized() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("http.toml");
    std::fs::write(&path, "version = 99\ncredential = 'do-not-echo-this'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_haystack"))
        .args(["mcp", "http", "--config"])
        .arg(path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("invalid MCP HTTP configuration"));
    assert!(!error.contains("do-not-echo-this"));
}
