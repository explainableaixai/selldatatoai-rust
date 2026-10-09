//! Blocking Rust client for the Data Asset Score API at
//! <https://www.selldatatoai.com/api/>.
//!
//! ```no_run
//! let client = selldatatoai::Client::new("YOUR_KEY");
//! let s = client.score("example.com")?;
//! println!("{} {} {:?}", s.data_asset_score, s.grade, s.status);
//! # Ok::<(), selldatatoai::Error>(())
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::thread::sleep;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_BASE: &str = "https://www.selldatatoai.com/api/v1";
pub const BATCH_MAX: usize = 100;

/// Errors returned by the client.
#[derive(Debug)]
pub enum Error {
    /// The API answered with a non-2xx status. `code` is the API error string, such as `invalid_domain`.
    Api { status: u16, code: Option<String>, message: String },
    /// Network or decoding failure.
    Transport(String),
    /// A batch was still processing when the wait limit ran out.
    BatchTimeout(String),
    /// Bad input caught before any request was sent.
    Input(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api { status, code, message } => write!(f, "API error {} ({}): {}", status, code.as_deref().unwrap_or("-"), message),
            Error::Transport(m) => write!(f, "transport error: {}", m),
            Error::BatchTimeout(id) => write!(f, "batch {} still processing", id),
            Error::Input(m) => write!(f, "invalid input: {}", m),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct History {
    pub first_seen_year: Option<i32>,
    pub years_online: Option<i32>,
    pub founded_year: Option<i32>,
    pub pre_ai_years: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DataSystem {
    pub system: String,
    pub data_type: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DataAsset {
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
}

/// One scored company.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Score {
    pub domain: String,
    pub data_asset_score: u8,
    pub grade: String,
    pub status: String,
    #[serde(default)]
    pub verified_active: bool,
    pub iab_category: Option<String>,
    pub country: Option<String>,
    #[serde(default)]
    pub history: History,
    #[serde(default)]
    pub pre_ai_archive_likely: bool,
    #[serde(default)]
    pub data_systems: Vec<DataSystem>,
    #[serde(default)]
    pub likely_data_assets: Vec<DataAsset>,
    #[serde(default)]
    pub strengths: Vec<String>,
    pub data_coverage: Option<String>,
    #[serde(default)]
    pub in_index: bool,
    pub checked_at: Option<String>,
    #[serde(default)]
    pub cached: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchItem {
    pub input: String,
    pub domain: Option<String>,
    /// `done`, `pending`, `invalid_domain` or `error`.
    pub status: String,
    pub result: Option<Score>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Batch {
    pub id: String,
    /// `processing` or `done`.
    pub status: String,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub pending: usize,
    #[serde(default)]
    pub results: Vec<BatchItem>,
    pub lookups_charged: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Usage {
    pub plan: String,
    pub month: String,
    pub used: u64,
    pub limit: u64,
    pub remaining: u64,
}

#[derive(Deserialize)]
struct ApiErr {
    error: Option<String>,
    message: Option<String>,
}

/// API client. Cheap to clone; reuses one connection pool.
#[derive(Clone)]
pub struct Client {
    key: String,
    base: String,
    agent: ureq::Agent,
    /// Pause between polls in [`Client::wait_for_batch`].
    pub poll_interval: Duration,
    /// Longest wait for one batch.
    pub max_wait: Duration,
}

impl Client {
    pub fn new(api_key: &str) -> Self {
        Self::with_base(api_key, DEFAULT_BASE)
    }

    pub fn with_base(api_key: &str, base: &str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .user_agent(&format!("selldatatoai-rust/{} (+https://www.selldatatoai.com)", VERSION))
            .build();
        Client {
            key: api_key.to_string(),
            base: base.trim_end_matches('/').to_string(),
            agent,
            poll_interval: Duration::from_secs(5),
            max_wait: Duration::from_secs(300),
        }
    }

    fn handle<T: serde::de::DeserializeOwned>(r: Result<ureq::Response, ureq::Error>) -> Result<T, Error> {
        match r {
            Ok(resp) => resp.into_json::<T>().map_err(|e| Error::Transport(e.to_string())),
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                let parsed: Option<ApiErr> = serde_json::from_str(&body).ok();
                let (code, message) = match parsed {
                    Some(p) => (p.error, p.message.unwrap_or_else(|| format!("HTTP {}", status))),
                    None => (None, format!("HTTP {}", status)),
                };
                Err(Error::Api { status, code, message })
            }
            Err(e) => Err(Error::Transport(e.to_string())),
        }
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<ureq::Response, ureq::Error> {
        let mut req = self.agent.get(&format!("{}{}", self.base, path)).set("X-API-Key", &self.key);
        for (k, v) in query {
            req = req.query(k, v);
        }
        req.call()
    }

    /// Score one company domain. Full URLs are accepted.
    pub fn score(&self, domain: &str) -> Result<Score, Error> {
        if domain.trim().is_empty() {
            return Err(Error::Input("domain is empty".into()));
        }
        Self::handle(self.get("/score", &[("domain", domain.trim())]))
    }

    /// Submit 1 to 100 domains. One lookup per valid, unique domain.
    pub fn submit_batch<S: AsRef<str>>(&self, domains: &[S]) -> Result<Batch, Error> {
        if domains.is_empty() || domains.len() > BATCH_MAX {
            return Err(Error::Input(format!("a batch takes 1 to {} domains", BATCH_MAX)));
        }
        let list: Vec<&str> = domains.iter().map(|d| d.as_ref()).collect();
        let r = self
            .agent
            .post(&format!("{}/score/batch", self.base))
            .set("X-API-Key", &self.key)
            .send_json(serde_json::json!({ "domains": list }));
        Self::handle(r)
    }

    /// Poll a batch. Free.
    pub fn get_batch(&self, id: &str) -> Result<Batch, Error> {
        Self::handle(self.get("/score/batch", &[("id", id)]))
    }

    /// Poll until the batch is done or `max_wait` passes.
    pub fn wait_for_batch(&self, id: &str) -> Result<Batch, Error> {
        let start = Instant::now();
        loop {
            let b = self.get_batch(id)?;
            if b.status == "done" {
                return Ok(b);
            }
            if start.elapsed() > self.max_wait {
                return Err(Error::BatchTimeout(id.to_string()));
            }
            sleep(self.poll_interval);
        }
    }

    /// Score any number of domains in batches of 100, results in input order.
    pub fn score_many<S: AsRef<str>>(&self, domains: &[S]) -> Result<Vec<BatchItem>, Error> {
        let mut out = Vec::with_capacity(domains.len());
        for chunk in domains.chunks(BATCH_MAX) {
            let sent = self.submit_batch(chunk)?;
            let done = if sent.status == "done" { sent } else { self.wait_for_batch(&sent.id)? };
            out.extend(done.results);
        }
        Ok(out)
    }

    /// Plan, lookups used this month and lookups remaining.
    pub fn usage(&self) -> Result<Usage, Error> {
        Self::handle(self.get("/usage", &[]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_example() {
        let j = r#"{"domain":"example-labs.com","data_asset_score":82,"grade":"A","status":"active","verified_active":true,
        "iab_category":"Business and Finance > Industries > Biotech and Biomedical Industry","country":"United States",
        "history":{"first_seen_year":1998,"years_online":28,"founded_year":1995,"pre_ai_years":25},"pre_ai_archive_likely":true,
        "data_systems":[{"system":"Salesforce","data_type":"CRM records","type":"crm"}],
        "likely_data_assets":[{"type":"support_tickets","label":"Support tickets and chat transcripts"}],
        "strengths":["History"],"data_coverage":"full","in_index":true,"index_size":102000000,"checked_at":"2026-10-08T21:30:00+00:00","cached":false}"#;
        let s: Score = serde_json::from_str(j).unwrap();
        assert_eq!(s.data_asset_score, 82);
        assert_eq!(s.history.pre_ai_years, Some(25));
        assert_eq!(s.data_systems[0].kind, "crm");
    }

    #[test]
    fn rejects_oversized_batch() {
        let c = Client::new("k");
        let many: Vec<String> = (0..101).map(|i| format!("d{}.com", i)).collect();
        assert!(matches!(c.submit_batch(&many), Err(Error::Input(_))));
    }
}
