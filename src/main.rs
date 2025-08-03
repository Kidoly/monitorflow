use axum::{
    routing::post,
    Router,
    Json,
    extract::State,
    http::StatusCode
};
use serde::Deserialize;
use sqlx::PgPool;
use std::{env, net::SocketAddr};
use chrono::{DateTime, Utc};
use dotenv::dotenv;
use tokio::net::TcpListener;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db: PgPool,
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");
    let state = AppState { db };
    let app = Router::new()
        .route("/collect", post(collect_handler))
        .with_state(state);
    let addr: SocketAddr = "0.0.0.0:8000".parse().unwrap();
    println!("🚀 Backend listening on {}", addr);
    let listener = TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[derive(Debug, Deserialize)]
struct Reading {
    channel: String,
    timestamp: DateTime<Utc>,
    value: f64,
}

#[derive(Debug, Deserialize)]
struct Metric {
    sensor: String,
    readings: Vec<Reading>,
}

#[derive(Debug, Deserialize)]
struct CollectPayload {
    device_uid: String,
    name: String,
    #[serde(rename = "type")]
    device_type: String,
    metrics: Vec<Metric>,
}

async fn collect_handler(
    State(state): State<AppState>,
    Json(payload): Json<CollectPayload>,
) -> Result<StatusCode, (StatusCode, String)> {
    let device_uid = match Uuid::parse_str(&payload.device_uid) {
        Ok(uid) => uid,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid UUID".into())),
    };

    let mut tx = state.db.begin().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to begin transaction: {}", e))
    })?;

    // Insert device if not exists
    sqlx::query!(
        r#"
        INSERT INTO device (uid, name, device_type, display_name)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (uid) DO NOTHING
        "#,
        device_uid,
        payload.name,
        payload.device_type,
        payload.name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Device insert error: {}", e)))?;

    for metric in &payload.metrics {
        // Attempt to insert sensor and get the ID
        let sensor_id = match sqlx::query_scalar!(
            r#"
            INSERT INTO sensor (device_uid, sensor_type, name)
            VALUES ($1, $2, $3)
            ON CONFLICT (device_uid, sensor_type, name) DO NOTHING
            RETURNING id
            "#,
            device_uid,
            metric.sensor,
            metric.sensor
        )
        .fetch_optional(&mut *tx)
        .await {
            Ok(sensor) => sensor,
            Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Sensor insert error: {}", e))),
        };

        let sensor_id = match sensor_id {
            Some(id) => id,
            None => {
                match sqlx::query_scalar!(
                    r#"SELECT id FROM sensor WHERE device_uid = $1 AND sensor_type = $2"#,
                    device_uid,
                    metric.sensor
                )
                .fetch_one(&mut *tx)
                .await {
                    Ok(id) => id,
                    Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get sensor ID: {}", e))),
                }
            }
        };

        for reading in &metric.readings {
            // Check if the channel already exists
            let channel_id = match sqlx::query_scalar!(
                r#"
                SELECT id FROM channel WHERE sensor_id = $1 AND name = $2
                "#,
                sensor_id,
                reading.channel
            )
            .fetch_optional(&mut *tx)
            .await {
                Ok(channel) => channel,
                Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Channel lookup error: {}", e))),
            };

            let channel_id = match channel_id {
                Some(id) => id,
                None => {
                    match sqlx::query_scalar!(
                        r#"
                        INSERT INTO channel (sensor_id, name)
                        VALUES ($1, $2)
                        RETURNING id
                        "#,
                        sensor_id,
                        reading.channel
                    )
                    .fetch_one(&mut *tx)
                    .await {
                        Ok(id) => id,
                        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get channel ID: {}", e))),
                    }
                }
            };

            // Insert reading with conflict handling
            match sqlx::query!(
                r#"
                INSERT INTO sensor_reading (sensor_id, channel_id, timestamp, value)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (sensor_id, channel_id, timestamp)
                DO UPDATE SET value = EXCLUDED.value
                "#,
                sensor_id,
                channel_id,
                reading.timestamp,
                reading.value
            )
            .execute(&mut *tx)
            .await {
                Ok(_) => (),
                Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("Reading insert error: {}", e))),
            };
        }
    }

    tx.commit()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Transaction commit failed: {}", e)))?;

    Ok(StatusCode::OK)
}

