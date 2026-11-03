//! A change of certificate authority (ADR-0088) with the real renewal
//! client of agents and a relay, the relay itself, and the server's
//! renewal, enrollment, CRL and rotation pieces over mTLS and its database.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::renew::{Renewal, RenewalSettings};
use monitorflow_common::client::shutdown::{self, Trigger};
use monitorflow_common::tls::{self, PeerRole};
use monitorflow_server::ca::{ServerName, Stage};
use monitorflow_server::ca_rotation::{self, Actor, Rotator};
use monitorflow_server::crl::CrlPublisher;
use monitorflow_server::enroll::{self, Enroller, FailureLimiter};
use monitorflow_server::grpc::{
    self, AgentStreamService, CrlService, DbInventory, DbRelayPolicy, Registry,
};
use monitorflow_server::renew::Renewer;
use sqlx::PgPool;
use support::{enroll_as, hello, open, server_settings, wait_for, BufferOptions, Pki, Relay};
use tokio::net::TcpListener;
use tokio::sync::watch;

/// The server as `main` wires it for a change of CA: listeners that follow
/// the publisher, and the rotator.
struct Server {
    url: String,
    enroll_url: String,
    rotator: Rotator,
}

async fn start_server(pki: &Pki, pool: &PgPool) -> Server {
    let (crl, tls_config) = CrlPublisher::start(pki.ca.clone(), pki.server.clone())
        .await
        .unwrap();
    let crl = Arc::new(crl);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = support::url(&listener);
    let service = AgentStreamService::new(
        Registry::default(),
        Arc::new(DbRelayPolicy::new(pool.clone())),
        server_settings(),
    )
    .with_inventory(Arc::new(DbInventory::new(pool.clone())));
    // The production renewal limit: a change of CA is exempt from it.
    let renewer = Renewer::new(pool.clone(), pki.ca.clone());
    tokio::spawn(grpc::serve_all(
        listener,
        tls_config,
        service,
        Some(renewer),
        Some(CrlService::new(crl.subscribe())),
        std::future::pending(),
    ));
    let enroll_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let enroll_url = support::url(&enroll_listener);
    let enroller = Enroller::new(pool.clone(), pki.ca.clone(), FailureLimiter::default());
    tokio::spawn(enroll::serve_following(
        enroll_listener,
        crl.enroll_config(),
        enroller,
        std::future::pending(),
    ));
    let rotator = Rotator::new(
        pool.clone(),
        pki.ca.clone(),
        crl,
        support::paths(pki.dir.path(), "server"),
        vec![ServerName::Dns("localhost".into())],
    );
    rotator.backfill().await.unwrap();
    Server {
        url,
        enroll_url,
        rotator,
    }
}

/// A peer's renewal task, checking every 50 ms as it does every hour.
fn run_renewal(
    pki: &Pki,
    url: &str,
    identity: StoredIdentity,
) -> (watch::Receiver<StoredIdentity>, Trigger) {
    let settings = RenewalSettings {
        check_every: Duration::from_millis(50),
        jitter: Duration::ZERO,
        backoff: Backoff::new(Duration::from_millis(50), Duration::from_millis(200)),
        ..RenewalSettings::for_upstream(url)
    };
    let paths = support::paths(pki.dir.path(), identity.id.id());
    let (renewal, identities) = Renewal::new(settings, paths, identity);
    let (trigger, stop) = shutdown::channel();
    tokio::spawn(renewal.run(stop));
    (identities, trigger)
}

async fn connects(url: &str, identity: &StoredIdentity) -> bool {
    match open(url, identity, hello(identity.id.id())).await {
        Ok(_) => true,
        Err(status) => {
            eprintln!("{} refused: {status}", identity.id);
            false
        }
    }
}

async fn fleet(server: &Server) -> Vec<ca_rotation::PeerState> {
    ca_rotation::fleet(server.rotator.pool(), server.rotator.ca())
        .await
        .unwrap()
}

async fn all(server: &Server, f: impl Fn(&ca_rotation::PeerState) -> bool) -> bool {
    fleet(server).await.iter().all(f)
}

fn stage(server: &Server) -> Option<Stage> {
    server.rotator.ca().rotation().map(|r| r.stage)
}

async fn audited(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_planned_change_moves_agents_and_a_relay_without_a_restart(pool: PgPool) {
        let pki = Pki::new();
        let server = start_server(&pki, &pool).await;
        let relay = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Relay, "paris-1", "paris", vec!["localhost".into()]).await;
        // Behind the relay, direct, and offline when the change starts.
        let web01 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-01", "paris", vec![]).await;
        let web02 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-02", "paris", vec![]).await;
        let web03 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-03", "paris", vec![]).await;

        let (relay_ids, _relay_renewal) = run_renewal(&pki, &server.url, relay);
        let relay = Relay::start_following(relay_ids.clone(), &server.url, BufferOptions::temp()).await;
        let (web01_ids, _r1) = run_renewal(&pki, &relay.url, web01);
        let (web02_ids, _r2) = run_renewal(&pki, &server.url, web02.clone());
        assert!(connects(&relay.url, &web01_ids.borrow()).await);
        assert!(connects(&server.url, &web02).await);
        let old = server.rotator.ca().fingerprint();

        // 1. Introduced: online peers learn the new CA over mTLS.
        server.rotator.start(false, Actor::System).await.unwrap();
        assert_eq!(stage(&server), Some(Stage::Introduced));
        assert_eq!(server.rotator.ca().fingerprint(), old, "the old CA still issues");
        wait_for("the online peers trust the new CA", || async {
            fleet(&server).await.iter().filter(|p| p.trusts_all).count() == 3
        }).await;
        // The server records a certificate just before the peer stores it.
        for ids in [&web01_ids, &relay_ids] {
            wait_for("the peer stored both CAs", || async { ids.borrow().ca_fingerprints.len() == 2 }).await;
        }

        // The offline agent holds the switch while its certificate is valid.
        server.rotator.advance().await.unwrap();
        assert_eq!(stage(&server), Some(Stage::Introduced));
        let waiting: Vec<_> = fleet(&server).await.into_iter()
            .filter(|p| ca_rotation::is_waiting(Stage::Introduced, p))
            .map(|p| p.peer_id)
            .collect();
        assert_eq!(waiting, ["web-03"]);

        // It comes back, picks the new CA up, and the switch follows.
        let (web03_ids, _r3) = run_renewal(&pki, &server.url, web03);
        wait_for("every peer trusts the new CA", || all(&server, |p| p.trusts_all)).await;
        server.rotator.advance().await.unwrap();
        assert_eq!(stage(&server), Some(Stage::Switched));
        let new = server.rotator.ca().fingerprint();
        assert_ne!(new, old);

        // 2. Switched: the server presents its new certificate at once, and
        // every peer gets one from the new CA within a check.
        let server_cert = tls::load_certs(&support::paths(pki.dir.path(), "server").cert).unwrap();
        assert!(pki.ca.verify_issued(&server_cert[0]).is_ok());
        wait_for("every peer moved", || all(&server, |p| p.moved)).await;
        for ids in [&web01_ids, &web02_ids, &web03_ids, &relay_ids] {
            wait_for("the new identity is stored", || async { ids.borrow().ca_fingerprints[0] == new }).await;
        }
        assert!(connects(&relay.url, &web01_ids.borrow()).await, "through the relay with the new CA");
        assert!(connects(&server.url, &web02_ids.borrow()).await);
        // A certificate of the old CA is still accepted until it is retired.
        assert!(pki.ca.verify_issued(web02.tls.leaf()).is_ok());

        // 3. Retired: peers drop the old CA, which nothing accepts any more.
        server.rotator.advance().await.unwrap();
        assert_eq!(stage(&server), None);
        for ids in [&web01_ids, &web02_ids, &web03_ids, &relay_ids] {
            wait_for("the old CA is dropped", || async { ids.borrow().ca_fingerprints == [new] }).await;
        }
        assert!(connects(&relay.url, &web01_ids.borrow()).await);
        assert!(connects(&server.url, &web02_ids.borrow()).await);
        assert!(pki.ca.verify_issued(web02.tls.leaf()).is_err(), "the old CA is no longer trusted");
        assert!(fleet(&server).await.iter().all(|p| !p.cut_off));

        for action in ["ca.rotation.start", "ca.rotation.switch", "ca.rotation.retire"] {
            assert_eq!(audited(&pool, action).await, 1, "{action}");
        }
        // New tokens pin the new CA, and enrollment works with them.
        let web04 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-04", "paris", vec![]).await;
        assert_eq!(web04.ca_fingerprints, [new]);
        assert!(connects(&server.url, &web04).await);
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn an_emergency_change_cuts_off_who_misses_it(pool: PgPool) {
        let pki = Pki::new();
        let server = start_server(&pki, &pool).await;
        let web01 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-01", "paris", vec![]).await;
        let web02 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-02", "paris", vec![]).await;
        let (web01_ids, _r1) = run_renewal(&pki, &server.url, web01);
        let old = server.rotator.ca().fingerprint();

        // Switched at once, the old CA distrusted in a day.
        server.rotator.start(true, Actor::System).await.unwrap();
        let rotation = server.rotator.ca().rotation().unwrap();
        assert!(rotation.emergency && rotation.stage == Stage::Switched);
        assert!(rotation.distrust_at.is_some());
        let new = server.rotator.ca().fingerprint();
        wait_for("the online agent moved", || async {
            web01_ids.borrow().ca_fingerprints[0] == new
        }).await;
        // The offline one holds nothing back before the distrust time.
        server.rotator.advance().await.unwrap();
        assert_eq!(stage(&server), Some(Stage::Switched));

        // "Distrust the old authority now".
        server.rotator.finish_now(Actor::System).await.unwrap();
        assert_eq!(stage(&server), None);
        wait_for("the online agent dropped the old CA", || async {
            web01_ids.borrow().ca_fingerprints == [new]
        }).await;
        assert!(connects(&server.url, &web01_ids.borrow()).await);
        assert!(!connects(&server.url, &web02).await, "web-02 missed the window");
        assert!(pki.ca.verify_issued(web02.tls.leaf()).is_err(), "and the server refuses its certificate");
        let cut_off: Vec<_> = fleet(&server).await.into_iter().filter(|p| p.cut_off).map(|p| p.peer_id).collect();
        assert_eq!(cut_off, ["web-02"]);
        let retire: serde_json::Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'ca.rotation.retire'",
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(retire["not_moved"], 1);
        assert_eq!(retire["old_ca"], tls::fingerprint_hex(&old));

        // A new install, with a new token, brings it back.
        std::fs::remove_dir_all(pki.dir.path().join("web-02")).unwrap();
        let web02 = enroll_as(&pki, &pool, &server.enroll_url, PeerRole::Agent, "web-02", "paris", vec![]).await;
        assert!(connects(&server.url, &web02).await);
        assert!(fleet(&server).await.iter().all(|p| !p.cut_off && p.moved));
    }
}
