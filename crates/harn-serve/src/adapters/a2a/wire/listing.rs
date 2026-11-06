//! Typed A2A listing over the canonical task store and status-publication times.
use super::super::schema::task_to_json_versioned;
use super::*;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
struct ListOptions {
    tenant: String,
    context_id: String,
    status: String,
    page_size: Option<u8>,
    page_token: String,
    history_length: Option<u32>,
    status_timestamp_after: Option<String>,
    include_artifacts: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    options: ListOptions,
    timestamp: String,
    id: String,
}

pub(in super::super) struct ListQuery {
    options: ListOptions,
    after: Option<OffsetDateTime>,
    cursor: Option<(OffsetDateTime, String)>,
}

fn timestamp(value: &str) -> Result<OffsetDateTime, &'static str> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| "Expected RFC3339 status timestamp")
}

impl ListQuery {
    pub(in super::super) fn decode(params: &JsonValue) -> Result<Self, &'static str> {
        let mut options: ListOptions =
            serde_json::from_value(params.clone()).map_err(|_| "Invalid ListTasks parameters")?;
        if options
            .page_size
            .is_some_and(|size| !(1..=100).contains(&size))
            || options
                .history_length
                .is_some_and(|length| length > i32::MAX as u32)
        {
            return Err("ListTasks pageSize must be 1..100 and historyLength a nonnegative int32");
        }
        if !options.status.is_empty()
            && ![
                TaskStatus::Submitted,
                TaskStatus::Working,
                TaskStatus::InputRequired,
                TaskStatus::AuthRequired,
                TaskStatus::Completed,
                TaskStatus::Failed,
                TaskStatus::Cancelled,
                TaskStatus::Rejected,
            ]
            .iter()
            .any(|status| state_name(status.as_str()) == options.status)
        {
            return Err("Unknown ListTasks status");
        }
        let after = options
            .status_timestamp_after
            .as_deref()
            .map(timestamp)
            .transpose()?;
        let token = std::mem::take(&mut options.page_token);
        let cursor = if token.is_empty() {
            None
        } else {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token)
                .map_err(|_| "Invalid ListTasks pageToken")?;
            let cursor: Cursor =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid ListTasks pageToken")?;
            if cursor.options != options || cursor.id.is_empty() {
                return Err("ListTasks pageToken does not match request filters");
            }
            Some((timestamp(&cursor.timestamp)?, cursor.id))
        };
        Ok(Self {
            options,
            after,
            cursor,
        })
    }

    pub(in super::super) fn execute(
        &self,
        server: &A2aServer,
        principal: Option<&crate::AuthenticatedPrincipal>,
    ) -> JsonValue {
        let tasks = server.tasks.lock().expect("tasks poisoned");
        let mut selected: Vec<_> = tasks
            .values()
            .filter_map(|task| {
                if !task.visible_to(principal)
                    || (!self.options.context_id.is_empty()
                        && task.context_id.as_deref().unwrap_or(&task.id)
                            != self.options.context_id)
                    || (!self.options.status.is_empty()
                        && state_name(task.status.as_str()) != self.options.status)
                {
                    return None;
                }
                let updated = task.status_updated_at;
                if self.after.is_some_and(|after| updated < after) {
                    return None;
                }
                Some((updated, task.id.as_str(), task))
            })
            .collect();
        selected.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
        let total_size = selected.len();
        let page_size = self.options.page_size.unwrap_or(50) as usize;
        let mut remaining = selected.iter().filter(|(updated, id, _)| {
            self.cursor
                .as_ref()
                .is_none_or(|(before, last)| (*updated, *id) < (*before, last.as_str()))
        });
        let page: Vec<_> = remaining.by_ref().take(page_size).collect();
        let next = if remaining.next().is_some() {
            let (updated, id, _) = page.last().expect("nonempty page");
            let cursor = json!({"options":self.options, "timestamp":harn_clock::format_rfc3339(*updated), "id":id});
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&cursor).expect("JSON cursor"))
        } else {
            String::new()
        };
        let tasks: Vec<_> = page
            .into_iter()
            .map(|(_, _, task)| {
                let mut value = task_to_json_versioned(task, harn_vm::a2a::A2aWireVersion::V1_0);
                limit_history(
                    &mut value,
                    self.options.history_length.map(|length| length as usize),
                );
                if !self.options.include_artifacts {
                    value.as_object_mut().unwrap().remove("artifacts");
                }
                value
            })
            .collect();
        json!({"tasks": tasks, "nextPageToken":next, "pageSize":page_size, "totalSize":total_size})
    }
}
