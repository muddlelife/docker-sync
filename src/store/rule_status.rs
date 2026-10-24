use super::*;
use crate::domain::*;

impl Store {
    pub(super) fn rule_sync(conn: &Connection, rule: &RuleView) -> Result<RuleSyncStatus> {
        // Match the frozen configuration, across all history, before selecting the latest task.
        let row: Option<(String, Option<String>)> = conn.query_row(
            "SELECT t.data,a.result FROM tasks t
             JOIN json_each(t.data,'$.items') i
             LEFT JOIN attempts a ON a.task_id=t.id AND a.attempt=json_extract(t.data,'$.run_attempt')
             WHERE json_extract(i.value,'$.item_id')=?1
               AND json_extract(i.value,'$.source')=?2
               AND json_extract(i.value,'$.target')=?3
               AND json_extract(i.value,'$.platform')=?4
             ORDER BY t.rowid DESC LIMIT 1",
            rusqlite::params![rule.id.to_string(),rule.source,rule.target,rule.platform.label()],
            |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((data, result)) = row else {
            return Ok(RuleSyncStatus::default());
        };
        let task: TaskView = decode(data)?;
        let mut sync = RuleSyncStatus {
            warning: task.refresh_error,
            status: task.status.as_str().into(),
            task_id: Some(task.id),
            updated_at: Some(task.updated_at),
            elapsed_ms: None,
            error: task.error,
        };
        if let Some(result) =
            result.filter(|_| matches!(task.status, TaskStatus::Succeeded | TaskStatus::Failed))
        {
            let result: BatchResult = decode(result)?;
            if let Some(item) = result.items.iter().find(|i| {
                i.item_id == rule.id
                    && i.source == rule.source
                    && i.target == rule.target
                    && i.platform == rule.platform
            }) {
                sync.status = match item.outcome {
                    ItemOutcome::Synced => "succeeded",
                    ItemOutcome::Unchanged => "unchanged",
                    ItemOutcome::Failed => "failed",
                }
                .into();
                sync.elapsed_ms = Some(item.elapsed_ms);
                sync.error = item.error.clone();
            }
        } else if task.status == TaskStatus::Succeeded {
            sync.status = "result_pending".into();
        }
        Ok(sync)
    }
}
