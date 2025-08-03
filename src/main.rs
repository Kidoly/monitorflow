use axum::{
    routing::post,
    Router,
    Json,
    extract::State,
    http::StatusCode
};
use serde::Deserialize;
use sqlx::{PgPool, Row};
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
    let device_uid = match uuid::Uuid::parse_str(&payload.device_uid) {
        Ok(uid) => uid,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid UUID".into())),
    };
    let mut tx = state.db.begin().await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to begin transaction: {}", e))
    })?;
    // Insert device if not exists
    sqlx::query(
        r#"
        INSERT INTO device (uid, name, device_type, display_name)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (uid) DO NOTHING
        "#,
    )
    .bind(&device_uid)
    .bind(&payload.name)
    .bind(&payload.device_type)
    .bind(&payload.name)
    .execute(&mut *tx)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Device insert error: {}", e)))?;
    for metric in &payload.metrics {
        // Insert sensor if not exists
        let sensor = sqlx::query(
            r#"
            INSERT INTO sensor (device_uid, sensor_type, name, display_name)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT DO NOTHING
            RETURNING id
            "#,
        )
        .bind(device_uid)
        .bind(&metric.sensor)
        .bind(&metric.sensor)
        .bind(&metric.sensor)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Sensor insert error: {}", e)))?;
        let sensor_id: i32 = if let Some(row) = sensor {
            row.get("id")
        } else {
            sqlx::query("SELECT id FROM sensor WHERE device_uid = $1 AND sensor_type = $2")
                .bind(device_uid)
                .bind(&metric.sensor)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| {
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("Sensor lookup failed: {}", e))
                })?
                .get("id")
        };
        for reading in &metric.readings {
            // Insert channel if not exists
            let channel = sqlx::query(
                r#"
                INSERT INTO channel (sensor_id, name)
                VALUES ($1, $2)
                ON CONFLICT DO NOTHING
                RETURNING id
                "#,
            )
            .bind(sensor_id)
            .bind(&reading.channel)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Channel insert error: {}", e)))?;
            let channel_id: i32 = if let Some(row) = channel {
                row.get("id")
            } else {
                sqlx::query("SELECT id FROM channel WHERE sensor_id = $1 AND name = $2")
                    .bind(sensor_id)
                    .bind(&reading.channel)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| {
                        (StatusCode::INTERNAL_SERVER_ERROR, format!("Channel lookup failed: {}", e))
                    })?
                    .get("id")
            };
            // Insert reading
            sqlx::query(
                r#"
                INSERT INTO sensor_reading (sensor_id, channel_id, timestamp, value)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(sensor_id)
            .bind(channel_id)
            .bind(reading.timestamp)
            .bind(reading.value)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Reading insert error: {}", e))
            })?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Transaction commit failed: {}", e)))?;
    Ok(StatusCode::OK)
}
