use agentos_interfaces::memory::{Memory, MemoryError, Query, Record, Selector};
use agentos_interfaces::orchestrator::{MemoryFragment, RunContext};
use agentos_proto::{AgentId, ConversationId, Namespace, RecordId};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod accounting;
mod hybrid;
mod in_memory;
mod qdrant;
mod reflection;
mod scope;
mod sqlite;
mod sqlite_vec;

use accounting::{MemoryAccessLogEntry, MemoryAccounting, MemoryOperation};
use hybrid::reciprocal_rank_fusion;
pub use hybrid::{SemanticIndex, SemanticSearchHit};
pub use in_memory::{InMemoryMemory, InMemorySession};
pub use qdrant::{QdrantSemanticConfig, QdrantSemanticIndex};
pub use reflection::{
    LexicalIndexReport, MemoryMaintenance, PromotionReport, ReflectionReport, ReflectionRequest,
    RetentionReport, RetentionRequest, StoreRetentionBudget,
};
pub(crate) use scope::scope_component;
pub use scope::{
    EpisodeOutcome, EpisodeRecord, HydrationRequest, HydrationResult, HydrationStats, MemoryCaller,
    MemoryOwner, MemoryScope, MemoryStore, MemoryVisibility, RetrievalStrategy,
};
pub use sqlite::SqliteStore;
pub(crate) use sqlite::{memory_json_error, memory_sqlite_error};
pub use sqlite_vec::{SqliteVecConfig, SqliteVecSemanticIndex};

#[derive(Clone)]
pub struct MemoryManager {
    backend: Arc<dyn Memory>,
    accounting: Option<Arc<dyn MemoryAccounting>>,
    maintenance: Option<Arc<dyn MemoryMaintenance>>,
    semantic_index: Option<Arc<dyn SemanticIndex>>,
}

impl MemoryManager {
    pub fn new(backend: Arc<dyn Memory>) -> Self {
        Self {
            backend,
            accounting: None,
            maintenance: None,
            semantic_index: None,
        }
    }

    pub fn new_sqlite(backend: Arc<SqliteStore>) -> Self {
        Self {
            backend: backend.clone(),
            accounting: Some(backend.clone()),
            maintenance: Some(backend),
            semantic_index: None,
        }
    }

    pub fn with_semantic_index(mut self, semantic_index: Arc<dyn SemanticIndex>) -> Self {
        self.semantic_index = Some(semantic_index);
        self
    }

    pub fn backend(&self) -> &Arc<dyn Memory> {
        &self.backend
    }

    pub fn maintenance(&self) -> Option<&Arc<dyn MemoryMaintenance>> {
        self.maintenance.as_ref()
    }

    pub async fn write_scoped(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        body: Value,
        metadata: BTreeMap<Arc<str>, Value>,
    ) -> Result<RecordId, MemoryError> {
        self.write_scoped_with_reason(caller, scope, body, metadata, Arc::from("managed_write"))
            .await
    }

    pub async fn write_scoped_with_reason(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        body: Value,
        metadata: BTreeMap<Arc<str>, Value>,
        reason: Arc<str>,
    ) -> Result<RecordId, MemoryError> {
        authorize_scope(caller, &scope, MemoryOperation::Write)?;
        let namespace = scope.namespace();
        let metadata = managed_metadata(caller, &scope, metadata);
        let id = self
            .backend
            .write(
                &namespace,
                Record {
                    id: None,
                    namespace: namespace.clone(),
                    body: body.clone(),
                    metadata: metadata.clone(),
                },
            )
            .await?;
        self.upsert_semantic_index(
            &scope,
            Record {
                id: Some(id.clone()),
                namespace: namespace.clone(),
                body: body.clone(),
                metadata: metadata.clone(),
            },
        )
        .await;
        self.append_access_log(
            MemoryOperation::Write,
            Some(&id),
            &namespace,
            caller,
            Some(&reason),
        )?;
        tracing::info!(
            operation = "managed_write",
            record_id = id.as_str(),
            namespace = namespace.as_str(),
            caller_agent_id = caller.agent_id.as_str(),
            caller_task_id = caller.task_id.as_str(),
            caller_conversation_id = caller.conversation_id.as_str(),
            store = scope.store.as_str(),
            owner_kind = scope.owner.kind(),
            visibility = scope.visibility.as_str(),
            domain = scope.domain_name(),
            reason = reason.as_ref(),
            "memory managed write"
        );
        Ok(id)
    }

    pub async fn read_scoped(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        query: &Query,
    ) -> Result<Vec<Record>, MemoryError> {
        self.read_scoped_with_reason(caller, scope, query, Arc::from("managed_read"))
            .await
    }

    pub async fn read_scoped_with_reason(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        query: &Query,
        reason: Arc<str>,
    ) -> Result<Vec<Record>, MemoryError> {
        authorize_scope(caller, &scope, MemoryOperation::Read)?;
        let namespace = scope.namespace();
        let records = self.backend.read(&namespace, query).await?;
        let record_ids = records
            .iter()
            .filter_map(|record| record.id.clone())
            .collect::<Vec<_>>();
        self.record_read_access(&record_ids)?;
        if record_ids.is_empty() {
            self.append_access_log(
                MemoryOperation::Read,
                None,
                &namespace,
                caller,
                Some(&reason),
            )?;
        } else {
            for record_id in &record_ids {
                self.append_access_log(
                    MemoryOperation::Read,
                    Some(record_id),
                    &namespace,
                    caller,
                    Some(&reason),
                )?;
            }
        }
        tracing::info!(
            operation = "managed_read",
            namespace = namespace.as_str(),
            caller_agent_id = caller.agent_id.as_str(),
            caller_task_id = caller.task_id.as_str(),
            caller_conversation_id = caller.conversation_id.as_str(),
            store = scope.store.as_str(),
            owner_kind = scope.owner.kind(),
            visibility = scope.visibility.as_str(),
            domain = scope.domain_name(),
            record_count = records.len(),
            reason = reason.as_ref(),
            "memory managed read"
        );
        Ok(records)
    }

    pub async fn hydrate(
        &self,
        caller: &MemoryCaller,
        request: HydrationRequest,
    ) -> Result<Vec<MemoryFragment>, MemoryError> {
        Ok(self.hydrate_with_stats(caller, request).await?.fragments)
    }

    pub async fn hydrate_with_stats(
        &self,
        caller: &MemoryCaller,
        request: HydrationRequest,
    ) -> Result<HydrationResult, MemoryError> {
        tracing::info!(
            operation = "hydrate",
            caller_agent_id = caller.agent_id.as_str(),
            caller_task_id = caller.task_id.as_str(),
            caller_conversation_id = caller.conversation_id.as_str(),
            query_bytes = request.query.len(),
            max_fragments = request.max_fragments,
            max_estimated_tokens = request.max_tokens,
            store_count = request.stores.len(),
            shared_domain_count = caller.allowed_shared_domains.len(),
            "memory hydrate started"
        );
        if request.max_fragments == 0 {
            tracing::info!(
                operation = "hydrate",
                caller_agent_id = caller.agent_id.as_str(),
                caller_task_id = caller.task_id.as_str(),
                caller_conversation_id = caller.conversation_id.as_str(),
                candidate_count = 0,
                selected_count = 0,
                namespace_count = 0,
                "memory hydrate finished"
            );
            return Ok(HydrationResult::default());
        }

        let mut fragments = Vec::new();
        let mut stats = HydrationStats::default();
        let mut selected_namespaces = Vec::<Namespace>::new();
        let mut estimated_tokens = 0usize;
        for scope in hydration_scopes(caller, &request) {
            let remaining = request.max_fragments.saturating_sub(fragments.len());
            if remaining == 0 {
                break;
            }
            let candidate_limit = remaining.saturating_mul(4).max(remaining);
            for record in self
                .hydrate_scope_records(caller, scope, &request, candidate_limit)
                .await?
            {
                if !record_is_active(&record) {
                    continue;
                }
                stats.candidate_count += 1;
                let fragment = MemoryFragment {
                    id: record.id,
                    namespace: record.namespace,
                    body: record.body,
                    metadata: record.metadata,
                };
                let fragment_tokens = estimate_fragment_tokens(&fragment);
                if request.max_tokens > 0
                    && estimated_tokens.saturating_add(fragment_tokens) > request.max_tokens
                {
                    continue;
                }
                if !selected_namespaces
                    .iter()
                    .any(|namespace| namespace == &fragment.namespace)
                {
                    selected_namespaces.push(fragment.namespace.clone());
                }
                estimated_tokens = estimated_tokens.saturating_add(fragment_tokens);
                fragments.push(fragment);
                if fragments.len() >= request.max_fragments {
                    break;
                }
            }
        }
        stats.selected_count = fragments.len();
        stats.namespace_count = selected_namespaces.len();
        tracing::info!(
            operation = "hydrate",
            caller_agent_id = caller.agent_id.as_str(),
            caller_task_id = caller.task_id.as_str(),
            caller_conversation_id = caller.conversation_id.as_str(),
            candidate_count = stats.candidate_count,
            selected_count = stats.selected_count,
            namespace_count = stats.namespace_count,
            "memory hydrate finished"
        );
        Ok(HydrationResult { fragments, stats })
    }

    pub async fn record_episode(
        &self,
        episode: EpisodeRecord,
    ) -> Result<Option<RecordId>, MemoryError> {
        if !episode.should_record() {
            tracing::info!(
                operation = "episode",
                run_id = episode.run_id.as_str(),
                task_id = episode.task_id.as_str(),
                active_agent = episode.active_agent.as_str(),
                conversation_id = episode.conversation_id.as_str(),
                outcome = episode.outcome.as_str(),
                turn_count = episode.turn_count,
                tools_count = episode.tools_used.len(),
                subagents_count = episode.subagents_used.len(),
                skip_reason = "trivial_run",
                "memory episode skipped"
            );
            return Ok(None);
        }

        let run_id = episode.run_id.clone();
        let task_id = episode.task_id.clone();
        let active_agent = episode.active_agent.clone();
        let conversation_id = episode.conversation_id.clone();
        let outcome = episode.outcome;
        let tools_count = episode.tools_used.len();
        let subagents_count = episode.subagents_used.len();
        let turn_count = episode.turn_count;
        let caller = MemoryCaller {
            agent_id: episode.active_agent.clone(),
            task_id: episode.task_id.clone(),
            conversation_id: episode.conversation_id.clone(),
            user_id: episode.user_id.clone(),
            allowed_shared_domains: Vec::new(),
        };
        let scope = MemoryScope::new(
            MemoryStore::Episodic,
            MemoryOwner::Conversation(episode.conversation_id.clone()),
            MemoryVisibility::Private,
            None,
        );
        let body = json!({
            "kind": "run_episode",
            "run_id": episode.run_id.as_str(),
            "task_id": episode.task_id.as_str(),
            "active_agent": episode.active_agent.as_str(),
            "conversation_id": episode.conversation_id.as_str(),
            "outcome": episode.outcome.as_str(),
            "tools_used": episode
                .tools_used
                .iter()
                .map(Arc::as_ref)
                .collect::<Vec<_>>(),
            "subagents_used": episode
                .subagents_used
                .iter()
                .map(AgentId::as_str)
                .collect::<Vec<_>>(),
            "summary": episode.summary.as_ref(),
        });
        let mut metadata = episode.metadata;
        metadata.insert(
            Arc::from("source_run_id"),
            Value::String(episode.run_id.as_str().to_owned()),
        );
        metadata.insert(
            Arc::from("outcome"),
            Value::String(episode.outcome.as_str().to_owned()),
        );
        metadata.insert(
            Arc::from("tools_count"),
            Value::from(episode.tools_used.len() as u64),
        );
        metadata.insert(
            Arc::from("subagents_count"),
            Value::from(episode.subagents_used.len() as u64),
        );
        metadata.insert(
            Arc::from("turn_count"),
            Value::from(episode.turn_count as u64),
        );

        let id = self
            .write_scoped_with_reason(&caller, scope, body, metadata, Arc::from("episode_record"))
            .await?;
        tracing::info!(
            operation = "episode",
            record_id = id.as_str(),
            run_id = run_id.as_str(),
            task_id = task_id.as_str(),
            active_agent = active_agent.as_str(),
            conversation_id = conversation_id.as_str(),
            outcome = outcome.as_str(),
            turn_count = turn_count,
            tools_count = tools_count,
            subagents_count = subagents_count,
            "memory episode recorded"
        );
        Ok(Some(id))
    }

    async fn hydrate_scope_records(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        request: &HydrationRequest,
        limit: usize,
    ) -> Result<Vec<Record>, MemoryError> {
        match request.strategy {
            RetrievalStrategy::Recency => {
                self.read_scoped(caller, scope, &Query::filter(limit)).await
            }
            RetrievalStrategy::Lexical => {
                let scoped_query = Query::lexical(request.query.as_ref(), limit);
                self.read_scoped(caller, scope, &scoped_query).await
            }
            RetrievalStrategy::Hybrid => {
                self.hybrid_scope_records(caller, scope, request.query.as_ref(), limit)
                    .await
            }
        }
    }

    async fn hybrid_scope_records(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Record>, MemoryError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let lexical_records = self
            .read_scoped(caller, scope.clone(), &Query::lexical(query, limit))
            .await?;
        let lexical_ids = lexical_records
            .iter()
            .filter_map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let mut records_by_id = lexical_records
            .into_iter()
            .filter_map(|record| record.id.clone().map(|id| (id, record)))
            .collect::<BTreeMap<_, _>>();

        let Some(index) = &self.semantic_index else {
            return Ok(lexical_ids
                .into_iter()
                .filter_map(|record_id| records_by_id.remove(&record_id))
                .take(limit)
                .collect());
        };

        let namespace = scope.namespace();
        let mut semantic_hits = match index.search(&namespace, query, limit).await {
            Ok(hits) => hits,
            Err(err) => {
                tracing::warn!(
                    operation = "semantic_index_search",
                    namespace = namespace.as_str(),
                    error = err.to_string(),
                    "semantic index search failed"
                );
                return Ok(lexical_ids
                    .into_iter()
                    .filter_map(|record_id| records_by_id.remove(&record_id))
                    .take(limit)
                    .collect());
            }
        };
        semantic_hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.record_id.as_str().cmp(right.record_id.as_str()))
        });
        let semantic_ids = semantic_hits
            .into_iter()
            .map(|hit| hit.record_id)
            .collect::<Vec<_>>();
        let missing_ids = semantic_ids
            .iter()
            .filter(|record_id| !records_by_id.contains_key(*record_id))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !missing_ids.is_empty() {
            for record in self
                .backend
                .read(&namespace, &Query::filter(usize::MAX))
                .await?
            {
                let Some(record_id) = record.id.clone() else {
                    continue;
                };
                if missing_ids.contains(&record_id) {
                    records_by_id.insert(record_id, record);
                }
            }
        }

        Ok(reciprocal_rank_fusion(&[lexical_ids, semantic_ids], limit)
            .into_iter()
            .filter_map(|record_id| records_by_id.remove(&record_id))
            .collect())
    }

    pub async fn forget_scoped(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        selector: Selector,
    ) -> Result<usize, MemoryError> {
        authorize_scope(caller, &scope, MemoryOperation::Forget)?;
        self.forget_scoped_with_reason(caller, scope, selector, Arc::from("managed_forget"))
            .await
    }

    pub async fn forget_scoped_with_reason(
        &self,
        caller: &MemoryCaller,
        scope: MemoryScope,
        selector: Selector,
        reason: Arc<str>,
    ) -> Result<usize, MemoryError> {
        authorize_scope(caller, &scope, MemoryOperation::Forget)?;
        let namespace = scope.namespace();
        if selector
            .namespace
            .as_ref()
            .is_some_and(|selected| selected != &namespace)
        {
            return Err(unauthorized(
                "selector namespace is outside the requested scope",
            ));
        }
        let candidates = self
            .backend
            .read(&namespace, &Query::filter(usize::MAX))
            .await?;
        let removed_ids = candidates
            .iter()
            .filter(|record| selector_matches_record(record, &selector))
            .filter_map(|record| record.id.clone())
            .collect::<Vec<_>>();
        let removed = self.backend.forget(&namespace, &selector).await?;
        self.delete_semantic_index_records(&namespace, &removed_ids)
            .await;
        if removed_ids.is_empty() {
            self.append_access_log(
                MemoryOperation::Forget,
                None,
                &namespace,
                caller,
                Some(&reason),
            )?;
        } else {
            for record_id in &removed_ids {
                self.append_access_log(
                    MemoryOperation::Forget,
                    Some(record_id),
                    &namespace,
                    caller,
                    Some(&reason),
                )?;
            }
        }
        tracing::info!(
            operation = "managed_forget",
            namespace = namespace.as_str(),
            caller_agent_id = caller.agent_id.as_str(),
            caller_task_id = caller.task_id.as_str(),
            caller_conversation_id = caller.conversation_id.as_str(),
            store = scope.store.as_str(),
            owner_kind = scope.owner.kind(),
            visibility = scope.visibility.as_str(),
            domain = scope.domain_name(),
            removed_count = removed,
            reason = reason.as_ref(),
            "memory managed forget"
        );
        Ok(removed)
    }

    fn record_read_access(&self, record_ids: &[RecordId]) -> Result<(), MemoryError> {
        if let Some(accounting) = &self.accounting {
            accounting.record_read_access(record_ids)?;
        }
        Ok(())
    }

    fn append_access_log(
        &self,
        operation: MemoryOperation,
        record_id: Option<&RecordId>,
        namespace: &Namespace,
        caller: &MemoryCaller,
        reason: Option<&Arc<str>>,
    ) -> Result<(), MemoryError> {
        if let Some(accounting) = &self.accounting {
            accounting.append_access_log(MemoryAccessLogEntry {
                operation: operation.as_str(),
                record_id,
                namespace,
                caller,
                reason: reason.map(Arc::as_ref),
            })?;
        }
        Ok(())
    }

    async fn upsert_semantic_index(&self, scope: &MemoryScope, record: Record) {
        let Some(index) = &self.semantic_index else {
            return;
        };
        if let Err(err) = index.upsert(scope, &record).await {
            tracing::warn!(
                operation = "semantic_index_upsert",
                namespace = record.namespace.as_str(),
                record_id = record.id.as_ref().map(RecordId::as_str),
                error = err.to_string(),
                "semantic index upsert failed"
            );
        }
    }

    async fn delete_semantic_index_records(&self, namespace: &Namespace, record_ids: &[RecordId]) {
        let Some(index) = &self.semantic_index else {
            return;
        };
        if let Err(err) = index.delete(namespace, record_ids).await {
            tracing::warn!(
                operation = "semantic_index_delete",
                namespace = namespace.as_str(),
                record_count = record_ids.len(),
                error = err.to_string(),
                "semantic index delete failed"
            );
        }
    }
}

pub fn memory_caller_from_context(
    ctx: &RunContext<'_>,
    allowed_shared_domains: Vec<Arc<str>>,
) -> MemoryCaller {
    let metadata = ctx
        .transcript
        .items
        .last()
        .map(|item| &item.metadata)
        .unwrap_or(&ctx.system.metadata);
    let conversation_id = metadata
        .get("conversation_id")
        .and_then(Value::as_str)
        .map(ConversationId::new)
        .unwrap_or_else(|| ConversationId::new(ctx.state.run_id.as_str()));
    let user_id_key = if metadata
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "subagent_input")
    {
        "user_id"
    } else {
        "sender"
    };
    let user_id = metadata
        .get(user_id_key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|sender| !sender.is_empty())
        .map(Arc::from);
    let mut allowed_shared_domains = allowed_shared_domains;
    if let Some(view) = metadata.get("memory_view").and_then(Value::as_str) {
        if matches!(view, "shared_readonly" | "shared_readwrite") {
            allowed_shared_domains.extend(
                metadata
                    .get("memory_domains")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(Arc::from),
            );
            allowed_shared_domains.sort();
            allowed_shared_domains.dedup();
        }
    }

    MemoryCaller {
        agent_id: ctx.system.active_agent.clone(),
        task_id: ctx.system.task_id.clone(),
        conversation_id,
        user_id,
        allowed_shared_domains,
    }
}

fn authorize_scope(
    caller: &MemoryCaller,
    scope: &MemoryScope,
    operation: MemoryOperation,
) -> Result<(), MemoryError> {
    if scope.store == MemoryStore::Audit {
        return Err(unauthorized("audit memory requires an administrative path"));
    }

    match &scope.owner {
        MemoryOwner::User(user_id) => {
            let caller_user = caller
                .user_id
                .as_deref()
                .unwrap_or_else(|| caller.conversation_id.as_str());
            if caller_user == user_id.as_ref() {
                Ok(())
            } else {
                Err(unauthorized("user memory belongs to a different caller"))
            }
        }
        MemoryOwner::Conversation(conversation_id) => {
            if conversation_id == &caller.conversation_id {
                Ok(())
            } else {
                Err(unauthorized(
                    "conversation memory belongs to a different conversation",
                ))
            }
        }
        MemoryOwner::Agent(agent_id) => {
            if scope.visibility == MemoryVisibility::Private && agent_id == &caller.agent_id {
                Ok(())
            } else {
                Err(unauthorized("agent memory belongs to a different agent"))
            }
        }
        MemoryOwner::Task(task_id) => {
            if task_id == &caller.task_id {
                Ok(())
            } else {
                Err(unauthorized("task memory belongs to a different task"))
            }
        }
        MemoryOwner::Shared => {
            let shared_read = matches!(operation, MemoryOperation::Read)
                && scope.visibility != MemoryVisibility::Private;
            if shared_read && shared_domain_allowed(caller, scope) {
                Ok(())
            } else {
                Err(unauthorized(
                    "shared memory is outside the caller's allowed domains",
                ))
            }
        }
    }
}

fn hydration_scopes(caller: &MemoryCaller, request: &HydrationRequest) -> Vec<MemoryScope> {
    let stores = if request.stores.is_empty() {
        vec![MemoryStore::Semantic]
    } else {
        request.stores.clone()
    };
    let domain = request.domain.clone();
    let mut scopes = Vec::new();
    for store in stores {
        if let Some(user_id) = &caller.user_id {
            scopes.push(MemoryScope::new(
                store,
                MemoryOwner::User(Arc::clone(user_id)),
                MemoryVisibility::Private,
                domain.clone(),
            ));
        }
        scopes.push(MemoryScope::new(
            store,
            MemoryOwner::Conversation(caller.conversation_id.clone()),
            MemoryVisibility::Private,
            domain.clone(),
        ));
        scopes.push(MemoryScope::new(
            store,
            MemoryOwner::Agent(caller.agent_id.clone()),
            MemoryVisibility::Private,
            domain.clone(),
        ));
        scopes.push(MemoryScope::new(
            store,
            MemoryOwner::Task(caller.task_id.clone()),
            MemoryVisibility::Private,
            domain.clone(),
        ));
        for shared_domain in &caller.allowed_shared_domains {
            scopes.push(MemoryScope::new(
                store,
                MemoryOwner::Shared,
                MemoryVisibility::Shared,
                Some(Arc::clone(shared_domain)),
            ));
        }
    }
    scopes
}

fn shared_domain_allowed(caller: &MemoryCaller, scope: &MemoryScope) -> bool {
    let domain = scope.domain.as_deref().unwrap_or("general");
    caller
        .allowed_shared_domains
        .iter()
        .any(|allowed| allowed.as_ref() == domain)
}

fn unauthorized(message: &'static str) -> MemoryError {
    MemoryError::Backend(Arc::from(format!("memory scope unauthorized: {message}")))
}

fn managed_metadata(
    caller: &MemoryCaller,
    scope: &MemoryScope,
    mut metadata: BTreeMap<Arc<str>, Value>,
) -> BTreeMap<Arc<str>, Value> {
    metadata.insert(
        Arc::from("store"),
        Value::String(scope.store.as_str().to_owned()),
    );
    metadata.insert(
        Arc::from("owner_kind"),
        Value::String(scope.owner.kind().to_owned()),
    );
    metadata.insert(
        Arc::from("owner_id"),
        Value::String(scope_component(scope.owner.id(), "global")),
    );
    metadata.insert(
        Arc::from("visibility"),
        Value::String(scope.visibility.as_str().to_owned()),
    );
    metadata.insert(Arc::from("domain"), Value::String(scope.domain_name()));
    metadata.insert(
        Arc::from("source_agent_id"),
        Value::String(caller.agent_id.as_str().to_owned()),
    );
    metadata.insert(
        Arc::from("source_task_id"),
        Value::String(caller.task_id.as_str().to_owned()),
    );
    metadata.insert(
        Arc::from("conversation_id"),
        Value::String(caller.conversation_id.as_str().to_owned()),
    );
    metadata
        .entry(Arc::from("importance"))
        .or_insert_with(|| Value::from(0.0));
    metadata
        .entry(Arc::from("confidence"))
        .or_insert_with(|| Value::from(1.0));
    metadata
        .entry(Arc::from("status"))
        .or_insert_with(|| Value::String("active".to_owned()));
    metadata
        .entry(Arc::from("schema"))
        .or_insert_with(|| Value::String("agentos.memory.v1".to_owned()));
    metadata
}

fn selector_matches_record(record: &Record, selector: &Selector) -> bool {
    if let Some(id) = &selector.id {
        return record.id.as_ref() == Some(id);
    }
    if let Some(namespace) = &selector.namespace {
        return &record.namespace == namespace;
    }
    true
}

fn estimate_fragment_tokens(fragment: &MemoryFragment) -> usize {
    let body_chars = fragment.body.to_string().chars().count();
    let metadata_chars = serde_json::to_string(&fragment.metadata)
        .map(|metadata| metadata.chars().count())
        .unwrap_or(0);
    (body_chars + metadata_chars).div_ceil(4).max(1)
}

fn record_matches_query(record: &Record, q: &Query) -> bool {
    if !record_is_active(record) {
        return false;
    }
    let Some(query) = q.lexical_text().map(str::trim) else {
        return true;
    };
    if query.is_empty() {
        return true;
    }
    let needle = query.to_ascii_lowercase();
    record
        .body
        .to_string()
        .to_ascii_lowercase()
        .contains(&needle)
        || serde_json::to_string(&record.metadata)
            .map(|metadata| metadata.to_ascii_lowercase().contains(&needle))
            .unwrap_or(false)
}

pub(crate) fn record_is_active(record: &Record) -> bool {
    record
        .metadata
        .get("status")
        .and_then(Value::as_str)
        .map(|status| status == "active")
        .unwrap_or(true)
}
