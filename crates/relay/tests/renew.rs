//! Agents renew their certificates through their site's relay (ADR-0028),
//! with the real renewal client, relay and server pieces over mTLS and the
//! server's database.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::renew::{RenewError, Renewal, RenewalSettings};
use monitorflow_common::client::shutdown;
use monitorflow_common::proto::v1::certificate_service_client::CertificateServiceClient;
use monitorflow_common::proto::v1::ForwardedRenewRequest;
use monitorflow_common::tls::{self, PeerRole};
use monitorflow_relay::names;
use monitorflow_server::ca::RevocationReason;
use monitorflow_server::grpc::{AgentStreamService, DbInventory, DbRelayPolicy};
use monitorflow_server::renew::{PresentedCert, Renewer};
use sqlx::PgPool;
use support::{enroll_as, serve_enroll, server_settings, Pki, Relay, TIMEOUT};
use tonic::Code;

/// One renewal attempt by `agent` against `url`, as its renewal task makes.
async fn renew(pki: &Pki, url: &str, agent: &StoredIdentity) -> Result<StoredIdentity, RenewError> {
    let paths = support::paths(pki.dir.path(), agent.id.id());
    let (renewal, _rx) = Renewal::new(RenewalSettings::for_upstream(url), paths, agent.clone());
    renewal.renew(agent).await
}

fn assert_refused(result: Result<StoredIdentity, RenewError>, code: &str, why: &str) {
    let error = result.unwrap_err().to_string();
    assert!(error.contains(code) && error.contains(why), "{error}");
}

fn serial(identity: &StoredIdentity) -> monitorflow_server::ca::Serial {
    PresentedCert::from_der(identity.tls.leaf()).unwrap().serial
}

async fn backdate_certificates(pool: &PgPool) {
    sqlx::query("UPDATE certificates SET issued_at = now() - interval '2 hours'")
        .execute(pool)
        .await
        .unwrap();
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_waiting_relay_gets_its_names_once_confirmed(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = serve_enroll(&pki, &pool).await;
        let relay = enroll_as(&pki, &pool, &enroll_url, PeerRole::Relay, "paris-1", "paris", vec![]).await;
        assert!(names::lacks_names(&relay));
        let upstream = pki.serve_with_renewer(|registry| {
            AgentStreamService::new(registry, Arc::new(DbRelayPolicy::new(pool.clone())), server_settings())
                .with_inventory(Arc::new(DbInventory::new(pool.clone())))
        }, Some(Renewer::new(pool.clone(), pki.ca.clone()))).await;

        let settings = RenewalSettings {
            names_every: Duration::from_millis(50),
            ..RenewalSettings::for_upstream(&upstream.url)
        };
        let paths = support::paths(pki.dir.path(), "paris-1");
        let (renewal, identities) = Renewal::new(settings, paths, relay);
        let (trigger, stop) = shutdown::channel();
        tokio::spawn(renewal.awaiting_names(names::lacks_names).run(stop.clone()));
        let confirmed = tokio::spawn(names::confirmed(identities.clone(), stop));

        // Asking changes nothing while nobody confirmed.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!confirmed.is_finished());
        let issued: i64 = sqlx::query_scalar("SELECT count(*) FROM certificates WHERE peer_id = 'paris-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(issued, 1);

        sqlx::query("UPDATE relays SET confirmed_names = '{relay.paris.example}', pending_names = NULL")
            .execute(&pool)
            .await
            .unwrap();
        let confirmed = tokio::time::timeout(TIMEOUT, confirmed).await.unwrap().unwrap();
        assert_eq!(confirmed.unwrap(), ["relay.paris.example"]);
        assert!(!names::lacks_names(&identities.borrow()));
        trigger.fire();
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn agents_renew_through_their_sites_relay(pool: PgPool) {
        let pki = Pki::new();
        let enroll_url = serve_enroll(&pki, &pool).await;
        let relay_id = enroll_as(&pki, &pool, &enroll_url, PeerRole::Relay, "paris-1", "paris", vec!["localhost".into()]).await;
        let web01 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-01", "paris", vec![]).await;
        let web02 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-02", "lyon", vec![]).await;
        let web03 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-03", "paris", vec![]).await;
        let web04 = enroll_as(&pki, &pool, &enroll_url, PeerRole::Agent, "web-04", "paris", vec![]).await;

        // The production limit: one certificate an hour per peer.
        let renewer = Renewer::new(pool.clone(), pki.ca.clone());
        let upstream = pki.serve_with_renewer(|registry| {
            AgentStreamService::new(registry, Arc::new(DbRelayPolicy::new(pool.clone())), server_settings())
                .with_inventory(Arc::new(DbInventory::new(pool.clone())))
        }, Some(renewer)).await;
        let relay = Relay::start(relay_id, &upstream.url).await;

        // The enrollment counts towards the limit of the agent.
        assert_refused(renew(&pki, &relay.url, &web01).await, "ResourceExhausted", "less than");

        // Renewal through the relay, for the agent's own identity.
        backdate_certificates(&pool).await;
        let renewed = renew(&pki, &relay.url, &web01).await.unwrap();
        assert_eq!(renewed.id, web01.id);
        assert_ne!(serial(&renewed), serial(&web01));
        let via: Option<String> = sqlx::query_scalar(
            "SELECT details->>'via_relay' FROM audit_log WHERE action = 'peer.renew' AND target_id = 'web-01'",
        )
        .fetch_one(&pool).await.unwrap();
        assert_eq!(via.as_deref(), Some("paris-1"));

        // The limit is per agent, not per relay.
        assert_refused(renew(&pki, &relay.url, &renewed).await, "ResourceExhausted", "less than");
        renew(&pki, &relay.url, &web03).await.unwrap();

        // An agent of another site is refused by the relay policy.
        assert_refused(renew(&pki, &relay.url, &web02).await, "PermissionDenied", "relay paris-1 may not renew for agent web-02");

        // A revoked agent is refused: by its inventory row, which the relay
        // policy checks, or by the CA's list even before the relay's copy
        // of the CRL has it.
        sqlx::query(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, revoked_at)
             SELECT 'web-03', id, 'web-03', 'debian', now() FROM sites WHERE name = 'paris'",
        )
        .execute(&pool).await.unwrap();
        backdate_certificates(&pool).await;
        assert_refused(renew(&pki, &relay.url, &web03).await, "PermissionDenied", "may not renew for agent web-03");
        pki.ca.revoke(&serial(&web04), RevocationReason::KeyCompromise).unwrap();
        assert_refused(renew(&pki, &relay.url, &web04).await, "PermissionDenied", "revoked");

        // Only relays may forward: an agent calling the server directly
        // cannot renew for another agent of its site.
        let config = web01.tls.client_config().unwrap();
        let channel = tls::connect(&upstream.url, config, TIMEOUT).await.unwrap();
        let status = CertificateServiceClient::new(channel)
            .renew_forwarded(ForwardedRenewRequest {
                names_only: false,
                csr_pem: String::new(),
                peer_cert_der: web03.tls.leaf().to_vec(),
            })
            .await
            .unwrap_err();
        assert_eq!(status.code(), Code::PermissionDenied, "{status}");

        let renewals: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'peer.renew'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(renewals, 2);
    }
}
