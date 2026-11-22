use super::super::lineage::tests::{Logs, assert_no_plaintext};
use super::super::{connectors, probe};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const REFRESH: &str = "aorAAAAAGkiro-refresh-token-0123456789";
const SECRET: &str = "kiro-client-secret-abcdefghij";
const ROTATED: &str = "aorAAAAAGkiro-rotated-token-9876543210";

fn signin() -> String {
    json!({
        "refresh_token": REFRESH, "client_id": "cid-123", "client_secret": SECRET,
        "client_secret_expires_at": 4_102_444_800i64, "region": "us-east-1",
        "start_url": "https://view.awsapps.com/start",
    })
    .to_string()
}

async fn core() -> (
    Core,
    tempfile::TempDir,
    Logs,
    tracing::subscriber::DefaultGuard,
) {
    let logs = Logs::default();
    let g = logs.install();
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), None).await.unwrap();
    c.store()
        .set_setting(probe::AUTO_TEST_SETTING, "off")
        .await
        .unwrap();
    (c, dir, logs, g)
}

/// A local SSO-OIDC stand-in answering `/token` with `body`.
async fn oidc(
    status: u16,
    body: String,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let s2 = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let mut buf = vec![0u8; 8192];
            let n = c.read(&mut buf).await.unwrap_or(0);
            s2.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_string());
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = c.write_all(resp.as_bytes()).await;
        }
    });
    (format!("http://{addr}"), seen)
}

#[test]
fn sign_ins_are_checked_and_re_encoded_for_belai() {
    let now = kls_store::now_ms();
    let (v, s) = parse_login(&format!("\n{}\n", signin()), now).unwrap();
    let back: Value = serde_json::from_str(v.expose()).unwrap();
    assert_eq!(back["refresh_token"], REFRESH);
    assert_eq!(back["client_secret_expires_at"], 4_102_444_800i64);
    assert!(!v.expose().contains('\n'), "compact, no control characters");
    assert_eq!(s.region, "us-east-1");
    assert!(!s.identity_center);
    // The AWS SSO cache's camel case and an RFC 3339 expiry read too.
    let camel = json!({
        "refreshToken": REFRESH, "clientId": "cid", "clientSecret": SECRET,
        "registrationExpiresAt": "2099-01-01T00:00:00Z", "region": "ap-southeast-2",
        "startUrl": "https://acme.awsapps.com/start",
        "profileArn": "arn:aws:codewhisperer:us-east-1:111122223333:profile/X",
    })
    .to_string();
    let (v, s) = parse_login(&camel, now).unwrap();
    let back: Value = serde_json::from_str(v.expose()).unwrap();
    assert_eq!(back["client_id"], "cid");
    assert!(back["client_secret_expires_at"].as_i64().unwrap() > 4_000_000_000);
    assert!(s.identity_center);
    assert_eq!(s.region, "ap-southeast-2");
    // Refusals name the problem, never a value.
    for (raw, why) in [
        ("not json".to_string(), "is JSON"),
        ("[1]".into(), "JSON object"),
        (
            json!({"refresh_token": REFRESH, "provider": "Github"}).to_string(),
            "GitHub and Google",
        ),
        (json!({"refresh_token": REFRESH}).to_string(), "client_id"),
        (
            json!({"refresh_token": REFRESH, "client_id": "c", "client_secret": SECRET,
                   "region": "us-east-1.evil.com"})
            .to_string(),
            "AWS region",
        ),
        (
            json!({"refresh_token": REFRESH, "client_id": "c", "client_secret": SECRET,
                   "client_secret_expires_at": 1_000})
            .to_string(),
            "expired",
        ),
        (
            json!({"refresh_token": REFRESH, "client_id": "c", "client_secret": SECRET,
                   "start_url": "http://x"})
            .to_string(),
            "https",
        ),
    ] {
        let e = parse_login(&raw, now).unwrap_err();
        assert!(e.contains(why), "{raw}: {e}");
        assert!(!e.contains(REFRESH) && !e.contains(SECRET));
        assert_eq!(login_rule(&raw), e);
    }
    assert!(login_rule(&signin()).is_empty());
    assert!(valid_region("us-gov-west-1") && valid_region("eu-central-1"));
    assert!(!valid_region("us-east") && !valid_region("US-EAST-1"));
    // The dialog test never contacts AWS.
    assert_eq!(check_typed(&signin()).state, ProbeState::Skipped);
    assert_eq!(check_typed("{}").state, ProbeState::Refused);
}

#[tokio::test]
async fn sign_in_is_sealed_audited_and_granted_to_kiro_harnesses() {
    let (c, dir, logs, _g) = core().await;
    let file = dir.path().join("kiro.json");
    std::fs::write(&file, signin()).unwrap();
    let raw = read_login_file(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    let (m, s) = put_login(&c, "", raw, "file").await.unwrap();
    assert_eq!((m.provider.as_str(), m.env.as_str()), (PROVIDER, LOGIN_ENV));
    assert_eq!(s.region, "us-east-1");
    // The catalogue gained Kiro; the generic form refuses a bad sign-in.
    assert!(c.store().provider(PROVIDER).await.unwrap().is_some());
    let e = super::super::put_secret(&c, PROVIDER, LOGIN_ENV, "", SecretString::new("{}"))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("refresh_token"));
    let e = super::super::rotate_secret(&c, &m.id, SecretString::new("nope"))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("JSON"));
    let k = put_api_key(&c, "", SecretString::new("ksk_kiro_api_key_0123456789"))
        .await
        .unwrap();
    assert_eq!(k.env, API_KEY_ENV);
    assert!(
        put_api_key(&c, "", SecretString::new("{\"a\":1}"))
            .await
            .is_err()
    );
    // Provenance on the ledger: where it came from, its fingerprint.
    c.audit_sync().await.unwrap();
    let imported = c
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["vault".into()],
            text: Some("kiro sign-in imported".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].payload["source"], "file");
    assert_eq!(imported[0].payload["fingerprint"], m.fingerprint.as_str());
    // A grant for kiro carries both variables; the kiro-cli harness takes
    // KIRO_API_KEY (its grant_env), belai's kiro provider KIRO_LOGIN.
    let g = super::super::grant_for(&c, &[PROVIDER.into()], 60_000)
        .await
        .unwrap()
        .unwrap();
    let mut envs: Vec<&str> = g.secrets.iter().map(|s| s.env.as_str()).collect();
    envs.sort();
    assert_eq!(envs, [API_KEY_ENV, LOGIN_ENV]);
    let handed: Value = serde_json::from_str(
        g.secrets
            .iter()
            .find(|s| s.env == LOGIN_ENV)
            .unwrap()
            .value
            .expose(),
    )
    .unwrap();
    assert_eq!(handed["refresh_token"], REFRESH);
    // Kill switch: no grant leaves, and the refusal is audited.
    crate::control::pause(&c, "drill").await.unwrap();
    let e = super::super::grant_for(&c, &[PROVIDER.into()], 60_000)
        .await
        .unwrap_err();
    assert!(crate::control::is_blocked(&e));
    assert!(
        c.store()
            .recent_events(200)
            .await
            .unwrap()
            .iter()
            .any(|e| e.kind == super::super::kinds::VAULT_GRANT_REFUSED)
    );
    crate::control::resume(&c).await.unwrap();
    drop(g);
    assert_no_plaintext(&c, dir.path(), &logs, REFRESH).await;
    assert_no_plaintext(&c, dir.path(), &logs, SECRET).await;
}

#[tokio::test]
async fn stored_test_mints_keeps_a_rotated_token_and_obeys_the_kill_switch() {
    let (c, dir, logs, _g) = core().await;
    let (base, seen) = oidc(
        200,
        json!({"accessToken": "kiro-access-token-zzzzzzzz", "refreshToken": ROTATED,
               "expiresIn": 3600})
        .to_string(),
    )
    .await;
    set_test_base(&base);
    let (m, _) = put_login(&c, "kiro", SecretString::new(signin()), "paste")
        .await
        .unwrap();
    // Paused: skipped, nothing sent.
    crate::control::pause(&c, "drill").await.unwrap();
    let r = probe::probe_stored(&c, &m.id, "owner").await.unwrap();
    assert_eq!(r.state, ProbeState::Skipped);
    assert!(seen.lock().unwrap().is_empty());
    crate::control::resume(&c).await.unwrap();
    // Running: one POST /token with the refresh grant; accepted.
    let r = probe::probe_stored(&c, &m.id, "owner").await.unwrap();
    assert_eq!(r.state, ProbeState::Accepted, "{r:?}");
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("POST /token"));
    assert!(req.contains("\"grantType\":\"refresh_token\""));
    // The rotated refresh token is the next version, by kiro-refresh.
    let after = c.store().vault_secret_meta(&m.id).await.unwrap().unwrap();
    assert_eq!(after.version, m.version + 1);
    assert_eq!(after.test_state, "accepted");
    let v = super::super::vault(&c)
        .await
        .unwrap()
        .get(&m.id)
        .await
        .unwrap()
        .unwrap();
    assert!(v.expose().contains(ROTATED));
    assert!(c.store().recent_events(100).await.unwrap().iter().any(|e| {
        e.kind == super::super::kinds::VAULT_SECRET_SAVED && e.data_json.contains("kiro-refresh")
    }));
    // A refused sign-in reads refused with the OIDC code only.
    let (base, _) = oidc(
        400,
        json!({"error": "invalid_grant", "error_description": REFRESH}).to_string(),
    )
    .await;
    set_test_base(&base);
    let r = probe::probe_stored(&c, &m.id, "owner").await.unwrap();
    assert_eq!(r.state, ProbeState::Refused);
    assert!(r.detail.starts_with("invalid_grant") && !r.detail.contains(REFRESH));
    set_test_base("");
    drop(v);
    assert_no_plaintext(&c, dir.path(), &logs, ROTATED).await;
    assert_no_plaintext(&c, dir.path(), &logs, SECRET).await;
}

#[tokio::test]
async fn connector_secrets_are_kept_never_granted_and_opened_audited() {
    let (c, dir, logs, _g) = core().await;
    const TOKEN: &str = "xoxb-connector-slack-token-123456";
    let f = |k: &str, v: &str| (k.to_string(), SecretString::new(v));
    // The fields each kind needs.
    assert!(
        connectors::encode("slack", &[])
            .unwrap_err()
            .contains("botToken")
    );
    assert!(connectors::encode("discord", &[f("url", "http://x/y")]).is_err());
    assert!(connectors::encode("jira", &[f("email", "me"), f("apiToken", "t")]).is_err());
    assert!(connectors::encode("nope", &[]).is_err());
    assert!(connectors::encode("slack", &[f("botToken", TOKEN), f("extra", "x")]).is_err());
    let m = connectors::put(&c, "slack", "ops", "", &[f("botToken", TOKEN)], "vault")
        .await
        .unwrap();
    assert_eq!(m.provider, "connector-slack");
    assert_eq!(m.env, "OPS");
    assert_eq!(m.kind, connectors::KIND);
    let list = connectors::list(&c).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].kind_label, "Slack");
    // Never granted, whatever asks.
    let g = super::super::grant_for(&c, &["connector-slack".into()], 60_000)
        .await
        .unwrap();
    assert!(g.is_none());
    // Opened by Keleustes only, audited once per surface per hour.
    let v = connectors::open(&c, &m.id, "notify", "run-1")
        .await
        .unwrap();
    assert_eq!(connectors::field(&v, "botToken").unwrap().expose(), TOKEN);
    connectors::open(&c, &m.id, "notify", "run-2")
        .await
        .unwrap();
    let list = connectors::list(&c).await.unwrap();
    assert_eq!(list[0].used_by, ["notify"]);
    c.audit_sync().await.unwrap();
    let opened = c
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["vault".into()],
            text: Some("connector opened".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(opened.len(), 1, "merged per surface and hour");
    // Kill switch: refused and audited.
    crate::control::stop(&c, "drill").await.unwrap();
    let e = connectors::open(&c, &m.id, "notify", "run-3")
        .await
        .unwrap_err();
    assert!(crate::control::is_blocked(&e));
    assert!(
        c.store()
            .recent_events(50)
            .await
            .unwrap()
            .iter()
            .any(|e| e.kind == "factory.refused")
    );
    crate::control::resume(&c).await.unwrap();
    // Replace and revoke, each on the ledger; the generic rotate refuses.
    assert!(
        super::super::rotate_secret(&c, &m.id, SecretString::new("x"))
            .await
            .is_err()
    );
    let m2 = connectors::replace(&c, &m.id, &[f("botToken", "xoxb-replaced-token-000000")])
        .await
        .unwrap();
    assert_eq!(m2.version, m.version + 1);
    assert!(connectors::revoke(&c, &m.id).await.unwrap());
    assert!(connectors::list(&c).await.unwrap().is_empty());
    c.audit_sync().await.unwrap();
    let acts: Vec<String> = c
        .audit_entries(&kls_store::audit::LedgerQuery {
            kinds: vec!["vault".into()],
            text: Some("connector".into()),
            limit: 50,
            ..Default::default()
        })
        .await
        .unwrap()
        .iter()
        .map(|e| e.payload["action"].as_str().unwrap_or("").to_string())
        .collect();
    for a in [
        "connector_saved",
        "connector_opened",
        "connector_replaced",
        "connector_revoked",
    ] {
        assert!(acts.iter().any(|x| x == a), "{a}: {acts:?}");
    }
    drop(v);
    assert_no_plaintext(&c, dir.path(), &logs, TOKEN).await;
}
