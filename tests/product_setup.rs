//! The customer setup contract: provider credentials never require Docker or leak into status.
use serde_json::Value;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

fn command(temp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ic"));
    cmd.env("IC_DATA_DIR", temp.path())
        .env("IC_MODELS_DIR", temp.path().join("models"))
        .env("ANTHROPIC_CONFIG_DIR", temp.path().join("anthropic"))
        .env("IC_MODEL", "openai/gpt-5.6")
        .env("IC_DOCKER", temp.path().join("docker"))
        .env_remove("IC_LLM_URL")
        .env_remove("IC_LLM_KEY")
        .env_remove("IC_SCORER");
    cmd
}

#[test]
fn required_jev_key_and_openai_key_can_be_saved_without_installing_or_starting_docker() {
    let temp = tempfile::tempdir().unwrap();
    let docker = temp.path().join("docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\ntouch \"$IC_DATA_DIR/docker-was-used\"\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    for (target, key) in [
        ("typesafe", "typesafe-test-key-long-enough"),
        ("openai", "sk-openai-test-key-long-enough"),
    ] {
        let mut child = command(&temp)
            .args(["proxy", "key", target, "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "{key}").unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&result.stdout).contains(key));
    }
    let result = command(&temp)
        .args(["setup", "status", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let status: Value = serde_json::from_slice(&result.stdout).unwrap();
    let checks = status["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .all(|c| c["id"] != "docker" && c["id"] != "proxy")
    );
    let jev = checks.iter().find(|c| c["id"] == "typesafe_key").unwrap();
    assert_eq!(jev["required"], true);
    assert_eq!(jev["status"], "ok");
    assert_eq!(status["openai"]["using_api_key"], true);
    assert!(!String::from_utf8_lossy(&result.stdout).contains("test-key-long-enough"));
    assert!(!temp.path().join("docker-was-used").exists());
    assert!(!temp.path().join("litellm").exists());
}
