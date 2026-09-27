//! Gives a new account its own database with the schema of an existing one.
//!
//! The migrations cannot build a database from nothing: everything up to
//! [`migrate::BASELINE`](super::migrate) was applied by hand, several of those
//! files name `telegram_user_bot` outright, and 022 moves tables that only the
//! first account ever had. So a second account's database is made the way the
//! first one looks now -- every table's `CREATE` statement read from
//! `system.tables` and replayed under the new name -- together with the
//! `schema_migrations` record, so the runner carries on from the same place.

use anyhow::{Context, bail};
use clickhouse::{Client, Row};
use log::info;
use serde::Deserialize;

#[derive(Row, Deserialize)]
struct TableDef {
    name: String,
    engine: String,
    create_table_query: String,
}

/// Whether `db` has no tables yet. A missing database counts as empty.
pub async fn is_empty(admin: &Client, db: &str) -> anyhow::Result<bool> {
    let n: u64 = admin
        .query(
            "SELECT count() FROM system.tables WHERE database = ? AND name != 'schema_migrations'",
        )
        .bind(db)
        .fetch_one()
        .await
        .context("counting tables")?;
    Ok(n == 0)
}

/// Create `target` with every table, view and buffer of `source`, and the same
/// migration record. `admin` is a client not tied to either database.
pub async fn run(admin: &Client, source: &str, target: &str) -> anyhow::Result<()> {
    if !is_empty(admin, target).await? {
        bail!("database {target} already has tables; not cloning over them");
    }
    let defs: Vec<TableDef> = admin
        .query(
            "SELECT ?fields FROM system.tables \
             WHERE database = ? AND NOT is_temporary AND NOT startsWith(name, '.inner') \
             ORDER BY name",
        )
        .bind(source)
        .fetch_all()
        .await
        .context("reading the source schema")?;
    if defs.is_empty() {
        bail!("database {source} has no tables to clone");
    }

    admin
        .query(&format!("CREATE DATABASE IF NOT EXISTS `{target}`"))
        .execute()
        .await
        .context("creating the database")?;

    // Storage first, then what reads from it: a Buffer names its destination,
    // a view the tables it selects from.
    let mut pending: Vec<&TableDef> = defs.iter().collect();
    pending.sort_by_key(|d| rank(&d.engine));
    while !pending.is_empty() {
        let mut failed = Vec::new();
        let mut last_error = None;
        for def in &pending {
            let sql = retarget(&def.create_table_query, source, target);
            match admin.query(&sql).execute().await {
                Ok(()) => info!("clone: {target}.{}", def.name),
                Err(e) => {
                    last_error = Some(format!("{}: {e}", def.name));
                    failed.push(*def);
                }
            }
        }
        // A view over a view created later in the same pass succeeds on the
        // next; a pass that gets nothing further through never will.
        if failed.len() == pending.len() {
            bail!("could not create: {}", last_error.unwrap_or_default());
        }
        pending = failed;
    }

    admin
        .query(&format!(
            "INSERT INTO `{target}`.schema_migrations SELECT * FROM `{source}`.schema_migrations"
        ))
        .execute()
        .await
        .context("copying schema_migrations")?;
    Ok(())
}

fn rank(engine: &str) -> u8 {
    match engine {
        "Buffer" => 1,
        "View" => 2,
        "MaterializedView" => 3,
        _ => 0,
    }
}

/// The statement with every reference to `source` pointed at `target`:
/// qualified names (`source.table`, `` `source`.table ``) and the database
/// literal a Buffer or Dictionary carries (`'source'`).
fn retarget(sql: &str, source: &str, target: &str) -> String {
    sql.replace(&format!("{source}."), &format!("{target}."))
        .replace(&format!("`{source}`."), &format!("`{target}`."))
        .replace(&format!("'{source}'"), &format!("'{target}'"))
        .replacen("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ", 1)
        .replacen("CREATE VIEW ", "CREATE VIEW IF NOT EXISTS ", 1)
        .replacen(
            "CREATE MATERIALIZED VIEW ",
            "CREATE MATERIALIZED VIEW IF NOT EXISTS ",
            1,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retarget_moves_names_but_not_lookalikes() {
        let sql = "CREATE MATERIALIZED VIEW telegram_user_bot.mv TO telegram_user_bot.stat \
                   AS SELECT * FROM telegram_user_bot.events_log \
                   JOIN telegram_user_bot_legacy.old USING id";
        assert_eq!(
            retarget(sql, "telegram_user_bot", "tg2"),
            "CREATE MATERIALIZED VIEW IF NOT EXISTS tg2.mv TO tg2.stat \
             AS SELECT * FROM tg2.events_log \
             JOIN telegram_user_bot_legacy.old USING id"
        );
        assert_eq!(
            retarget(
                "CREATE TABLE telegram_user_bot.b ENGINE = Buffer('telegram_user_bot', 'events_log', 1)",
                "telegram_user_bot",
                "tg2"
            ),
            "CREATE TABLE IF NOT EXISTS tg2.b ENGINE = Buffer('tg2', 'events_log', 1)"
        );
    }
}
