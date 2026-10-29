//! Index schema: 21 fields, one document per
//! session file. Metadata fields are also fast (columnar) so the session list
//! loads without touching the docstore, which holds the large `content`.

use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, STRING, Schema, TEXT, TextFieldIndexing, TextOptions,
};

/// Field handles, looked up by name.
#[derive(Debug, Clone, Copy)]
pub struct Fields {
    pub session_id: Field,
    pub agent: Field,
    pub project: Field,
    pub branch: Field,
    pub cwd: Field,
    pub created: Field,
    pub modified: Field,
    pub modified_ts: Field,
    pub lines: Field,
    pub export_path: Field,
    pub first_msg_role: Field,
    pub first_msg_content: Field,
    pub last_msg_role: Field,
    pub last_msg_content: Field,
    pub first_user_msg_content: Field,
    pub derivation_type: Field,
    pub is_sidechain: Field,
    pub is_exec_run: Field,
    pub claude_home: Field,
    pub custom_title: Field,
    pub content: Field,
}

/// Tokenized (default tokenizer, positions), stored, raw fast column.
fn meta_text() -> TextOptions {
    TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("default")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored()
        .set_fast("raw")
}

/// Untokenized key, stored, raw fast column.
fn raw_key() -> TextOptions {
    (STRING | STORED).set_fast("raw")
}

/// The index schema.
pub fn build_schema() -> Schema {
    let mut b = Schema::builder();
    for name in [
        "session_id",
        "agent",
        "project",
        "branch",
        "cwd",
        "created",
        "modified",
    ] {
        b.add_text_field(name, meta_text());
    }
    b.add_u64_field("modified_ts", FAST | STORED);
    b.add_i64_field("lines", FAST | STORED);
    b.add_text_field("export_path", raw_key());
    for name in [
        "first_msg_role",
        "first_msg_content",
        "last_msg_role",
        "last_msg_content",
        "first_user_msg_content",
        "derivation_type",
        "is_sidechain",
        "is_exec_run",
    ] {
        b.add_text_field(name, meta_text());
    }
    b.add_text_field("claude_home", raw_key());
    b.add_text_field("custom_title", meta_text());
    b.add_text_field("content", TEXT | STORED);
    b.build()
}

impl Fields {
    /// Resolve every field of [`build_schema`] in `schema`.
    pub fn new(schema: &Schema) -> tantivy::Result<Fields> {
        let f = |n: &str| schema.get_field(n);
        Ok(Fields {
            session_id: f("session_id")?,
            agent: f("agent")?,
            project: f("project")?,
            branch: f("branch")?,
            cwd: f("cwd")?,
            created: f("created")?,
            modified: f("modified")?,
            modified_ts: f("modified_ts")?,
            lines: f("lines")?,
            export_path: f("export_path")?,
            first_msg_role: f("first_msg_role")?,
            first_msg_content: f("first_msg_content")?,
            last_msg_role: f("last_msg_role")?,
            last_msg_content: f("last_msg_content")?,
            first_user_msg_content: f("first_user_msg_content")?,
            derivation_type: f("derivation_type")?,
            is_sidechain: f("is_sidechain")?,
            is_exec_run: f("is_exec_run")?,
            claude_home: f("claude_home")?,
            custom_title: f("custom_title")?,
            content: f("content")?,
        })
    }
}
