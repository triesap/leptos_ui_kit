#![forbid(unsafe_code)]

//! Code generation and install-planning layer.

use serde::Serialize;

pub use leptos_ui_kit_registry::SCHEMA_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEnvelope<T>
where
    T: Serialize,
{
    pub schema_version: &'static str,
    pub command: String,
    pub status: CommandStatus,
    pub diagnostics: Vec<Diagnostic>,
    pub changes: Vec<ChangeRecord>,
    pub data: T,
}

impl<T> CommandEnvelope<T>
where
    T: Serialize,
{
    pub fn new(command: impl Into<String>, status: CommandStatus, data: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.into(),
            status,
            diagnostics: Vec::new(),
            changes: Vec::new(),
            data,
        }
    }

    pub fn success(command: impl Into<String>, data: T) -> Self {
        Self::new(command, CommandStatus::Success, data)
    }

    pub fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub fn with_changes(mut self, changes: Vec<ChangeRecord>) -> Self {
        self.changes = changes;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Success,
    NoChange,
    Planned,
    Warning,
    Error,
    Conflict,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl Diagnostic {
    pub fn new(
        level: DiagnosticLevel,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            level,
            code: code.into(),
            message: message.into(),
            path: None,
            suggestion: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub kind: ChangeKind,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    pub tracked: bool,
}

impl ChangeRecord {
    pub fn new(kind: ChangeKind, path: impl Into<String>, tracked: bool) -> Self {
        Self {
            kind,
            path: path.into(),
            item: None,
            tracked,
        }
    }

    pub fn with_item(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    CreateFile,
    UpdateFile,
    DeleteFile,
    CreateDir,
    UpdateCssBlock,
    WriteState,
    WriteBaseline,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct DemoData {
        value: &'static str,
    }

    #[test]
    fn serializes_diagnostics_and_change_records_in_json_envelope() {
        let envelope =
            CommandEnvelope::new("add", CommandStatus::Planned, DemoData { value: "ok" })
                .with_diagnostics(vec![
                    Diagnostic::new(DiagnosticLevel::Warning, "demo.warning", "heads up")
                        .with_path("components.json")
                        .with_suggestion("Run leptos-ui init."),
                ])
                .with_changes(vec![
                    ChangeRecord::new(ChangeKind::CreateFile, "src/components/ui/button.rs", true)
                        .with_item("builtin:button"),
                ]);

        let json = serde_json::to_string(&envelope).expect("serialize envelope");

        assert!(json.contains(r#""schemaVersion":"0.9.0-alpha""#));
        assert!(json.contains(r#""command":"add""#));
        assert!(json.contains(r#""status":"planned""#));
        assert!(json.contains(r#""level":"warning""#));
        assert!(json.contains(r#""kind":"create_file""#));
        assert!(json.contains(r#""data":{"value":"ok"}"#));
    }
}
