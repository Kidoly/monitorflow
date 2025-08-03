CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE "access" (
  "id" SERIAL PRIMARY KEY,
  "name" VARCHAR(255) NOT NULL,
  "description" TEXT NOT NULL,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "tag" (
  "id" SERIAL PRIMARY KEY,
  "name" TEXT NOT NULL,
  "description" TEXT,
  "color" TEXT,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "device" (
  "uid" UUID PRIMARY KEY,
  "name" TEXT NOT NULL,
  "display_name" TEXT,
  "description" TEXT,
  "location" TEXT,
  "device_type" TEXT,
  "active" BOOLEAN NOT NULL DEFAULT true,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "sensor" (
  "id" SERIAL PRIMARY KEY,
  "device_uid" UUID NOT NULL,
  "sensor_type" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "display_name" TEXT,
  "interval_sec" INT NOT NULL DEFAULT 60,
  "active" BOOLEAN NOT NULL DEFAULT true,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "channel" (
  "id" SERIAL PRIMARY KEY,
  "sensor_id" INT NOT NULL,
  "name" TEXT NOT NULL,
  "unit" TEXT,
  "limit_low" DOUBLE PRECISION,
  "limit_high" DOUBLE PRECISION,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "sensor_reading" (
  "sensor_id" INT NOT NULL,
  "channel_id" INT NOT NULL,
  "timestamp" TIMESTAMPTZ NOT NULL,
  "value" DOUBLE PRECISION NOT NULL,
  "message" TEXT,
  PRIMARY KEY ("sensor_id", "channel_id", "timestamp")
);

CREATE TABLE "log" (
  "id" BIGSERIAL PRIMARY KEY,
  "timestamp" TIMESTAMPTZ NOT NULL DEFAULT (now()),
  "object_type" TEXT NOT NULL,
  "object_id" INT,
  "level" TEXT NOT NULL,
  "message" TEXT NOT NULL
);

CREATE TABLE "user" (
  "id" SERIAL PRIMARY KEY,
  "first_name" TEXT NOT NULL,
  "last_name" TEXT NOT NULL,
  "email" TEXT UNIQUE NOT NULL,
  "access_id" INT NOT NULL,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "dashboard" (
  "id" SERIAL PRIMARY KEY,
  "name" TEXT NOT NULL,
  "user_id" INT NOT NULL,
  "is_global" BOOLEAN DEFAULT false,
  "created_at" TIMESTAMPTZ NOT NULL DEFAULT (now())
);

CREATE TABLE "dashboard_widget" (
  "id" SERIAL PRIMARY KEY,
  "dashboard_id" INT NOT NULL,
  "title" TEXT,
  "type" TEXT NOT NULL,
  "config" JSON,
  "layout" JSON
);

CREATE TABLE "device_tag" (
  "device_uid" UUID,
  "tag_id" INT NOT NULL,
  PRIMARY KEY ("device_uid", "tag_id")
);

CREATE INDEX "idx_log_timestamp" ON "log" ("timestamp");

ALTER TABLE "sensor" ADD FOREIGN KEY ("device_uid") REFERENCES "device" ("uid") ON DELETE CASCADE;

ALTER TABLE "channel" ADD FOREIGN KEY ("sensor_id") REFERENCES "sensor" ("id") ON DELETE CASCADE;

ALTER TABLE "sensor_reading" ADD FOREIGN KEY ("sensor_id") REFERENCES "sensor" ("id") ON DELETE CASCADE;

ALTER TABLE "sensor_reading" ADD FOREIGN KEY ("channel_id") REFERENCES "channel" ("id") ON DELETE CASCADE;

ALTER TABLE "user" ADD FOREIGN KEY ("access_id") REFERENCES "access" ("id");

ALTER TABLE "device_tag" ADD FOREIGN KEY ("device_uid") REFERENCES "device" ("uid");

ALTER TABLE "device_tag" ADD FOREIGN KEY ("tag_id") REFERENCES "tag" ("id");

ALTER TABLE "dashboard_widget" ADD FOREIGN KEY ("dashboard_id") REFERENCES "dashboard" ("id") ON DELETE CASCADE;

ALTER TABLE "dashboard" ADD FOREIGN KEY ("user_id") REFERENCES "user" ("id") ON DELETE CASCADE;

-- Pas besoin de PRIMARY KEY si tu n'en veux pas
-- Tu peux créer un index simple si besoin :
CREATE INDEX idx_sensor_reading_timestamp ON sensor_reading (timestamp);

-- Et créer l’hypertable
SELECT create_hypertable('sensor_reading', 'timestamp');

CREATE INDEX idx_sensor_device_uid ON sensor(device_uid);
CREATE INDEX idx_channel_sensor_id ON channel(sensor_id);
CREATE INDEX idx_sensor_reading_sensor_channel ON sensor_reading(sensor_id, channel_id);

ALTER TABLE sensor_reading SET (
  timescaledb.compress,
  timescaledb.compress_segmentby = 'sensor_id,channel_id'
);

SELECT add_compression_policy('sensor_reading', INTERVAL '30 days');
