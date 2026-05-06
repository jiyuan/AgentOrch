use agentos_proto::{Namespace, RecordId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory backend failed: {0}")]
    Backend(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: Option<RecordId>,
    pub namespace: Namespace,
    pub body: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<Arc<str>, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum QueryType {
    Filter,
    Semantic,
    Lexical(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub query_type: QueryType,
    pub limit: usize,
}

impl Query {
    pub fn filter(limit: usize) -> Self {
        Self {
            query_type: QueryType::Filter,
            limit,
        }
    }

    pub fn lexical(text: impl Into<String>, limit: usize) -> Self {
        Self {
            query_type: QueryType::Lexical(text.into()),
            limit,
        }
    }

    pub fn semantic(limit: usize) -> Self {
        Self {
            query_type: QueryType::Semantic,
            limit,
        }
    }

    pub fn lexical_text(&self) -> Option<&str> {
        match &self.query_type {
            QueryType::Lexical(text) => Some(text.as_str()),
            QueryType::Filter | QueryType::Semantic => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Selector {
    pub id: Option<RecordId>,
    pub namespace: Option<Namespace>,
}

#[async_trait]
pub trait Memory: Send + Sync {
    /// Persist one record in the target namespace.
    ///
    /// Implementations must scope writes by `Namespace` and return the stable
    /// identifier assigned to the stored record.
    async fn write(&self, ns: &Namespace, record: Record) -> Result<RecordId, MemoryError>;

    /// Read records from a namespace according to a query.
    ///
    /// Implementations should treat `Query::limit` as a hard upper bound.
    async fn read(&self, ns: &Namespace, q: &Query) -> Result<Vec<Record>, MemoryError>;

    /// Forget records matching a selector.
    ///
    /// Implementations must return the number of records removed.
    async fn forget(&self, ns: &Namespace, sel: &Selector) -> Result<usize, MemoryError>;
}
