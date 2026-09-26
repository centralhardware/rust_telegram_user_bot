//! Brings the ClickHouse schema up to date at startup, before anything is
//! written: every file in `migrations/` that `schema_migrations` does not
//! list is applied, in order, and recorded.
//!
//! The files are compiled into the binary (see `build.rs`), so a deploy always
//! carries exactly the schema its code expects.
//!
//! Migrations up to [`BASELINE`] were applied by hand before this runner
//! existed. On a database with no record yet they are written down as applied,
//! not run again: several of them move or rewrite data and are not safe to
//! repeat.

use anyhow::{Context, bail};
use clickhouse::{Client, Row};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// The last migration applied by hand, before the runner.
const BASELINE: u32 = 46;

const TABLE: &str = "schema_migrations";

#[derive(Row, Serialize, Deserialize)]
struct Applied {
    version: u32,
    name: String,
    checksum: String,
}

pub async fn run(ch: &Client) -> anyhow::Result<()> {
    ch.query(&format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} ( \
             version UInt32, \
             name String, \
             checksum String, \
             applied_at DateTime DEFAULT now() \
         ) ENGINE = MergeTree ORDER BY version"
    ))
    .execute()
    .await
    .context("creating schema_migrations")?;

    let applied: Vec<Applied> = ch
        .query(&format!("SELECT ?fields FROM {TABLE} ORDER BY version"))
        .fetch_all()
        .await
        .context("reading schema_migrations")?;

    let files = parse_files()?;

    if applied.is_empty() {
        let baseline: Vec<Applied> = files
            .iter()
            .filter(|f| f.version <= BASELINE)
            .map(|f| Applied { version: f.version, name: f.name.to_string(), checksum: f.checksum.clone() })
            .collect();
        info!("migrations: recording {} applied by hand as the baseline", baseline.len());
        record(ch, &baseline).await?;
        return apply_after(ch, &files, BASELINE).await;
    }

    for file in &files {
        if let Some(done) = applied.iter().find(|a| a.version == file.version)
            && done.checksum != file.checksum
        {
            // Editing an applied migration changes nothing in the database;
            // say so rather than leave the file looking authoritative.
            warn!("migrations: {} changed after it was applied", file.name);
        }
    }
    let last = applied.iter().map(|a| a.version).max().unwrap_or(0);
    apply_after(ch, &files, last).await
}

struct File {
    version: u32,
    name: &'static str,
    sql: &'static str,
    checksum: String,
}

fn parse_files() -> anyhow::Result<Vec<File>> {
    let mut files = Vec::with_capacity(MIGRATIONS.len());
    for &(name, sql) in MIGRATIONS {
        let version = name
            .split('_')
            .next()
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("migration {name} has no number"))?;
        if files.iter().any(|f: &File| f.version == version) {
            bail!("two migrations numbered {version}");
        }
        let checksum = Sha256::digest(sql.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        files.push(File { version, name, sql, checksum });
    }
    Ok(files)
}

async fn apply_after(ch: &Client, files: &[File], last: u32) -> anyhow::Result<()> {
    for file in files.iter().filter(|f| f.version > last) {
        info!("migrations: applying {}", file.name);
        let mut settings: Vec<(String, String)> = Vec::new();
        for statement in statements(file.sql) {
            // `SET` holds for one HTTP request only, so it is carried over as a
            // setting on each statement after it in the same file.
            if let Some((name, value)) = set_statement(&statement) {
                settings.push((name, value));
                continue;
            }
            let mut query = ch.query(&statement);
            for (name, value) in &settings {
                query = query.with_setting(name, value);
            }
            query
                .execute()
                .await
                .with_context(|| format!("migration {} failed", file.name))?;
        }
        record(
            ch,
            &[Applied { version: file.version, name: file.name.to_string(), checksum: file.checksum.clone() }],
        )
        .await?;
    }
    Ok(())
}

async fn record(ch: &Client, rows: &[Applied]) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    super::ch::insert_rows(ch, TABLE, rows).await.context("recording migrations")
}

/// `SET name = value`, as a setting to carry.
fn set_statement(statement: &str) -> Option<(String, String)> {
    let rest = statement.trim().strip_prefix("SET ").or_else(|| statement.trim().strip_prefix("set "))?;
    let (name, value) = rest.split_once('=')?;
    Some((name.trim().to_string(), value.trim().trim_matches('\'').to_string()))
}

/// The statements of a file: `--` comments dropped, split on `;` outside
/// quotes, empty pieces left out.
fn statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                current.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '`' | '"' => {
                    quote = Some(c);
                    current.push(c);
                }
                '-' if chars.peek() == Some(&'-') => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            current.push('\n');
                            break;
                        }
                    }
                }
                ';' => {
                    out.push(std::mem::take(&mut current));
                }
                _ => current.push(c),
            },
        }
    }
    out.push(current);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_quoted_semicolons_do_not_split() {
        let sql = "-- a comment; with a semicolon\nCREATE TABLE t (a String COMMENT 'x; y');\n\n-- end;\nDROP TABLE u;";
        assert_eq!(
            statements(sql),
            ["CREATE TABLE t (a String COMMENT 'x; y')", "DROP TABLE u"]
        );
    }

    #[test]
    fn a_set_becomes_a_setting() {
        assert_eq!(
            set_statement("SET allow_suspicious_low_cardinality_types = 1"),
            Some(("allow_suspicious_low_cardinality_types".into(), "1".into()))
        );
        assert_eq!(set_statement("SELECT 1"), None);
    }

    #[test]
    fn every_migration_is_numbered_once_and_splits_into_statements() {
        let files = parse_files().unwrap();
        assert!(files.len() >= BASELINE as usize);
        for f in &files {
            let parts = statements(f.sql);
            assert!(!parts.is_empty(), "{} has no statements", f.name);
            for p in parts {
                let first = p.split_whitespace().next().unwrap().to_uppercase();
                assert!(
                    ["CREATE", "ALTER", "DROP", "INSERT", "RENAME", "SET", "OPTIMIZE", "TRUNCATE", "EXCHANGE", "DETACH", "ATTACH", "SYSTEM"]
                        .contains(&first.as_str()),
                    "{}: unexpected statement start {first:?}",
                    f.name
                );
            }
        }
    }
}
