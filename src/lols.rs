//! Whether the senders of new messages are on the lols.bot spam banlist
//! (<https://lols.bot>), kept in `lols_status` (migration 054).
//!
//! Every logged message hands its sender to [`Lols::on_message`], which never
//! waits. A sender not checked yet is checked on a task of its own:
//!
//! 1. Against the hourly dump (`banlist.txt`, one id per line), held in memory
//!    as a sorted `Vec` and downloaded again whenever its ETag changes. A
//!    sender in it is recorded as banned without a request.
//! 2. Otherwise through `api.lols.bot/account`, which also catches a ban newer
//!    than the dump.
//!
//! A banned user is not checked again. A clean one is matched against every
//! new dump, and asked about through the API again after [`RECHECK_AFTER`],
//! the next time they write.
//!
//! One checker serves every account: it is about users, not about which
//! account saw them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Result, bail};
use clickhouse::Row;
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::db::ch::ClickhouseDb;
use crate::db::now;

const DUMP_URL: &str = "https://lols.bot/spam/banlist.txt";
const API_URL: &str = "https://api.lols.bot/account";
/// The dump is rebuilt hourly.
const DUMP_EVERY: Duration = Duration::from_secs(3600);
/// How long a clean answer stands before the user is asked about again.
const RECHECK_AFTER: u32 = 7 * 24 * 3600;
/// Pause between API requests, which go out one at a time: a busy chat must
/// not turn into a burst against a free service.
const API_GAP: Duration = Duration::from_millis(200);
/// What `seen` records for a banned user: never due again.
const BANNED: u32 = u32::MAX;
/// `seen` starts over past this, the way `WrittenNames` does.
const SEEN_LIMIT: usize = 1_000_000;

/// The latest check of a user, as `lols_status` holds it.
#[derive(Row, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Known {
    pub user_id: i64,
    pub banned: bool,
    pub checked_at: u32,
}

#[derive(Row, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct LolsStatus {
    pub user_id: i64,
    pub banned: bool,
    pub source: String,
    pub banned_at: Option<u32>,
    pub offenses: Option<u32>,
    pub spam_factor: Option<f32>,
    pub scammer: bool,
    pub checked_at: u32,
}

impl LolsStatus {
    fn from_dump(user_id: i64, at: u32) -> Self {
        LolsStatus {
            user_id,
            banned: true,
            source: "dump".into(),
            banned_at: None,
            offenses: None,
            spam_factor: None,
            scammer: false,
            checked_at: at,
        }
    }
}

/// `api.lols.bot/account`'s answer. Everything past `banned` may be absent.
#[derive(Deserialize, Debug)]
struct Account {
    ok: bool,
    banned: bool,
    when: Option<String>,
    offenses: Option<u32>,
    spam_factor: Option<f32>,
    #[serde(default)]
    scammer: bool,
}

impl Account {
    fn into_status(self, user_id: i64, at: u32) -> LolsStatus {
        LolsStatus {
            user_id,
            banned: self.banned,
            source: "api".into(),
            // "2024-06-28 17:18:05 UTC"
            banned_at: self.when.as_deref().and_then(|w| {
                chrono::NaiveDateTime::parse_from_str(w, "%Y-%m-%d %H:%M:%S UTC")
                    .ok()
                    .map(|t| t.and_utc().timestamp() as u32)
            }),
            offenses: self.offenses,
            spam_factor: self.spam_factor,
            scammer: self.scammer,
            checked_at: at,
        }
    }
}

/// The banlist dump: user ids, sorted for binary search. Channel ids
/// (`-100…`) in it are kept; they never match a user.
#[derive(Default)]
struct Dump {
    ids: Vec<i64>,
    etag: Option<String>,
}

impl Dump {
    fn contains(&self, id: i64) -> bool {
        self.ids.binary_search(&id).is_ok()
    }
}

fn parse_dump(text: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

pub struct Lols {
    /// `None` in tests, which turns the checker off.
    db: Option<Arc<ClickhouseDb>>,
    http: reqwest::Client,
    dump: RwLock<Dump>,
    /// When each user was last found clean, [`BANNED`] once found banned. A
    /// user is put here before their check starts, so a burst of messages
    /// from one sender is one check.
    seen: Mutex<HashMap<i64, u32>>,
    /// One API request at a time.
    api: tokio::sync::Mutex<()>,
}

/// Whether a user is due a check, by what `seen` holds for them.
fn due(seen: Option<u32>, at: u32) -> bool {
    match seen {
        None => true,
        Some(BANNED) => false,
        Some(checked_at) => at.saturating_sub(checked_at) >= RECHECK_AFTER,
    }
}

impl Lols {
    /// A checker that does nothing: what an `App` starts with, and what tests
    /// run on.
    pub fn disabled() -> Self {
        Self::new(None)
    }

    fn new(db: Option<Arc<ClickhouseDb>>) -> Self {
        Lols {
            db,
            http: reqwest::Client::builder()
                .user_agent("rust_telegram_user_bot")
                .timeout(Duration::from_secs(120))
                .build()
                .expect("http client"),
            dump: RwLock::default(),
            seen: Mutex::default(),
            api: tokio::sync::Mutex::new(()),
        }
    }

    /// The checker, with what `lols_status` already knows loaded, and the dump
    /// kept fresh in the background from now on.
    pub async fn start(db: Arc<ClickhouseDb>) -> Arc<Self> {
        let lols = Arc::new(Self::new(Some(Arc::clone(&db))));
        match db.lols_known().await {
            Ok(known) => {
                let mut seen = lols.seen.lock().unwrap();
                for k in known {
                    seen.insert(k.user_id, if k.banned { BANNED } else { k.checked_at });
                }
            }
            // Not fatal: the users are checked again as they write.
            Err(e) => warn!("lols: reading lols_status: {e:#}"),
        }
        let refresher = Arc::clone(&lols);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(DUMP_EVERY);
            loop {
                interval.tick().await;
                match refresher.refresh_dump().await {
                    Ok(true) => refresher.match_clean_against_dump().await,
                    Ok(false) => {}
                    Err(e) => {
                        warn!("lols: banlist download failed, keeping the previous one: {e:#}")
                    }
                }
            }
        });
        lols
    }

    /// A message from `user_id` was logged. 0 is a channel or an anonymous
    /// admin, not a user.
    pub fn on_message(self: &Arc<Self>, user_id: u64) {
        let Ok(user_id) = i64::try_from(user_id) else {
            return;
        };
        if self.db.is_none() || user_id <= 0 {
            return;
        }
        let at = now();
        {
            let mut seen = self.seen.lock().unwrap();
            if !due(seen.get(&user_id).copied(), at) {
                return;
            }
            if seen.len() >= SEEN_LIMIT {
                seen.clear();
            }
            seen.insert(user_id, at);
        }
        let lols = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = lols.check_user(user_id).await {
                warn!("lols: checking {user_id}: {e:#}");
                // Due again on their next message.
                lols.seen.lock().unwrap().remove(&user_id);
            }
        });
    }

    async fn check_user(&self, user_id: i64) -> Result<()> {
        let in_dump = self.dump.read().unwrap().contains(user_id);
        let status = if in_dump {
            LolsStatus::from_dump(user_id, now())
        } else {
            let _one = self.api.lock().await;
            let status = check(&self.http, user_id).await;
            tokio::time::sleep(API_GAP).await;
            status?
        };
        self.record(&[status]).await
    }

    async fn record(&self, rows: &[LolsStatus]) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        db.write_lols_status(rows).await?;
        let mut seen = self.seen.lock().unwrap();
        for row in rows {
            seen.insert(
                row.user_id,
                if row.banned { BANNED } else { row.checked_at },
            );
        }
        for row in rows.iter().filter(|r| r.banned) {
            info!(
                "lols: {} is on the spam banlist ({})",
                row.user_id, row.source
            );
        }
        Ok(())
    }

    /// Downloads the dump when it has changed. Whether it had.
    async fn refresh_dump(&self) -> Result<bool> {
        let mut req = self.http.get(DUMP_URL);
        if let Some(etag) = self.dump.read().unwrap().etag.clone() {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(false);
        }
        let resp = resp.error_for_status()?;
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let ids = parse_dump(&resp.text().await?);
        if ids.is_empty() {
            bail!("banlist is empty");
        }
        info!("lols: banlist loaded, {} ids", ids.len());
        *self.dump.write().unwrap() = Dump { ids, etag };
        Ok(true)
    }

    /// Users found clean before who are on the new dump.
    async fn match_clean_against_dump(&self) {
        let at = now();
        let rows = newly_banned(&self.seen.lock().unwrap(), &self.dump.read().unwrap(), at);
        if let Err(e) = self.record(&rows).await {
            warn!("lols: recording {} bans from the dump: {e:#}", rows.len());
        }
    }
}

fn newly_banned(seen: &HashMap<i64, u32>, dump: &Dump, at: u32) -> Vec<LolsStatus> {
    seen.iter()
        .filter(|&(&id, &checked)| checked != BANNED && dump.contains(id))
        .map(|(&id, _)| LolsStatus::from_dump(id, at))
        .collect()
}

async fn check(http: &reqwest::Client, user_id: i64) -> Result<LolsStatus> {
    let account: Account = http
        .get(API_URL)
        .query(&[("id", user_id)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if !account.ok {
        bail!("api answered ok=false");
    }
    Ok(account.into_status(user_id, now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump(ids: &[i64]) -> Dump {
        Dump {
            ids: parse_dump(&ids.iter().map(|i| format!("{i}\n")).collect::<String>()),
            etag: None,
        }
    }

    #[test]
    fn the_dump_parses_and_skips_junk() {
        assert_eq!(parse_dump("5\n-1001\n\n x\n3\n5\n"), vec![-1001, 3, 5]);
        assert!(dump(&[3]).contains(3));
        assert!(!dump(&[3]).contains(4));
    }

    #[test]
    fn who_is_due_a_check() {
        let at = 10_000_000;
        assert!(due(None, at));
        assert!(!due(Some(BANNED), at));
        assert!(!due(Some(at - 60), at));
        assert!(due(Some(at - RECHECK_AFTER), at));
    }

    #[test]
    fn a_new_dump_bans_only_users_found_clean() {
        let seen = HashMap::from([(1, 5), (2, BANNED), (3, 5)]);
        let rows = newly_banned(&seen, &dump(&[1, 2]), 9);
        assert_eq!(rows, vec![LolsStatus::from_dump(1, 9)]);
    }

    #[tokio::test]
    async fn a_disabled_checker_ignores_messages() {
        let lols = Arc::new(Lols::disabled());
        lols.on_message(42);
        assert!(lols.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_api_answer_maps_onto_a_row() {
        let a: Account = serde_json::from_str(
            r#"{"ok":true,"user_id":7,"banned":true,"when":"2024-06-28 17:18:05 UTC","offenses":3,"spam_factor":55.5,"scammer":true}"#,
        )
        .unwrap();
        let s = a.into_status(7, 1);
        assert!(s.banned && s.scammer);
        assert_eq!(s.banned_at, Some(1_719_595_085));
        assert_eq!(s.offenses, Some(3));

        let clean: Account =
            serde_json::from_str(r#"{"ok":true,"user_id":7,"banned":false}"#).unwrap();
        let s = clean.into_status(7, 1);
        assert!(!s.banned && !s.scammer && s.banned_at.is_none());
    }
}
