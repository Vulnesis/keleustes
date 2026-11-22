//! `keleustes sessions vault-key`: the key is saved and printed at once, and
//! its background test still finishes before the process exits (it runs on
//! the command's runtime, so it used to die with it and the key read
//! untested forever).

use kls_core::Core;
use std::io::Write;
use std::process::{Command, Stdio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BIN: &str = env!("CARGO_BIN_EXE_keleustes");

/// A provider test endpoint that answers 200 after a short delay.
async fn probe_server() -> String {
    let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = ln.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = ln.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                    )
                    .await;
            });
        }
    });
    format!("http://{addr}/v1/models")
}

#[tokio::test(flavor = "multi_thread")]
async fn vault_key_from_the_command_line_is_tested() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), None).await.unwrap();
    let url = probe_server().await;
    kls_core::vault::save_provider(
        &core,
        &kls_store::vault::Provider {
            id: "fake".into(),
            name: "Fake".into(),
            env: "FAKE_API_KEY".into(),
            probe_url: url,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut child = Command::new(BIN)
        .args(["--data"])
        .arg(dir.path())
        .args([
            "sessions",
            "vault-key",
            "--provider",
            "fake",
            "--env",
            "FAKE_API_KEY",
        ])
        .env("KELEUSTES_VAULT_BACKEND", "file")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"fk-test-0123456789")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let id = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let m = core.store().vault_secret_meta(&id).await.unwrap().unwrap();
    assert_eq!(m.test_state, "accepted", "{m:?}");
    assert_eq!(m.test_status, Some(200));
}
