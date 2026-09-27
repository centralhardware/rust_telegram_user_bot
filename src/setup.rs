//! `telegram_user_bot setup [<name>]`: log an account in from a machine with a
//! terminal, and leave the session in ClickHouse for the server to run on.
//!
//! The bot itself never asks for a login code -- a headless server has no one
//! to type it. Instead this runs wherever there is a terminal, against the same
//! ClickHouse (directly, or through an SSH tunnel), with the same `TG_*` and
//! `CLICKHOUSE_*` settings the server uses. MTProto keys are not tied to an
//! address, so the session it writes works from the server as it is.
//!
//! Without a name it logs in the first account. With one, it logs in a further
//! account under that name and records it in `accounts`; the running bot finds
//! it there within a minute and starts it, no restart needed.

use anyhow::bail;
use clickhouse::Row;
use serde::Serialize;

use crate::db;

#[derive(Row, Serialize)]
struct AccountRow {
    name: String,
    user_id: u64,
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let name = match args {
        [] => "",
        [name] if valid_name(name) => name.as_str(),
        _ => bail!(
            "usage: telegram_user_bot setup [<name>]\n\
             <name>: lowercase letters, digits and _; leave it out for the first account"
        ),
    };

    let ch = db::ch::ClickhouseDb::from_env().client().clone();
    db::migrate::run(&ch).await?;

    // The session writes are awaited, but async inserts only queue on the
    // server; wait for them to land, since this process exits right after.
    let ch = ch.with_setting("wait_for_async_insert", "1");
    let me = crate::session::login(ch.clone(), name).await?;
    let user_id = me.id().bare_id_unchecked() as u64;
    if !name.is_empty() {
        db::ch::insert_rows(
            &ch,
            "accounts",
            &[AccountRow {
                name: name.to_string(),
                user_id,
            }],
        )
        .await?;
    }
    let who = match me.username() {
        Some(u) => format!("{} (@{u}, id {user_id})", me.full_name()),
        None => format!("{} (id {user_id})", me.full_name()),
    };
    match name {
        "" => println!("Logged in the first account as {who}."),
        _ => println!(
            "Logged in account {name:?} as {who}. The running bot starts it within a minute."
        ),
    }
    Ok(())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
