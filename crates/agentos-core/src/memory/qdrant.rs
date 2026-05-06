use super::hybrid::{
    hash_embedding, memory_backend_error, metadata_embedding, searchable_record_text,
    SemanticIndex, SemanticSearchHit,
};
use super::{MemoryError, MemoryScope};
use agentos_interfaces::memory::Record;
use agentos_proto::{Namespace, RecordId};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QdrantSemanticConfig {
    pub url: Arc<str>,
    pub collection: Arc<str>,
    pub vector_name: Option<Arc<str>>,
    pub vector_dimensions: usize,
    pub api_key: Option<Arc<str>>,
    pub timeout_ms: u64,
}

impl Default for QdrantSemanticConfig {
    fn default() -> Self {
        Self {
            url: Arc::from("http://127.0.0.1:6333"),
            collection: Arc::from("agentos_memory"),
            vector_name: None,
            vector_dimensions: 384,
            api_key: None,
            timeout_ms: 3_000,
        }
    }
}

pub struct QdrantSemanticIndex {
    config: QdrantSemanticConfig,
    endpoint: HttpEndpoint,
}

impl QdrantSemanticIndex {
    pub fn new(config: QdrantSemanticConfig) -> Result<Self, MemoryError> {
        let endpoint = HttpEndpoint::parse(&config.url)?;
        if config.collection.trim().is_empty() {
            return Err(memory_backend_error(
                "qdrant collection must not be empty when semantic memory is enabled",
            ));
        }
        if config.vector_dimensions == 0 {
            return Err(memory_backend_error(
                "qdrant vector_dimensions must be greater than 0",
            ));
        }
        Ok(Self { config, endpoint })
    }

    fn search_vector(&self, query: &str) -> Vec<f32> {
        hash_embedding(query, self.config.vector_dimensions)
    }

    fn record_vector(&self, record: &Record) -> Vec<f32> {
        metadata_embedding(record).unwrap_or_else(|| {
            hash_embedding(
                &searchable_record_text(record),
                self.config.vector_dimensions,
            )
        })
    }

    fn upsert_vector_value(&self, vector: Vec<f32>) -> Value {
        if let Some(vector_name) = &self.config.vector_name {
            let mut vectors = serde_json::Map::new();
            vectors.insert(vector_name.to_string(), json!(vector));
            Value::Object(vectors)
        } else {
            json!(vector)
        }
    }

    fn search_vector_value(&self, vector: Vec<f32>) -> Value {
        if let Some(vector_name) = &self.config.vector_name {
            json!({
                "name": vector_name.as_ref(),
                "vector": vector,
            })
        } else {
            json!(vector)
        }
    }

    fn upsert_body(&self, scope: &MemoryScope, record: &Record) -> Result<Value, MemoryError> {
        let Some(record_id) = &record.id else {
            return Err(memory_backend_error(
                "qdrant upsert requires a stable memory record id",
            ));
        };
        Ok(json!({
            "points": [{
                "id": qdrant_point_id(record_id),
                "vector": self.upsert_vector_value(self.record_vector(record)),
                "payload": {
                    "record_id": record_id.as_str(),
                    "namespace": record.namespace.as_str(),
                    "store": scope.store.as_str(),
                    "owner_kind": scope.owner.kind(),
                    "visibility": scope.visibility.as_str(),
                    "domain": scope.domain_name(),
                },
            }],
        }))
    }
}

#[async_trait]
impl SemanticIndex for QdrantSemanticIndex {
    async fn upsert(&self, scope: &MemoryScope, record: &Record) -> Result<(), MemoryError> {
        let body = self.upsert_body(scope, record)?;
        self.endpoint.request_json(
            "PUT",
            &format!(
                "/collections/{}/points?wait=true",
                percent_encode_path(&self.config.collection)
            ),
            &body,
            &self.config,
        )?;
        Ok(())
    }

    async fn search(
        &self,
        namespace: &Namespace,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SemanticSearchHit>, MemoryError> {
        if limit == 0 || query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let body = json!({
            "vector": self.search_vector_value(self.search_vector(query)),
            "filter": {
                "must": [{
                    "key": "namespace",
                    "match": { "value": namespace.as_str() }
                }]
            },
            "limit": limit,
            "with_payload": true,
        });
        let response = self.endpoint.request_json(
            "POST",
            &format!(
                "/collections/{}/points/search",
                percent_encode_path(&self.config.collection)
            ),
            &body,
            &self.config,
        )?;

        let Some(results) = response.get("result").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        Ok(results
            .iter()
            .filter_map(|result| {
                let score = result.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                let record_id = result
                    .get("payload")
                    .and_then(|payload| payload.get("record_id"))
                    .and_then(Value::as_str)?;
                Some(SemanticSearchHit {
                    record_id: RecordId::new(record_id),
                    score,
                })
            })
            .collect())
    }

    async fn delete(
        &self,
        _namespace: &Namespace,
        record_ids: &[RecordId],
    ) -> Result<(), MemoryError> {
        if record_ids.is_empty() {
            return Ok(());
        }
        let body = json!({
            "points": record_ids.iter().map(qdrant_point_id).collect::<Vec<_>>(),
        });
        self.endpoint.request_json(
            "POST",
            &format!(
                "/collections/{}/points/delete?wait=true",
                percent_encode_path(&self.config.collection)
            ),
            &body,
            &self.config,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HttpEndpoint {
    host: String,
    port: u16,
    base_path: String,
}

impl HttpEndpoint {
    fn parse(input: &str) -> Result<Self, MemoryError> {
        let without_scheme = input
            .strip_prefix("http://")
            .ok_or_else(|| memory_backend_error("qdrant url must use http://"))?;
        if without_scheme.starts_with('/') || without_scheme.trim().is_empty() {
            return Err(memory_backend_error("qdrant url is missing a host"));
        }
        let (authority, path) = without_scheme
            .split_once('/')
            .map(|(authority, path)| (authority, format!("/{path}")))
            .unwrap_or((without_scheme, String::new()));
        let (host, port) = authority
            .rsplit_once(':')
            .map(|(host, port)| {
                let parsed_port = port
                    .parse::<u16>()
                    .map_err(|_| memory_backend_error("qdrant url has an invalid port"))?;
                Ok((host.to_owned(), parsed_port))
            })
            .unwrap_or_else(|| Ok((authority.to_owned(), 80)))?;
        if host.trim().is_empty() {
            return Err(memory_backend_error("qdrant url is missing a host"));
        }
        Ok(Self {
            host,
            port,
            base_path: path.trim_end_matches('/').to_owned(),
        })
    }

    fn request_json(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        config: &QdrantSemanticConfig,
    ) -> Result<Value, MemoryError> {
        let body = serde_json::to_string(body).map_err(super::memory_json_error)?;
        let timeout = Duration::from_millis(config.timeout_ms);
        let mut stream = TcpStream::connect((self.host.as_str(), self.port)).map_err(|err| {
            memory_backend_error(format!(
                "failed to connect to qdrant '{}': {err}",
                config.url
            ))
        })?;
        stream.set_read_timeout(Some(timeout)).map_err(|err| {
            memory_backend_error(format!("failed to set qdrant read timeout: {err}"))
        })?;
        stream.set_write_timeout(Some(timeout)).map_err(|err| {
            memory_backend_error(format!("failed to set qdrant write timeout: {err}"))
        })?;
        let mut headers = format!(
            "{method} {}{path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.base_path,
            self.host,
            body.len()
        );
        if let Some(api_key) = &config.api_key {
            headers.push_str("api-key: ");
            headers.push_str(api_key);
            headers.push_str("\r\n");
        }
        headers.push_str("\r\n");
        stream
            .write_all(headers.as_bytes())
            .and_then(|_| stream.write_all(body.as_bytes()))
            .map_err(|err| {
                memory_backend_error(format!("failed to write qdrant request: {err}"))
            })?;

        let mut response = String::new();
        stream.read_to_string(&mut response).map_err(|err| {
            memory_backend_error(format!("failed to read qdrant response: {err}"))
        })?;
        parse_http_json_response(&response)
    }
}

fn parse_http_json_response(response: &str) -> Result<Value, MemoryError> {
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| memory_backend_error("qdrant returned a malformed HTTP response"))?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| memory_backend_error("qdrant returned a malformed HTTP status"))?;
    if !(200..300).contains(&status) {
        return Err(memory_backend_error(format!(
            "qdrant request failed with HTTP {status}: {body}"
        )));
    }
    if body.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(body).map_err(super::memory_json_error)
}

fn percent_encode_path(input: &str) -> String {
    input
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                vec![char::from(byte)]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn qdrant_point_id(record_id: &RecordId) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in record_id.as_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
