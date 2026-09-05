fn initialize_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;

    for statement in SCHEMA_STATEMENTS {
        conn.execute_batch(statement)?;
    }
    reconcile_declared_columns(conn)?;

    let stored_version: Option<String> = conn
        .query_row(
            "SELECT value FROM ai_history_meta WHERE key = 'normalized_history_schema_version' LIMIT 1;",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if stored_version.as_deref() != Some(NORMALIZED_HISTORY_SCHEMA_VERSION)
        || !usage_event_schema_is_current(conn)?
    {
        migrate_schema(conn)?;
    }
    create_project_scope_views(conn, false)?;
    Ok(())
}

/// Give every table the columns its declaration in [`SCHEMA_STATEMENTS`] has.
///
/// `CREATE TABLE IF NOT EXISTS` cannot evolve a table that already exists, so a
/// database written by an older build keeps that build's columns while the
/// declarations move on. Everything after this point assumes the two agree --
/// the migration reads columns straight off the declared shape -- and when they
/// do not it fails inside its transaction and rolls back, taking the stored
/// schema version with it. The next launch then fails identically, so the
/// database stalls on its old version forever and the usage tables stay empty
/// with nothing reported anywhere.
///
/// The declared shape is obtained by running the same statements against a
/// fresh in-memory database, which by construction gets the current shape,
/// rather than by parsing them or by keeping a second list of columns in step
/// by hand. The declarations stay the single source of truth: adding a column
/// there needs no change here.
fn reconcile_declared_columns(conn: &Connection) -> Result<()> {
    let model = Connection::open_in_memory()?;
    for statement in SCHEMA_STATEMENTS {
        model.execute_batch(statement)?;
    }
    for table in declared_table_names(&model)? {
        let existing = table_columns(conn, &table)?;
        if existing.is_empty() {
            continue;
        }
        for column in declared_columns(&model, &table)? {
            // A key column cannot be bolted on: a table whose key has drifted
            // needs rebuilding, which is the migration's job, not this one's.
            if existing.contains(&column.name) || column.primary_key {
                continue;
            }
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {} {};",
                column.name,
                column.added_definition()
            ))?;
        }
    }
    Ok(())
}

struct DeclaredColumn {
    name: String,
    kind: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key: bool,
}

impl DeclaredColumn {
    /// SQLite refuses to add a NOT NULL column without a default, and the
    /// declarations carry none. Fill in the type's zero: rows that predate the
    /// column have no value for it, and zero is what "absent" has to read as.
    fn added_definition(&self) -> String {
        let mut sql = self.kind.clone();
        if let Some(default_value) = &self.default_value {
            sql.push_str(&format!(" DEFAULT {default_value}"));
        } else if self.not_null {
            let zero = if self.kind.eq_ignore_ascii_case("TEXT") {
                "''"
            } else {
                "0"
            };
            sql.push_str(&format!(" DEFAULT {zero}"));
        }
        if self.not_null {
            sql.push_str(" NOT NULL");
        }
        sql
    }
}

fn declared_table_names(model: &Connection) -> Result<Vec<String>> {
    let mut statement =
        model.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name;")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

fn declared_columns(model: &Connection, table: &str) -> Result<Vec<DeclaredColumn>> {
    let mut statement = model.prepare(&format!("PRAGMA table_info({table});"))?;
    let columns = statement
        .query_map([], |row| {
            Ok(DeclaredColumn {
                name: row.get(1)?,
                kind: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default_value: row.get(4)?,
                primary_key: row.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

// Per-connection views every global query reads from; scoping is defined once
// here instead of per-statement.
fn create_project_scope_views(conn: &Connection, scoped: bool) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP VIEW IF EXISTS temp.scoped_file_usage_bucket;
        DROP VIEW IF EXISTS temp.scoped_file_session_link;
        "#,
    )?;
    let filter = if scoped {
        " WHERE project_path IN (SELECT path FROM temp.scope_project_path)"
    } else {
        ""
    };
    conn.execute_batch(&format!(
        r#"
        CREATE TEMP VIEW scoped_file_usage_bucket AS
            SELECT * FROM ai_history_file_usage_bucket{filter};
        CREATE TEMP VIEW scoped_file_session_link AS
            SELECT * FROM ai_history_file_session_link{filter};
        "#
    ))?;
    Ok(())
}

fn jsonl_index_mode(
    current_file_size: i64,
    current_modified_at: f64,
    stored_summary: Option<&AIExternalFileSummary>,
    checkpoint: Option<&AIExternalFileCheckpoint>,
) -> JSONLIndexMode {
    let (Some(stored_summary), Some(checkpoint)) = (stored_summary, checkpoint) else {
        return JSONLIndexMode::Rebuild;
    };
    if current_file_size < checkpoint.file_size {
        return JSONLIndexMode::Rebuild;
    }
    if checkpoint.last_offset < current_file_size {
        return JSONLIndexMode::Append;
    }
    if same_timestamp(stored_summary.file_modified_at, current_modified_at)
        && same_timestamp(checkpoint.file_modified_at, current_modified_at)
        && checkpoint.last_offset >= current_file_size
    {
        return JSONLIndexMode::Unchanged;
    }
    if current_file_size >= checkpoint.file_size && checkpoint.last_offset <= current_file_size {
        return JSONLIndexMode::Append;
    }
    JSONLIndexMode::Rebuild
}

fn merge_usage_buckets(existing: &[AIUsageBucket], delta: &[AIUsageBucket]) -> Vec<AIUsageBucket> {
    let mut map = HashMap::<(String, String, String, i64), AIUsageBucket>::new();
    for bucket in existing.iter().chain(delta.iter()) {
        let key = (
            bucket.source.clone(),
            bucket.session_key.clone(),
            bucket.model.clone().unwrap_or_default(),
            bucket.bucket_start as i64,
        );
            map.entry(key)
            .and_modify(|current| {
                if bucket.last_seen_at > current.last_seen_at
                    || (same_timestamp(bucket.last_seen_at, current.last_seen_at)
                        && bucket.session_title > current.session_title)
                {
                    current.session_title = bucket.session_title.clone();
                }
                current.input_tokens += bucket.input_tokens;
                current.output_tokens += bucket.output_tokens;
                current.total_tokens += bucket.total_tokens;
                current.cached_input_tokens += bucket.cached_input_tokens;
                merge_usage_amounts(&mut current.usage_amounts, &bucket.usage_amounts);
                current.request_count += bucket.request_count;
                current.active_duration_seconds += bucket.active_duration_seconds;
                current.first_seen_at = min_nonzero(current.first_seen_at, bucket.first_seen_at);
                current.last_seen_at = current.last_seen_at.max(bucket.last_seen_at);
                stable_optional_string(
                    &mut current.external_session_id,
                    bucket.external_session_id.as_deref(),
                );
                current.model = current.model.clone().or(bucket.model.clone());
            })
            .or_insert_with(|| bucket.clone());
    }
    let mut values = map.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        left.bucket_start
            .total_cmp(&right.bucket_start)
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.session_key.cmp(&right.session_key))
            .then_with(|| left.model.cmp(&right.model))
    });
    values
}
