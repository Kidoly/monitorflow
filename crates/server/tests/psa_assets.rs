//! Servers synced as PSA assets (ADR-0164, ADR-0187, ADR-0191): created
//! with an External Link, matched to existing assets for confirmation,
//! updated only when something changed, and marked inactive on removal
//! when asked. Against an in-process stand-in for HaloPSA, never a real
//! instance. Ignored without `DATABASE_URL`.

mod fake_halo;

use std::sync::Arc;

use fake_halo::{FakeHalo, CLIENT_ID, CLIENT_SECRET};
use monitorflow_server::psa::assets::AssetSync;
use monitorflow_server::psa::halopsa::Limiter;
use monitorflow_server::secrets::{self, Column};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const WEB: &str = "https://mf.example.com";

macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

/// A connection with asset sync on, Linux servers as the type 3 with the
/// host name as asset tag and the serial number, maker and model in their
/// fields, and a site matched to the client 10 and its site 100:
/// `(connection, site)`.
async fn setup(pool: &PgPool, halo: &FakeHalo) -> (Uuid, Uuid) {
    let secret = secrets::seal(Column::PsaConnections, CLIENT_SECRET).unwrap();
    let connection: Uuid = sqlx::query_scalar(
        "INSERT INTO psa_connections (base_url, client_id, client_secret, asset_sync)
         VALUES ($1, $2, $3, true) RETURNING id",
    )
    .bind(&halo.url)
    .bind(CLIENT_ID)
    .bind(secret)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO psa_asset_types (connection_id, family, type_id, type_name, hostname_field,
                                      serial_field, manufacturer_field, model_field)
         VALUES ($1, 'linux', 3, 'Server', 'inventory_number', '12', '13', '14')",
    )
    .bind(connection)
    .execute(pool)
    .await
    .unwrap();
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
    )
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO psa_site_links (site_id, connection_id, client_id, client_name, halo_site_id,
                                     halo_site_name)
         VALUES ($1, $2, 10, 'Example Industries', 100, 'Main office')",
    )
    .bind(site)
    .bind(connection)
    .execute(pool)
    .await
    .unwrap();
    (connection, site)
}

/// A server with a system item giving its serial number.
async fn server(pool: &PgPool, site: Uuid, hostname: &str, family: &str, serial: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($2, $1, $2, $3) RETURNING id",
    )
    .bind(site)
    .bind(hostname)
    .bind(family)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO server_inventory_items (server_id, kind, key, fields)
         VALUES ($1, 'system', 'system', $2)",
    )
    .bind(id)
    .bind(json!({"manufacturer": "Dell Inc.", "model": "PowerEdge R650", "serial": serial}))
    .execute(pool)
    .await
    .unwrap();
    id
}

fn sync(pool: &PgPool) -> AssetSync {
    AssetSync::with_limiter(pool.clone(), WEB, Arc::new(Limiter::new(600)))
}

async fn link(pool: &PgPool, server: Uuid) -> Option<(String, Option<i64>, Option<String>)> {
    sqlx::query_as("SELECT state, asset_id, matched_by FROM psa_asset_links WHERE server_id = $1")
        .bind(server)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// As if the last push was over an hour ago.
async fn an_hour_later(pool: &PgPool) {
    sqlx::query("UPDATE psa_asset_links SET pushed_at = now() - interval '2 hours'")
        .execute(pool)
        .await
        .unwrap();
}

db_tests! {
    #[sqlx::test]
    async fn a_server_becomes_an_asset_and_is_updated_only_when_it_changes(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (_, site) = setup(&pool, &halo).await;
        let web = server(&pool, site, "web-01", "debian", "SN123").await;
        // Windows has no asset type: not synced.
        let windows = server(&pool, site, "win-01", "windows", "SN999").await;
        assert_eq!(sync(&pool).pass().await.unwrap(), 1);
        let posted = halo.asset_posts();
        assert_eq!(posted.len(), 1);
        let created = &posted[0];
        assert_eq!(created["assettype_id"], 3);
        assert_eq!((created["client_id"].as_i64(), created["site_id"].as_i64()), (Some(10), Some(100)));
        assert_eq!(created["inventory_number"], "web-01");
        assert_eq!(
            created["fields"],
            json!([{"id": 12, "value": "SN123"}, {"id": 13, "value": "Dell Inc."}, {"id": 14, "value": "PowerEdge R650"}])
        );
        assert_eq!(created["new_external_link"]["integration_name"], "MonitorFlow");
        assert_eq!(created["new_external_link"]["third_party_id"], web.to_string());
        assert_eq!(
            created["new_external_link"]["third_party_url"],
            format!("{WEB}/servers/view/?id={web}")
        );
        assert_eq!(link(&pool, web).await, Some(("linked".into(), Some(7001), Some("created".into()))));
        assert_eq!(link(&pool, windows).await, None);

        // Within the hour, or unchanged: nothing sent.
        assert_eq!(sync(&pool).pass().await.unwrap(), 0);
        an_hour_later(&pool).await;
        sync(&pool).pass().await.unwrap();
        assert_eq!(halo.asset_posts().len(), 1);

        // Renamed: one update, without a second link.
        sqlx::query("UPDATE servers SET hostname = 'web-01b' WHERE id = $1")
            .bind(web).execute(&pool).await.unwrap();
        an_hour_later(&pool).await;
        sync(&pool).pass().await.unwrap();
        let posted = halo.asset_posts();
        assert_eq!(posted.len(), 2);
        assert_eq!(posted[1]["id"], 7001);
        assert_eq!(posted[1]["inventory_number"], "web-01b");
        assert!(posted[1].get("new_external_link").is_none());
    }

    #[sqlx::test]
    async fn an_existing_asset_is_proposed_by_serial_then_host_name(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (_, site) = setup(&pool, &halo).await;
        halo.add_asset(json!({"id": 900, "client_id": 10, "assettype_id": 3, "key_field": "OLD-NAME",
                              "fields": [{"id": 12, "name": "Serial Number", "value": "sn123"}]}));
        halo.add_asset(json!({"id": 901, "client_id": 10, "assettype_id": 3, "key_field": "DB-01",
                              "fields": []}));
        // Another client's asset of the same name is not this one.
        halo.add_asset(json!({"id": 902, "client_id": 11, "assettype_id": 3, "key_field": "app-01",
                              "fields": []}));
        let web = server(&pool, site, "web-01", "debian", "SN123").await;
        let db = server(&pool, site, "db-01", "debian", "").await;
        let app = server(&pool, site, "app-01", "rhel", "").await;
        sync(&pool).pass().await.unwrap();
        assert_eq!(link(&pool, web).await, Some(("proposed".into(), Some(900), Some("serial".into()))));
        assert_eq!(link(&pool, db).await, Some(("proposed".into(), Some(901), Some("hostname".into()))));
        assert_eq!(link(&pool, app).await.unwrap().0, "linked");
        // Only app-01 was created; proposals wait for a person.
        assert_eq!(halo.asset_posts().len(), 1);
        an_hour_later(&pool).await;
        sync(&pool).pass().await.unwrap();
        assert_eq!(halo.asset_posts().len(), 1);

        // Confirmed (as the API does): the next pass pushes and links back.
        sqlx::query("UPDATE psa_asset_links SET state = 'linked', pushed_hash = NULL, pushed_at = NULL WHERE server_id = $1")
            .bind(web).execute(&pool).await.unwrap();
        sync(&pool).pass().await.unwrap();
        let posted = halo.asset_posts();
        assert_eq!(posted.len(), 2);
        assert_eq!(posted[1]["id"], 900);
        assert_eq!(posted[1]["new_external_link"]["third_party_id"], web.to_string());
    }

    #[sqlx::test]
    async fn a_removed_server_s_asset_is_marked_inactive_only_when_asked(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (_, site) = setup(&pool, &halo).await;
        let web = server(&pool, site, "web-01", "debian", "SN123").await;
        sync(&pool).pass().await.unwrap();
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(web).execute(&pool).await.unwrap();
        an_hour_later(&pool).await;
        // Left as it is by default.
        sync(&pool).pass().await.unwrap();
        assert_eq!(halo.asset_posts().len(), 1);
        sqlx::query("UPDATE psa_connections SET asset_inactive_on_removal = true")
            .execute(&pool).await.unwrap();
        sync(&pool).pass().await.unwrap();
        let posted = halo.asset_posts();
        assert_eq!(posted.len(), 2);
        assert_eq!(posted[1]["id"], 7001);
        assert_eq!(posted[1]["inactive"], true);
        // Once.
        an_hour_later(&pool).await;
        sync(&pool).pass().await.unwrap();
        assert_eq!(halo.asset_posts().len(), 2);
        assert_eq!(halo.assets()[0]["inactive"], true);
    }

    #[sqlx::test]
    async fn nothing_is_synced_while_the_sync_is_off(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let (_, site) = setup(&pool, &halo).await;
        sqlx::query("UPDATE psa_connections SET asset_sync = false").execute(&pool).await.unwrap();
        server(&pool, site, "web-01", "debian", "SN123").await;
        assert_eq!(sync(&pool).pass().await.unwrap(), 0);
        assert!(halo.requests().is_empty());
    }
}
