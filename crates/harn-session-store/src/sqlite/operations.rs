use super::*;

#[async_trait]
impl SessionImporter for SqliteSessionStore {
    async fn import(&self, request: ImportSession) -> StoreResult<ImportResult> {
        request.validate()?;
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        if let Some(existing) = read_import(&tx, &request.source_id)? {
            if existing.source_digest != request.source_digest {
                return Err(StoreError::Conflict(format!(
                    "import source '{}' changed digest",
                    request.source_id
                )));
            }
            return Ok(existing);
        }

        let meta = crate::memory_helpers::meta_for_create(request.session);
        if tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![meta.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(map_sql)?
            .is_some()
        {
            return Err(StoreError::AlreadyExists(meta.id));
        }
        insert_session(&tx, &meta, 1)?;
        let event_count = request.events.len();
        for event in request.events {
            append_in_tx(&tx, &self.hooks, &meta.id, event)?;
        }
        tx.execute(
            "INSERT INTO session_imports (source_id, source_digest, session_id, event_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                request.source_id,
                request.source_digest,
                meta.id,
                event_count as i64
            ],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(ImportResult {
            source_id: request.source_id,
            source_digest: request.source_digest,
            session_id: meta.id,
            event_count,
            imported: true,
        })
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    fn hooks(&self) -> &StoreHooks {
        &self.hooks
    }

    async fn create(&self, request: CreateSession) -> StoreResult<SessionMeta> {
        let meta = crate::memory_helpers::meta_for_create(request);
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        insert_session(&tx, &meta, 1)?;
        tx.commit().map_err(map_sql)?;
        Ok(meta)
    }

    async fn describe(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
        Ok(meta)
    }

    async fn update(&self, session_id: &str, request: UpdateSession) -> StoreResult<SessionMeta> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let (updated_at_ms, updated_at) = now_ms_and_rfc3339();
        let changed = tx
            .execute(
                "UPDATE sessions SET
                    title = COALESCE(?1, title),
                    cwd = COALESCE(?2, cwd),
                    model = COALESCE(?3, model),
                    parent_session_id = COALESCE(?4, parent_session_id),
                    session_type = COALESCE(?5, session_type),
                    project_scope = COALESCE(?6, project_scope),
                    usage_input = COALESCE(?7, usage_input),
                    usage_output = COALESCE(?8, usage_output),
                    usage_cost_usd_micros = COALESCE(?9, usage_cost_usd_micros),
                    updated_at_ms = ?10,
                    updated_at = ?11
                 WHERE id = ?12",
                params![
                    request.title,
                    request.cwd,
                    request.model,
                    request.parent_session_id,
                    request.session_type.map(session_type_to_sql),
                    request.project_scope,
                    request.usage_input.map(|value| value as i64),
                    request.usage_output.map(|value| value as i64),
                    request.usage_cost_usd_micros.map(|value| value as i64),
                    updated_at_ms,
                    updated_at,
                    session_id,
                ],
            )
            .map_err(map_sql)?;
        if changed == 0 {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        let (meta, _) = read_session_meta(&tx, session_id)?;
        let mut events = load_all_events(&tx, session_id)?;
        redact_stored_events(&self.hooks, &mut events)?;
        tx.execute(
            "DELETE FROM session_events_fts WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_event_vectors WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(map_sql)?;
        for event in &events {
            insert_search_rows(&tx, &self.hooks, &meta, event)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(meta)
    }

    async fn list(&self, filter: ListFilter) -> StoreResult<Vec<SessionMeta>> {
        let conn = self.lock();
        let limit = filter.limit.unwrap_or(MAX_READ_BATCH).min(MAX_READ_BATCH) as i64;
        let sort_column = match filter.sort_by {
            ListSortKey::CreatedAt => "created_at_ms",
            ListSortKey::UpdatedAt => "updated_at_ms",
        };
        // Pull the cursor's anchor row up front so the SQL can do
        // keyset pagination on the selected timestamp and id instead of scanning
        // every prior row into memory.
        let cursor_anchor: Option<(i64, String)> = filter
            .cursor
            .as_ref()
            .map(|id| {
                conn.query_row(
                    &format!("SELECT {sort_column}, id FROM sessions WHERE id = ?1"),
                    params![id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sql)
            })
            .transpose()?
            .flatten();

        let mut sql = String::from("SELECT s.id FROM sessions s");
        if filter.tag.is_some() {
            sql.push_str(" INNER JOIN session_tags t ON t.session_id = s.id AND t.tag = :tag");
        }
        sql.push_str(" WHERE 1=1");
        let mut args: Vec<(&'static str, rusqlite::types::Value)> = Vec::new();
        if let Some(tag) = filter.tag {
            args.push((":tag", tag.into()));
        }
        if let Some(tenant) = filter.tenant_id {
            sql.push_str(" AND s.tenant_id = :tenant");
            args.push((":tenant", tenant.into()));
        }
        if let Some(persona) = filter.persona {
            sql.push_str(" AND s.persona = :persona");
            args.push((":persona", persona.into()));
        }
        if let Some(status) = filter.status {
            sql.push_str(" AND s.status = :status");
            args.push((":status", status_to_sql(status).to_string().into()));
        }
        if let Some(parent_session_id) = filter.parent_session_id {
            sql.push_str(" AND s.parent_session_id = :parent_session_id");
            args.push((":parent_session_id", parent_session_id.into()));
        }
        if let Some(session_type) = filter.session_type {
            sql.push_str(" AND s.session_type = :session_type");
            args.push((
                ":session_type",
                session_type_to_sql(session_type).to_string().into(),
            ));
        }
        if let Some(project_scope) = filter.project_scope {
            sql.push_str(" AND s.project_scope = :project_scope");
            args.push((":project_scope", project_scope.into()));
        }
        if let Some(after) = filter.created_after_ms {
            sql.push_str(" AND s.created_at_ms >= :after");
            args.push((":after", after.into()));
        }
        if let Some(before) = filter.created_before_ms {
            sql.push_str(" AND s.created_at_ms <= :before");
            args.push((":before", before.into()));
        }
        if let Some((anchor_ms, anchor_id)) = cursor_anchor {
            let comparison = match filter.order {
                ListOrder::Ascending => ">",
                ListOrder::Descending => "<",
            };
            sql.push_str(&format!(
                " AND (s.{sort_column} {comparison} :cursor_ms OR (s.{sort_column} = :cursor_ms AND s.id > :cursor_id))"
            ));
            args.push((":cursor_ms", anchor_ms.into()));
            args.push((":cursor_id", anchor_id.into()));
        }
        let direction = match filter.order {
            ListOrder::Ascending => "ASC",
            ListOrder::Descending => "DESC",
        };
        sql.push_str(&format!(
            " ORDER BY s.{sort_column} {direction}, s.id ASC LIMIT :limit"
        ));
        args.push((":limit", limit.into()));

        let named_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
            .iter()
            .map(|(name, value)| (*name, value as &dyn rusqlite::ToSql))
            .collect();
        let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
        let ids: Vec<String> = stmt
            .query_map(named_args.as_slice(), |row| row.get(0))
            .map_err(map_sql)?
            .collect::<Result<_, _>>()
            .map_err(map_sql)?;
        let mut metas = Vec::with_capacity(ids.len());
        for id in ids {
            let (meta, _) = read_session_meta(&conn, &id)?;
            metas.push(meta);
        }
        Ok(metas)
    }

    async fn append(&self, session_id: &str, event: AppendEvent) -> StoreResult<StoredEvent> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let stored = append_in_tx(&tx, &self.hooks, session_id, event)?;
        tx.commit().map_err(map_sql)?;
        Ok(stored)
    }

    async fn read(&self, session_id: &str, range: ReadRange) -> StoreResult<EventPage> {
        let conn = self.lock();
        let from = range.from_event_id.unwrap_or(1) as i64;
        // SQLite stores event_id as INTEGER (signed i64); use i64::MAX as
        // the unbounded upper sentinel rather than casting EventId::MAX,
        // which silently wraps to -1.
        let to = range
            .to_event_id
            .map(|value| value as i64)
            .unwrap_or(i64::MAX);
        let limit = range.limit.unwrap_or(MAX_READ_BATCH).min(MAX_READ_BATCH) as i64;
        let mut stmt = conn
            .prepare(
                "SELECT session_id, event_id, tenant_id, parent_event_id, actor, kind,
                        custom_kind, payload_json, tags_json, headers_json, ts_ms, ts,
                        record_hash, prev_hash, signature_json
                 FROM session_events
                 WHERE session_id = ?1 AND event_id >= ?2 AND event_id <= ?3
                 ORDER BY event_id ASC LIMIT ?4",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map(params![session_id, from, to, limit], read_event)
            .map_err(map_sql)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(map_sql)?);
        }
        redact_stored_events(&self.hooks, &mut events)?;
        let next_cursor = if events.len() as i64 == limit {
            events.last().map(|tail| tail.event_id + 1)
        } else {
            None
        };
        Ok(EventPage {
            events,
            next_cursor,
        })
    }

    async fn fork(
        &self,
        session_id: &str,
        at_event_id: EventId,
        child_id: Option<SessionId>,
    ) -> StoreResult<ForkResult> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let (parent_meta, _) = read_session_meta(&tx, session_id)?;
        let parent_events = load_all_events(&tx, session_id)?;
        if !parent_events
            .iter()
            .any(|event| event.event_id == at_event_id)
        {
            return Err(StoreError::InvalidInput(format!(
                "event {at_event_id} not found in session '{session_id}'"
            )));
        }
        let new_id = child_id.unwrap_or_else(|| Uuid::now_v7().to_string());
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![new_id],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_sql)?
            .unwrap_or(false);
        if exists {
            return Err(StoreError::AlreadyExists(new_id));
        }
        let (ms, text) = now_ms_and_rfc3339();
        let mut child_meta = parent_meta.clone();
        child_meta.id = new_id.clone();
        child_meta.parent_session_id = Some(parent_meta.id);
        child_meta.created_at_ms = ms;
        child_meta.created_at = text.clone();
        child_meta.updated_at_ms = ms;
        child_meta.updated_at = text;
        child_meta.status = SessionStatus::Open;
        child_meta.closed_at_ms = None;
        child_meta.closed_at = None;
        child_meta.soft_deleted_at_ms = None;
        let mut inherited: Vec<StoredEvent> = parent_events
            .into_iter()
            .filter(|event| event.event_id <= at_event_id)
            .collect();
        prepare_stored_events_for_persistence(&self.hooks, &mut inherited)?;
        let copied = re_anchor_events(&inherited, &new_id);
        child_meta.event_count = copied.len();
        child_meta.last_event_id = copied.last().map(|tail| tail.event_id);
        child_meta.chain_root_hash = Some(chain_root_hash(&copied));
        let next_event_id = copied.last().map(|tail| tail.event_id + 1).unwrap_or(1);
        insert_session(&tx, &child_meta, next_event_id)?;
        for event in &copied {
            insert_event(&tx, event)?;
            insert_search_rows(&tx, &self.hooks, &child_meta, event)?;
        }
        tx.commit().map_err(map_sql)?;
        Ok(ForkResult {
            child_session_id: new_id,
            forked_from_event_id: at_event_id,
            copied_event_count: copied.len(),
        })
    }

    async fn truncate(
        &self,
        session_id: &str,
        at_event_id: EventId,
    ) -> StoreResult<TruncateResult> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        let (mut meta, _) = read_session_meta(&tx, session_id)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM session_events WHERE session_id = ?1 AND event_id = ?2",
                params![session_id, at_event_id as i64],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_sql)?
            .unwrap_or(false);
        if !exists {
            return Err(StoreError::InvalidInput(format!(
                "event {at_event_id} not found in session '{session_id}'"
            )));
        }
        let removed: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM session_events
                 WHERE session_id = ?1 AND event_id > ?2",
                params![session_id, at_event_id as i64],
                |row| row.get(0),
            )
            .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_events WHERE session_id = ?1 AND event_id > ?2",
            params![session_id, at_event_id as i64],
        )
        .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_events_fts
             WHERE session_id = ?1 AND CAST(event_id AS INTEGER) > ?2",
            params![session_id, at_event_id as i64],
        )
        .map_err(map_sql)?;
        tx.execute(
            "DELETE FROM session_event_vectors
             WHERE session_id = ?1 AND event_id > ?2",
            params![session_id, at_event_id as i64],
        )
        .map_err(map_sql)?;
        let remaining_hashes: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT record_hash FROM session_events
                     WHERE session_id = ?1 ORDER BY event_id ASC",
                )
                .map_err(map_sql)?;
            let rows = stmt
                .query_map(params![session_id], |row| row.get::<_, String>(0))
                .map_err(map_sql)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_sql)?);
            }
            out
        };
        let new_root = remaining_hashes
            .iter()
            .fold(chain_root_init(), |root, hash| chain_root_fold(&root, hash));
        let (ms, text) = now_ms_and_rfc3339();
        meta.event_count = remaining_hashes.len();
        meta.last_event_id = Some(at_event_id);
        meta.chain_root_hash = Some(new_root);
        meta.updated_at_ms = ms;
        meta.updated_at = text;
        tx.execute(
            "UPDATE sessions SET event_count = ?1, last_event_id = ?2,
                                  chain_root_hash = ?3, updated_at_ms = ?4,
                                  updated_at = ?5, next_event_id = ?6 WHERE id = ?7",
            params![
                meta.event_count as i64,
                meta.last_event_id.map(|value| value as i64),
                meta.chain_root_hash,
                meta.updated_at_ms,
                meta.updated_at,
                (at_event_id + 1) as i64,
                session_id,
            ],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(TruncateResult {
            kept_event_count: meta.event_count,
            removed_event_count: removed as usize,
            new_tip_event_id: meta.last_event_id,
        })
    }

    async fn snapshot(&self, session_id: &str) -> StoreResult<Snapshot> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
        let mut events = load_all_events(&conn, session_id)?;
        redact_stored_events(&self.hooks, &mut events)?;
        let (ms, text) = now_ms_and_rfc3339();
        let snapshot = Snapshot {
            id: SnapshotId(format!("snap-{}", Uuid::now_v7())),
            session: meta,
            events,
            captured_at_ms: ms,
            captured_at: text,
        };
        let body = serde_json::to_string(&snapshot)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        conn.execute(
            "INSERT INTO session_snapshots (id, session_id, captured_at_ms, captured_at, body_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                snapshot.id.0,
                snapshot.session.id,
                snapshot.captured_at_ms,
                snapshot.captured_at,
                body,
            ],
        )
        .map_err(map_sql)?;
        Ok(snapshot)
    }

    async fn replay(&self, snapshot_id: &SnapshotId) -> StoreResult<Snapshot> {
        let conn = self.lock();
        let body: Option<String> = conn
            .query_row(
                "SELECT body_json FROM session_snapshots WHERE id = ?1",
                params![snapshot_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        let body = body.ok_or_else(|| StoreError::NotFound(snapshot_id.0.clone()))?;
        let mut snapshot: Snapshot =
            serde_json::from_str(&body).map_err(|error| StoreError::Backend(error.to_string()))?;
        redact_stored_events(&self.hooks, &mut snapshot.events)?;
        Ok(snapshot)
    }

    async fn close(&self, session_id: &str) -> StoreResult<StoredEvent> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        // Read the pre-receipt chain root inside the transaction so the
        // root we sign is exactly the chain the receipt finalises, with
        // no window for a concurrent append to move the tip.
        let (meta, _) = read_session_meta(&tx, session_id)?;
        crate::memory_helpers::validate_open(&meta)?;
        let record_root = match meta.chain_root_hash.clone() {
            Some(root) => root,
            None => chain_root_hash(&load_all_events(&tx, session_id)?),
        };
        let last_event_id = meta.last_event_id.unwrap_or(0);
        let payload =
            crate::signing::canonical_receipt_payload(session_id, last_event_id, &record_root);
        let mut append = AppendEvent::new(SessionEventKind::Receipt, payload);
        append.actor = Some("session_store".into());
        let mut stored = append_in_tx(&tx, &self.hooks, session_id, append)?;
        // Intentionally replace the receipt's append-time per-event
        // signature with a receipt-root signature. The receipt's purpose
        // is to attest the chain root, so `verify()` special-cases it via
        // `verify_receipt_root` against the pre-receipt root rather than
        // the receipt event's own canonical bytes.
        if let Some(signer) = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
        {
            let signature = signer.sign_receipt(&record_root);
            let signature_json =
                serde_json::to_string(&signature).unwrap_or_else(|_| "null".into());
            tx.execute(
                "UPDATE session_events SET signature_json = ?1
                 WHERE session_id = ?2 AND event_id = ?3",
                params![signature_json, session_id, stored.event_id as i64],
            )
            .map_err(map_sql)?;
            stored.signed_by = Some(signature);
        }
        let (ms, text) = now_ms_and_rfc3339();
        tx.execute(
            "UPDATE sessions SET status = ?1, closed_at_ms = ?2, closed_at = ?3,
                                  updated_at_ms = ?2, updated_at = ?3 WHERE id = ?4",
            params![status_to_sql(SessionStatus::Closed), ms, text, session_id,],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(stored)
    }

    async fn soft_delete(&self, session_id: &str) -> StoreResult<SessionMeta> {
        let conn = self.lock();
        let (mut meta, _) = read_session_meta(&conn, session_id)?;
        match meta.status {
            SessionStatus::HardDeleted => return Err(StoreError::NotFound(session_id.to_string())),
            SessionStatus::SoftDeleted => return Ok(meta),
            _ => {}
        }
        let (ms, text) = now_ms_and_rfc3339();
        conn.execute(
            "UPDATE sessions SET status = ?1, soft_deleted_at_ms = ?2,
                                  updated_at_ms = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                status_to_sql(SessionStatus::SoftDeleted),
                ms,
                text,
                session_id,
            ],
        )
        .map_err(map_sql)?;
        meta.status = SessionStatus::SoftDeleted;
        meta.soft_deleted_at_ms = Some(ms);
        meta.updated_at_ms = ms;
        meta.updated_at = text;
        Ok(meta)
    }

    async fn hard_delete(&self, session_id: &str) -> StoreResult<()> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        tx.execute(
            "DELETE FROM session_events_fts WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(map_sql)?;
        let removed = tx
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
            .map_err(map_sql)?;
        if removed == 0 {
            return Err(StoreError::NotFound(session_id.to_string()));
        }
        tx.commit().map_err(map_sql)?;
        Ok(())
    }

    async fn verify(&self, session_id: &str) -> StoreResult<VerifyReport> {
        let conn = self.lock();
        let (meta, _) = read_session_meta(&conn, session_id)?;
        let events = load_all_events(&conn, session_id)?;
        let event_verifier = self
            .hooks
            .event_signer
            .as_ref()
            .map(|signer| signer.verifying_key());
        let receipt_verifier = self
            .hooks
            .receipt_signer
            .as_ref()
            .or(self.hooks.event_signer.as_ref())
            .map(|signer| signer.verifying_key());
        Ok(verify_session_chain(
            &meta,
            &events,
            event_verifier.as_ref(),
            receipt_verifier.as_ref(),
        ))
    }

    async fn search(&self, query: SearchQuery) -> StoreResult<SearchResponse> {
        query.validate().map_err(StoreError::InvalidInput)?;
        let conn = self.lock();
        let embedder = self.hooks.embedder.clone();
        let semantic_available = embedder.is_semantic();
        let effective_mode = if semantic_available {
            query.mode
        } else {
            SearchMode::Fts
        };

        let literal_query = fts_literal_query(&query.query);
        if effective_mode == SearchMode::Fts && literal_query.is_empty() {
            let semantic_floor = !semantic_available;
            return Ok(SearchResponse {
                requested_mode: query.mode,
                effective_mode,
                embedding_backend: embedder.name().to_string(),
                semantic_floor,
                fallback_reason: (semantic_floor && query.mode != SearchMode::Fts)
                    .then(|| "semantic model unavailable; FTS-only fallback active".into()),
                hits: Vec::new(),
            });
        }
        let mut fts_scores = BTreeMap::new();
        if effective_mode == SearchMode::Hybrid && !literal_query.is_empty() {
            let mut sql = String::from(
                "SELECT f.session_id, CAST(f.event_id AS INTEGER),
                        -bm25(session_events_fts)
                 FROM session_events_fts f
                 INNER JOIN sessions s ON s.id = f.session_id
                 WHERE session_events_fts MATCH :match
                   AND s.status NOT IN ('soft_deleted', 'hard_deleted')",
            );
            let mut args: Vec<(&'static str, rusqlite::types::Value)> =
                vec![(":match", literal_query.clone().into())];
            append_search_scope(&mut sql, &mut args, &query);
            let named_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
                .iter()
                .map(|(name, value)| (*name, value as &dyn rusqlite::ToSql))
                .collect();
            let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
            let rows = stmt
                .query_map(named_args.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as EventId,
                        row.get::<_, f64>(2)? as f32,
                    ))
                })
                .map_err(map_sql)?;
            for row in rows {
                let (session_id, event_id, score) = row.map_err(map_sql)?;
                fts_scores.insert((session_id, event_id), score.max(f32::MIN_POSITIVE));
            }
        }

        let fts_only = effective_mode == SearchMode::Fts;
        let mut sql = if fts_only {
            String::from(
                "SELECT e.session_id, e.event_id, e.tenant_id, e.parent_event_id,
                        e.actor, e.kind, e.custom_kind, e.payload_json, e.tags_json,
                        e.headers_json, e.ts_ms, e.ts, e.record_hash, e.prev_hash,
                        e.signature_json, s.title, s.cwd, s.model, s.project_scope,
                        NULL, NULL, NULL, -bm25(session_events_fts)
                 FROM session_events_fts
                 INNER JOIN session_events e
                   ON e.session_id = session_events_fts.session_id
                  AND e.event_id = CAST(session_events_fts.event_id AS INTEGER)
                 INNER JOIN sessions s ON s.id = e.session_id
                 WHERE session_events_fts MATCH :candidate_match
                   AND s.status NOT IN ('soft_deleted', 'hard_deleted')",
            )
        } else {
            String::from(
                "SELECT e.session_id, e.event_id, e.tenant_id, e.parent_event_id,
                        e.actor, e.kind, e.custom_kind, e.payload_json, e.tags_json,
                        e.headers_json, e.ts_ms, e.ts, e.record_hash, e.prev_hash,
                        e.signature_json, s.title, s.cwd, s.model, s.project_scope,
                        v.backend, v.dim, v.embedding, NULL
                 FROM session_events e
                 INNER JOIN sessions s ON s.id = e.session_id
                 LEFT JOIN session_event_vectors v
                   ON v.session_id = e.session_id AND v.event_id = e.event_id
                 WHERE s.status NOT IN ('soft_deleted', 'hard_deleted')",
            )
        };
        let mut args: Vec<(&'static str, rusqlite::types::Value)> = if fts_only {
            vec![(":candidate_match", literal_query.into())]
        } else {
            Vec::new()
        };
        append_search_scope(&mut sql, &mut args, &query);
        if fts_only {
            sql.push_str(
                " ORDER BY bm25(session_events_fts) ASC,
                           e.session_id ASC, e.event_id ASC
                  LIMIT :candidate_limit",
            );
            args.push((
                ":candidate_limit",
                i64::try_from(query.limit()).unwrap_or(i64::MAX).into(),
            ));
        } else {
            sql.push_str(" ORDER BY e.session_id ASC, e.event_id ASC");
        }
        let named_args: Vec<(&str, &dyn rusqlite::ToSql)> = args
            .iter()
            .map(|(name, value)| (*name, value as &dyn rusqlite::ToSql))
            .collect();
        let mut stmt = conn.prepare(&sql).map_err(map_sql)?;
        let rows = stmt
            .query_map(named_args.as_slice(), |row| {
                Ok((
                    read_event(row)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, Option<String>>(19)?,
                    row.get::<_, Option<i64>>(20)?,
                    row.get::<_, Option<Vec<u8>>>(21)?,
                    row.get::<_, Option<f64>>(22)?.map(|score| score as f32),
                ))
            })
            .map_err(map_sql)?;
        let mut candidates = Vec::new();
        for row in rows {
            candidates.push(row.map_err(map_sql)?);
        }
        drop(stmt);
        drop(conn);

        let mut redacted = candidates
            .iter()
            .map(|(event, ..)| event.clone())
            .collect::<Vec<_>>();
        redact_stored_events(&self.hooks, &mut redacted)?;
        for ((event, ..), redacted_event) in candidates.iter_mut().zip(redacted) {
            *event = redacted_event;
        }
        let documents = candidates
            .iter()
            .map(|(event, title, cwd, model, project_scope, ..)| {
                redacted_search_document_parts(
                    self.hooks.redaction.as_ref(),
                    title.as_deref(),
                    cwd.as_deref(),
                    model.as_deref(),
                    project_scope.as_deref(),
                    event,
                )
            })
            .collect::<Vec<_>>();
        let aligned_fts_scores = candidates
            .iter()
            .map(|(event, _, _, _, _, _, _, _, direct_fts_score)| {
                direct_fts_score
                    .map(|score| score.max(f32::MIN_POSITIVE))
                    .unwrap_or_else(|| {
                        fts_scores
                            .get(&(event.session_id.clone(), event.event_id))
                            .copied()
                            .unwrap_or_default()
                    })
            })
            .collect::<Vec<_>>();
        let semantic_scores = if semantic_available {
            let query_vector = embedder.embed(&query.query);
            candidates
                .iter()
                .enumerate()
                .map(|(index, (_, _, _, _, _, backend, dim, blob, _))| {
                    let stored = backend
                        .as_deref()
                        .filter(|backend| *backend == embedder.name())
                        .zip(dim.and_then(|dim| usize::try_from(dim).ok()))
                        .filter(|(_, dim)| *dim == embedder.dim())
                        .zip(blob.as_deref())
                        .and_then(|((_, dim), blob)| vector_from_blob(blob, dim));
                    let vector =
                        stored.unwrap_or_else(|| embedder.embed(documents[index].as_str()));
                    crate::search::cosine(&query_vector, &vector).max(0.0)
                })
                .collect::<Vec<_>>()
        } else {
            vec![0.0; candidates.len()]
        };

        let fts_ranks = ranks(&aligned_fts_scores);
        let semantic_ranks = ranks(&semantic_scores);
        let mut hits = candidates
            .into_iter()
            .enumerate()
            .filter_map(|(index, (event, ..))| {
                let fts_rank = fts_ranks.get(&index).copied();
                let semantic_rank = semantic_ranks.get(&index).copied();
                let fts_score =
                    (aligned_fts_scores[index] > 0.0).then_some(aligned_fts_scores[index]);
                let semantic_score =
                    (semantic_scores[index] > 0.0).then_some(semantic_scores[index]);
                let included = match effective_mode {
                    SearchMode::Fts => fts_rank.is_some(),
                    SearchMode::Semantic => semantic_rank.is_some(),
                    SearchMode::Hybrid => fts_rank.is_some() || semantic_rank.is_some(),
                };
                included.then(|| SearchHit {
                    session_id: event.session_id.clone(),
                    event_id: event.event_id,
                    kind: event.kind.clone(),
                    score: combined_score(
                        effective_mode,
                        fts_rank,
                        semantic_rank,
                        fts_score,
                        semantic_score,
                    ),
                    fts_score,
                    semantic_score,
                    snippet: snippet(&documents[index], &query.query, 240),
                    event,
                })
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.session_id.cmp(&right.session_id))
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        hits.truncate(query.limit());
        let semantic_floor = !semantic_available;
        Ok(SearchResponse {
            requested_mode: query.mode,
            effective_mode,
            embedding_backend: embedder.name().to_string(),
            semantic_floor,
            fallback_reason: (semantic_floor && query.mode != SearchMode::Fts)
                .then(|| "semantic model unavailable; FTS-only fallback active".into()),
            hits,
        })
    }
}

fn append_search_scope(
    sql: &mut String,
    args: &mut Vec<(&'static str, rusqlite::types::Value)>,
    query: &SearchQuery,
) {
    if let Some(tenant_id) = query.filter.tenant_id.as_ref() {
        sql.push_str(" AND s.tenant_id = :search_tenant");
        args.push((":search_tenant", tenant_id.clone().into()));
    }
    if let Some(project_scope) = query.filter.project_scope.as_ref() {
        sql.push_str(" AND s.project_scope = :search_project");
        args.push((":search_project", project_scope.clone().into()));
    }
    if let Some(session_id) = query.filter.session_id.as_ref() {
        sql.push_str(" AND s.id = :search_session");
        args.push((":search_session", session_id.clone().into()));
    }
}
