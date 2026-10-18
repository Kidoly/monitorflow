//! Prometheus remote write to VictoriaMetrics: protobuf `WriteRequest`,
//! snappy block compression, POST to `/api/v1/write`.

use super::http;
use super::validate::Series;
use super::writer::{Sink, WriteError};

/// The messages of Prometheus' `remote.proto` that the server sends, defined
/// here so no `.proto` copy or code generation is needed.
pub mod remote_write {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct WriteRequest {
        #[prost(message, repeated, tag = "1")]
        pub timeseries: Vec<TimeSeries>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct TimeSeries {
        #[prost(message, repeated, tag = "1")]
        pub labels: Vec<Label>,
        #[prost(message, repeated, tag = "2")]
        pub samples: Vec<Sample>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Label {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "2")]
        pub value: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Sample {
        #[prost(double, tag = "1")]
        pub value: f64,
        #[prost(int64, tag = "2")]
        pub timestamp: i64,
    }
}

/// The request body for `batch`: one time series per sample, labels
/// already sorted by [`Series`].
pub fn encode(batch: &[Series]) -> Result<Vec<u8>, snap::Error> {
    use prost::Message;
    let request = remote_write::WriteRequest {
        timeseries: batch
            .iter()
            .map(|s| remote_write::TimeSeries {
                labels: s
                    .labels
                    .iter()
                    .map(|(name, value)| remote_write::Label {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect(),
                samples: vec![remote_write::Sample {
                    value: s.value,
                    timestamp: s.timestamp_ms,
                }],
            })
            .collect(),
    };
    snap::raw::Encoder::new().compress_vec(&request.encode_to_vec())
}

/// Decodes a body made by [`encode`]; for tests and debugging.
pub fn decode(body: &[u8]) -> Result<remote_write::WriteRequest, String> {
    use prost::Message;
    let raw = snap::raw::Decoder::new()
        .decompress_vec(body)
        .map_err(|e| e.to_string())?;
    remote_write::WriteRequest::decode(raw.as_slice()).map_err(|e| e.to_string())
}

/// The VictoriaMetrics [`Sink`].
#[derive(Debug, Clone)]
pub struct RemoteWrite {
    client: reqwest::Client,
    url: String,
}

impl RemoteWrite {
    /// `base_url` is `victoriametrics_url`, like `http://localhost:8428`.
    pub fn new(base_url: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: http::client()?,
            url: format!("{}/api/v1/write", base_url.trim_end_matches('/')),
        })
    }
}

impl Sink for RemoteWrite {
    type Item = Series;

    fn name(&self) -> &'static str {
        "victoriametrics"
    }

    async fn write(&self, batch: &[Series]) -> Result<(), WriteError> {
        let body = encode(batch).map_err(|e| WriteError::Permanent(e.to_string()))?;
        let request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/x-protobuf")
            .header("Content-Encoding", "snappy")
            .header("X-Prometheus-Remote-Write-Version", "0.1.0")
            .body(body);
        http::send(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_batches_decode_to_the_same_series() {
        let series = Series {
            labels: vec![
                ("__name__".into(), "monitorflow_cpu".into()),
                ("peer".into(), "web-01".into()),
            ],
            timestamp_ms: 1_800_000_000_000,
            value: 0.5,
        };
        let request = decode(&encode(&[series.clone(), series]).unwrap()).unwrap();
        assert_eq!(request.timeseries.len(), 2);
        let ts = &request.timeseries[0];
        assert_eq!(ts.labels[0].name, "__name__");
        assert_eq!(ts.labels[1].value, "web-01");
        assert_eq!(ts.samples[0].timestamp, 1_800_000_000_000);
        assert_eq!(ts.samples[0].value, 0.5);
    }

    #[test]
    fn the_write_path_is_appended_to_the_base_url() {
        let sink = RemoteWrite::new("http://vm:8428/").unwrap();
        assert_eq!(sink.url, "http://vm:8428/api/v1/write");
    }
}
