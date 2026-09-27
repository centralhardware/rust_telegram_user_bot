//! `telegram_user_bot setup`: log an account in from a machine with a
//! terminal, and leave the session in ClickHouse for the server to run on.
//!
//! The bot itself never asks for a login code -- a headless server has no one
//! to type it. Instead this runs wherever there is a terminal, against the same
//! ClickHouse (directly, or through an SSH tunnel), with the same `TG_*` and
//! `CLICKHOUSE_*` settings the server will use. MTProto keys are not tied to an
//! address, so the session it writes works from the server as it is.
//!
//! Each account lives in its own database, named by `CLICKHOUSE_DATABASE`. For
//! a new account, point that at a database that does not exist yet and pass
//! `--schema-from <database>` to copy the schema of an existing one first.

use anyhow::{Context, bail};

use crate::db;

const USAGE: &str = "usage: telegram_user_bot setup [--schema-from <database>]";

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let schema_from = match args {
        [] => None,
        [flag, db] if flag == "--schema-from" => Some(db.as_str()),
        _ => bail!("{USAGE}"),
    };

    let target = std::env::var("CLICKHOUSE_DATABASE").context("CLICKHOUSE_DATABASE not set")?;
    let ch = db::ch::ClickhouseDb::from_env().client().clone();
    let admin = ch.clone().with_database("default");

    if db::clone_schema::is_empty(&admin, &target).await? {
        let Some(source) = schema_from else {
            bail!(
                "database {target} has no tables; pass --schema-from <database> \
                 to copy the schema of an existing account"
            );
        };
        println!("Copying the schema of {source} into {target}...");
        db::clone_schema::run(&admin, source, &target).await?;
    }
    db::migrate::run(&ch).await?;

    // The session writes are awaited, but async inserts only queue on the
    // server; wait for them to land, since this process exits right after.
    let ch = ch.with_setting("wait_for_async_insert", "1");
    let me = crate::session::login(ch).await?;
    println!("Logged in as {me}. Session saved in database {target}.");
    println!("Start the bot on the server with CLICKHOUSE_DATABASE={target}.");
    Ok(())
}
