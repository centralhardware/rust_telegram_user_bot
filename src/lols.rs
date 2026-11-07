//! Whether the sender of an incoming message is on the lols.bot spam banlist
//! (<https://lols.bot>). Asked before the message is saved; every field of the
//! answer goes into the row's `lols_*` columns (migration 054).
//!
//! [`Lols::check`] answers from, in order:
//!
//! 1. The hourly dump (`banlist.txt`, one id per line), held in memory as a
//!    sorted `Vec` and downloaded again whenever its ETag changes.
//! 2. The cache of earlier API answers: a spammer for the life of the process,
//!    a clean user for [`CLEAN_FOR`].
//! 3. `api.lols.bot/account`, given [`API_TIMEOUT`] at most, since the save is
//!    waiting. A failure or a timeout answers "not banned" and is not cached.
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
/// How long a save may wait on the API.
const API_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a clean API answer is trusted. A spammer is cached for good.
const CLEAN_FOR: u32 = 24 * 3600;
/// The cache starts over past this, the way `WrittenNames` does.
const CACHE_LIMIT: usize = 1_000_000;

/// `api.lols.bot/account`'s answer. Everything past `banned` may be absent.
#[derive(Deserialize, Debug)]
struct Account {
    ok: bool,
    banned: bool,
    offenses: Option<u32>,
    spam_factor: Option<f32>,
    #[serde(default)]
    scammer: bool,
    scamrsalert: Option<u32>,
}

/// What lols.bot said about a user, as the row stores it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Verdict {
    pub banned: bool,
    pub offenses: Option<u32>,
    pub spam_factor: Option<f32>,
    pub scammer: bool,
    pub scamrsalert: Option<u32>,
}

impl Verdict {
    fn from_dump() -> Self {
        Verdict {
            banned: true,
            ..Default::default()
        }
    }
}

impl From<Account> for Verdict {
    fn from(a: Account) -> Self {
        Verdict {
            banned: a.banned,
            offenses: a.offenses,
            spam_factor: a.spam_factor,
            scammer: a.scammer,
            scamrsalert: a.scamrsalert,
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

/// A cached API answer and when it was given.
#[derive(Clone, Debug, PartialEq)]
struct Cached {
    verdict: Verdict,
    at: u32,
}

/// What the cache says about a user, `None` when it has to be asked. A ban
/// stands for good, a clean answer for [`CLEAN_FOR`].
fn cached_answer(entry: Option<&Cached>, at: u32) -> Option<Verdict> {
    let entry = entry?;
    (entry.verdict.banned || at.saturating_sub(entry.at) < CLEAN_FOR).then(|| entry.verdict.clone())
}

/// One HTTP request to lols.bot, for `lols_requests` (migration 054). A log of
/// its own, kept apart from the message tables: it says how much the bot leans
/// on lols.bot, not anything about a message.
#[derive(Row, Serialize, Debug, Clone)]
pub struct LolsRequest {
    pub ts: u32,
    /// `account` (api.lols.bot/account) or `banlist` (the dump).
    pub endpoint: &'static str,
    /// The user asked about; 0 for the dump.
    pub user_id: i64,
    /// HTTP status, 0 when no response came (timeout, connection error).
    pub status: u16,
    pub success: bool,
    pub error: String,
    pub duration_ms: u32,
    /// The API's `banned`, `None` for the dump or a failed request.
    pub banned: Option<bool>,
    /// Ids in a downloaded dump, 0 otherwise (and for a 304).
    pub ids: u32,
}

pub struct Lols {
    /// Off in tests: every answer is "not spam" and nothing goes out.
    enabled: bool,
    /// Where `lols_requests` goes. `None` in tests.
    db: Option<Arc<ClickhouseDb>>,
    http: reqwest::Client,
    dump: RwLock<Dump>,
    cache: Mutex<HashMap<i64, Cached>>,
}

impl Lols {
    /// A checker that does nothing: what an `App` starts with, and what tests
    /// run on.
    pub fn disabled() -> Self {
        Self::new(None)
    }

    fn new(db: Option<Arc<ClickhouseDb>>) -> Self {
        Lols {
            enabled: db.is_some(),
            db,
            http: reqwest::Client::builder()
                .user_agent("rust_telegram_user_bot")
                .timeout(Duration::from_secs(120))
                .build()
                .expect("http client"),
            dump: RwLock::default(),
            cache: Mutex::default(),
        }
    }

    /// The checker, with the dump kept fresh in the background from now on.
    pub fn start(db: Arc<ClickhouseDb>) -> Arc<Self> {
        let lols = Arc::new(Self::new(Some(db)));
        let refresher = Arc::clone(&lols);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(DUMP_EVERY);
            loop {
                interval.tick().await;
                if let Err(e) = refresher.refresh_dump().await {
                    warn!("lols: banlist download failed, keeping the previous one: {e:#}");
                }
            }
        });
        lols
    }

    /// What lols.bot says about `user_id`. 0 is no user (a channel post, an
    /// anonymous admin) and is never checked.
    pub async fn check(&self, user_id: u64) -> Verdict {
        let Ok(user_id) = i64::try_from(user_id) else {
            return Verdict::default();
        };
        if !self.enabled || user_id <= 0 {
            return Verdict::default();
        }
        if self.dump.read().unwrap().contains(user_id) {
            return Verdict::from_dump();
        }
        let at = now();
        if let Some(v) = cached_answer(self.cache.lock().unwrap().get(&user_id), at) {
            return v;
        }
        let started = std::time::Instant::now();
        let (result, status) = ask(&self.http, user_id).await;
        self.log(LolsRequest {
            ts: at,
            endpoint: "account",
            user_id,
            status,
            success: result.is_ok(),
            error: result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_default(),
            duration_ms: started.elapsed().as_millis() as u32,
            banned: result.as_ref().ok().map(|v| v.banned),
            ids: 0,
        });
        match result {
            Ok(verdict) => {
                if verdict.banned {
                    info!("lols: {user_id} is on the spam banlist (api)");
                }
                let mut cache = self.cache.lock().unwrap();
                if cache.len() >= CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(
                    user_id,
                    Cached {
                        verdict: verdict.clone(),
                        at,
                    },
                );
                verdict
            }
            Err(e) => {
                warn!("lols: API check of {user_id}: {e:#}");
                Verdict::default()
            }
        }
    }

    /// Writes a request to `lols_requests`, without waiting on it.
    fn log(&self, row: LolsRequest) {
        let Some(db) = self.db.clone() else { return };
        tokio::spawn(async move {
            if let Err(e) = db.write_lols_request(&row).await {
                warn!("lols: logging a request: {e:#}");
            }
        });
    }

    /// Downloads the dump when it has changed, and logs the request.
    async fn refresh_dump(&self) -> Result<()> {
        let started = std::time::Instant::now();
        let mut status = 0;
        let result = self.download_dump(&mut status).await;
        self.log(LolsRequest {
            ts: now(),
            endpoint: "banlist",
            user_id: 0,
            status,
            success: result.is_ok(),
            error: result
                .as_ref()
                .err()
                .map(|e| format!("{e:#}"))
                .unwrap_or_default(),
            duration_ms: started.elapsed().as_millis() as u32,
            banned: None,
            ids: *result.as_ref().unwrap_or(&0) as u32,
        });
        result.map(|_| ())
    }

    /// The dump download itself: how many ids it loaded, 0 for a 304.
    async fn download_dump(&self, status: &mut u16) -> Result<usize> {
        let mut req = self.http.get(DUMP_URL);
        if let Some(etag) = self.dump.read().unwrap().etag.clone() {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await?;
        *status = resp.status().as_u16();
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(0);
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
        let count = ids.len();
        info!("lols: banlist loaded, {count} ids");
        *self.dump.write().unwrap() = Dump { ids, etag };
        Ok(count)
    }
}

/// One API request, and the HTTP status it got (0 for none).
async fn ask(http: &reqwest::Client, user_id: i64) -> (Result<Verdict>, u16) {
    let resp = match http
        .get(API_URL)
        .query(&[("id", user_id)])
        .timeout(API_TIMEOUT)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => return (Err(e.into()), 0),
    };
    let status = resp.status().as_u16();
    let result = async {
        let account: Account = resp.error_for_status()?.json().await?;
        if !account.ok {
            bail!("api answered ok=false");
        }
        Ok(account.into())
    }
    .await;
    (result, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dump_parses_and_skips_junk() {
        let ids = parse_dump("5\n-1001\n\n x\n3\n5\n");
        assert_eq!(ids, vec![-1001, 3, 5]);
        let dump = Dump { ids, etag: None };
        assert!(dump.contains(3));
        assert!(!dump.contains(4));
    }

    #[test]
    fn what_the_cache_answers() {
        let at = 10_000_000;
        let banned = Verdict {
            banned: true,
            ..Default::default()
        };
        let clean = Verdict {
            ..Default::default()
        };
        let entry = |verdict: &Verdict, age| Cached {
            verdict: verdict.clone(),
            at: at - age,
        };
        assert_eq!(cached_answer(None, at), None);
        assert_eq!(
            cached_answer(Some(&entry(&banned, 10 * CLEAN_FOR)), at),
            Some(banned)
        );
        assert_eq!(
            cached_answer(Some(&entry(&clean, 60)), at),
            Some(clean.clone())
        );
        assert_eq!(cached_answer(Some(&entry(&clean, CLEAN_FOR)), at), None);
    }

    #[tokio::test]
    async fn a_disabled_checker_checks_nothing() {
        assert_eq!(Lols::disabled().check(42).await, Verdict::default());
    }

    #[test]
    fn every_api_field_reaches_the_verdict() {
        let a: Account = serde_json::from_str(
            r#"{"ok":true,"user_id":7,"banned":true,"when":"2024-06-28 17:18:05 UTC","offenses":3,"spam_factor":55.5,"scammer":true,"scamrsalert":1460}"#,
        )
        .unwrap();
        let v = Verdict::from(a);
        assert_eq!(
            v,
            Verdict {
                banned: true,
                offenses: Some(3),
                spam_factor: Some(55.5),
                scammer: true,
                scamrsalert: Some(1460),
            }
        );
        let clean: Account =
            serde_json::from_str(r#"{"ok":true,"user_id":7,"banned":false}"#).unwrap();
        assert_eq!(
            Verdict::from(clean),
            Verdict {
                ..Default::default()
            }
        );
    }
}
