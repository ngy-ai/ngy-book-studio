//! Turning a stored attempt into the report the UI reads.

use super::*;

pub(crate) fn present(attempt: &StoredAttempt) -> LearningRunReport {
    let raw = &attempt.report;
    let model_call_label = if raw
        .pointer("/request/chapter")
        .and_then(Value::as_u64)
        .is_some_and(|chapter| chapter > 1)
    {
        "练习输入获取"
    } else {
        "模型决策"
    };
    let text = |pointer: &str| {
        raw.pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut trace = Vec::new();
    let tool_calls = raw
        .pointer("/observations/tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut displayed = std::collections::HashSet::new();
    if let Some(calls) = raw
        .pointer("/observations/model_calls")
        .and_then(Value::as_array)
    {
        for (index, call) in calls.iter().enumerate() {
            trace.push(LearningTrace {
                label: format!("{model_call_label} {}", index + 1),
                detail: serde_json::to_string_pretty(call).unwrap_or_default(),
            });
            if let Some(requested) = call
                .pointer("/response/tool_calls")
                .and_then(Value::as_array)
            {
                for request in requested {
                    if let Some((position, tool)) = tool_calls
                        .iter()
                        .enumerate()
                        .find(|(_, tool)| tool.get("tool_call_id") == request.get("id"))
                        && displayed.insert(position)
                    {
                        trace.push(tool_trace(position, tool));
                    }
                }
            }
        }
    }
    for (index, call) in tool_calls.iter().enumerate() {
        if !displayed.contains(&index) {
            trace.push(tool_trace(index, call));
        }
    }
    if let Some(checks) = raw.get("checks").and_then(Value::as_array) {
        for check in checks {
            trace.push(LearningTrace {
                label: format!(
                    "{} · {}",
                    if check.get("passed").and_then(Value::as_bool) == Some(true) {
                        "通过"
                    } else {
                        "未通过"
                    },
                    check.get("id").and_then(Value::as_str).unwrap_or("检查")
                ),
                detail: check
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            });
        }
    }
    if let Some(error) = raw.get("error").filter(|value| !value.is_null()) {
        trace.push(LearningTrace {
            label: "运行诊断".into(),
            detail: serde_json::to_string_pretty(error).unwrap_or_default(),
        });
    }
    let count = |key: &str| {
        raw.pointer(&format!("/metrics/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    LearningRunReport {
        id: attempt.id.clone(),
        created_at: attempt.created_at.clone(),
        status: text("/outcome/status"),
        stop_reason: text("/outcome/stop_reason"),
        passed: raw.get("passed").and_then(Value::as_bool).unwrap_or(false),
        imported: attempt.imported,
        answer: raw
            .pointer("/outcome/answer")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_default(),
        metrics: LearningMetrics {
            model_decisions: count("model_decisions"),
            actual_tool_executions: count("actual_tool_executions"),
            framework_steps: count("framework_steps"),
            elapsed_seconds: raw
                .pointer("/metrics/elapsed_seconds")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.)
                .unwrap_or(0.),
        },
        trace,
        learning_status: if attempt.imported {
            "导入记录 · 待核验"
        } else {
            "学习证据待核验"
        }
        .into(),
        raw: raw.clone(),
        workspace: attempt.workspace.clone(),
    }
}

pub(crate) fn tool_trace(index: usize, call: &Value) -> LearningTrace {
    LearningTrace {
        label: format!(
            "工具结果 {} · {}",
            index + 1,
            call.get("name").and_then(Value::as_str).unwrap_or("")
        ),
        detail: serde_json::to_string_pretty(call).unwrap_or_default(),
    }
}
