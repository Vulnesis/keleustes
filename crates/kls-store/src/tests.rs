//! Store tests. SQLite always runs; Postgres runs when
//! `KLS_TEST_POSTGRES_URL` points at a disposable database.

use super::*;

async fn stores() -> Vec<(Store, Option<tempfile::TempDir>)> {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
    let mut out = vec![(s, Some(dir))];
    if let Ok(url) = std::env::var("KLS_TEST_POSTGRES_URL") {
        let s = Store::open(&url).await.unwrap();
        for t in ["node_outbox", "events", "nodes", "join_tokens", "settings"] {
            sqlx::query(AssertSqlSafe(format!("DELETE FROM {t}")))
                .execute(&s.pool)
                .await
                .unwrap();
        }
        out.push((s, None));
    }
    out
}

#[test]
fn postgres_placeholders() {
    assert_eq!(
        to_postgres("SELECT '?' FROM t WHERE a = ? AND b = ?"),
        "SELECT '?' FROM t WHERE a = $1 AND b = $2"
    );
}

#[tokio::test]
async fn settings_roundtrip() {
    for (s, _d) in stores().await {
        assert_eq!(s.setting("k").await.unwrap(), None);
        s.set_setting("k", "v1").await.unwrap();
        s.set_setting("k", "v2").await.unwrap();
        assert_eq!(s.setting("k").await.unwrap().as_deref(), Some("v2"));
    }
}

#[tokio::test]
async fn join_token_single_use_and_expiry() {
    for (s, _d) in stores().await {
        let far = now_ms() + 60_000;
        s.insert_join_token("t1", "h1", "laptop", far)
            .await
            .unwrap();
        assert!(s.redeem_join_token("t1", "wrong", "n0").await.is_err());
        let t = s.redeem_join_token("t1", "h1", "n1").await.unwrap();
        assert_eq!(t.used_by_node.as_deref(), Some("n1"));
        assert!(
            s.redeem_join_token("t1", "h1", "n2").await.is_err(),
            "reuse refused"
        );

        s.insert_join_token("t2", "h2", "", now_ms() - 1)
            .await
            .unwrap();
        assert!(
            s.redeem_join_token("t2", "h2", "n3").await.is_err(),
            "expired refused"
        );
    }
}

#[tokio::test]
async fn nodes_and_outbox() {
    for (s, _d) in stores().await {
        let host = HostInfo {
            hostname: "box".into(),
            capabilities: Capabilities {
                gh_authenticated: true,
                ..Default::default()
            },
            trusted_dirs: vec!["/src".into()],
            agents: vec!["belai:builder".into()],
            crews: vec!["belai:repro".into()],
            ..Default::default()
        };
        s.insert_node("n1", "box", &host, "serial", "t1")
            .await
            .unwrap();
        let n = s.node("n1").await.unwrap().unwrap();
        assert_eq!(n.host, host);
        assert_eq!(s.nodes().await.unwrap().len(), 1);
        // A later advert replaces the agents and crews.
        let mut later = host.clone();
        later.agents = vec!["belai:builder".into(), "belai:reviewer".into()];
        later.crews = Vec::new();
        s.update_node_host("n1", &later).await.unwrap();
        assert_eq!(s.node("n1").await.unwrap().unwrap().host, later);

        assert_eq!(s.outbox_push("n1", b"a").await.unwrap(), 1);
        assert_eq!(s.outbox_push("n1", b"b").await.unwrap(), 2);
        assert_eq!(s.outbox_after("n1", 0).await.unwrap().len(), 2);
        s.outbox_ack("n1", 1).await.unwrap();
        let rest = s.outbox_after("n1", 0).await.unwrap();
        assert_eq!(rest, vec![(2, b"b".to_vec())]);
        s.outbox_ack("n1", 2).await.unwrap();
        // Sequence keeps rising after the outbox empties.
        assert_eq!(s.outbox_push("n1", b"c").await.unwrap(), 3);

        s.set_node_seq("n1", 5).await.unwrap();
        s.set_node_seq("n1", 3).await.unwrap();
        assert_eq!(s.node("n1").await.unwrap().unwrap().node_seq, 5);

        assert!(s.revoke_node("n1", "lost").await.unwrap());
        assert!(!s.revoke_node("n1", "again").await.unwrap());
    }
}

#[tokio::test]
async fn events() {
    for (s, _d) in stores().await {
        let a = s.append_event("node.joined", "n1", "{}").await.unwrap();
        let b = s.append_event("node.seen", "n1", "{}").await.unwrap();
        assert!(b.id > a.id);
        assert_eq!(s.events_after(a.id, 10).await.unwrap().len(), 1);
        let recent = s.recent_events(10).await.unwrap();
        assert_eq!(recent.first().unwrap().id, a.id);
    }
}

/// Read-then-write transactions racing on one row and one sequence: every
/// one commits (no `SQLITE_BUSY_SNAPSHOT` 517, no lost update, no seq taken twice).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_read_then_write_no_busy_snapshot() {
    use crate::github::DecisionEnd;
    for (s, _d) in stores().await {
        let node = format!("n-{}", now_ms());
        let at = now_ms();
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let (s, node) = (s.clone(), node.clone());
            tasks.push(tokio::spawn(async move {
                let mut seqs = Vec::new();
                for _ in 0..20 {
                    seqs.push(s.outbox_push(&node, b"f").await?);
                    s.record_decision(at, "k", "m", DecisionEnd::Proceeded, Some(0.5), Some(1))
                        .await?;
                }
                anyhow::Ok(seqs)
            }));
        }
        let mut seqs = Vec::new();
        for t in tasks {
            seqs.extend(t.await.unwrap().expect("no BUSY / BUSY_SNAPSHOT"));
        }
        seqs.sort();
        assert_eq!(seqs, (1..=240).collect::<Vec<i64>>());
        let day = crate::github::day_start(at);
        let days = s.decision_days(day, day + 1).await.unwrap();
        assert_eq!(days.iter().map(|d| d.total).sum::<i64>(), 240);
    }
}

/// The poller's write pattern (quota and call-count upserts, ETag bodies,
/// events) next to read-then-write transactions: every single statement
/// stays well under a second (it waited seconds on fsync per commit before).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_stay_fast() {
    use crate::github::{DecisionEnd, Quota};
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
    let body = "x".repeat(64 * 1024);
    let mut tasks = Vec::new();
    for w in 0..8 {
        let (s, body) = (s.clone(), body.clone());
        tasks.push(tokio::spawn(async move {
            let mut lat = Vec::new();
            for i in 0..30 {
                let t = std::time::Instant::now();
                match w % 4 {
                    0 => {
                        let q = Quota {
                            route: "local".into(),
                            resource: format!("r{}", i % 3),
                            limit: 5000,
                            remaining: 4000 - i,
                            observed_at: now_ms(),
                            ..Default::default()
                        };
                        s.gh_put_quota(&q).await?
                    }
                    1 => {
                        s.gh_count_call("local", "poller", now_ms(), i % 2 == 0)
                            .await?
                    }
                    2 => {
                        s.gh_put_etag(&format!("k{w}-{}", i % 5), "e", "", &body)
                            .await?
                    }
                    _ => {
                        s.append_event("bench.tick", "s", "{}").await?;
                    }
                }
                lat.push(t.elapsed());
                let t = std::time::Instant::now();
                s.record_decision(now_ms(), "k", "m", DecisionEnd::Escalated, None, None)
                    .await?;
                lat.push(t.elapsed());
            }
            anyhow::Ok(lat)
        }));
    }
    let mut lat = Vec::new();
    for t in tasks {
        lat.extend(t.await.unwrap().unwrap());
    }
    lat.sort();
    let p = |q: f64| lat[((lat.len() - 1) as f64 * q) as usize];
    eprintln!(
        "writes={} p50={:?} p99={:?} max={:?}",
        lat.len(),
        p(0.5),
        p(0.99),
        lat[lat.len() - 1]
    );
    assert!(
        p(0.99) < std::time::Duration::from_millis(250),
        "p99 {:?}",
        p(0.99)
    );
}

/// Every SQLite pool connection commits without an fsync (WAL + NORMAL) and
/// leaves checkpoints to the background checkpointer, which runs.
#[tokio::test]
async fn sqlite_commits_skip_fsync_and_checkpointer_runs() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open_sqlite(&dir.path().join("t.db")).await.unwrap();
    let mut conns = Vec::new();
    for _ in 0..4 {
        conns.push(s.pool.acquire().await.unwrap());
    }
    for c in &mut conns {
        let mode: String = sqlx::query("PRAGMA journal_mode")
            .fetch_one(&mut **c)
            .await
            .unwrap()
            .get(0);
        assert_eq!(mode, "wal");
        let sync: i64 = sqlx::query("PRAGMA synchronous")
            .fetch_one(&mut **c)
            .await
            .unwrap()
            .get(0);
        assert_eq!(sync, 1, "NORMAL");
        let auto: i64 = sqlx::query("PRAGMA wal_autocheckpoint")
            .fetch_one(&mut **c)
            .await
            .unwrap()
            .get(0);
        assert_eq!(auto, WAL_AUTOCHECKPOINT_PAGES);
    }
    drop(conns);
    s.set_setting("k", "v").await.unwrap();
    let deadline = std::time::Instant::now() + CHECKPOINT_EVERY * 5;
    while s.wal_checkpoints() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "checkpointer never ran"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
