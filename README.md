# selldatatoai (Rust)

A blocking Rust client for the Data Asset Score API published by [Sell Data to AI (selldatatoai.com)](https://www.selldatatoai.com/). It turns a company domain into a typed `Score`: a value from 0 to 100, a grade from A to E, the company status, its web history and the systems of record it runs.

```toml
[dependencies]
selldatatoai = "1"
```

The crate depends on `ureq`, `serde` and `serde_json` only. No async runtime is pulled in, so it fits CLI tools, batch jobs and services that already run their own thread pools.

## At a glance

| | |
|---|---|
| Index behind the API | 102 million domains, 99.99% of the active internet, with history |
| Single lookup | `client.score("example.com")` |
| Batch | up to 100 domains per call, results in input order |
| Auth | `X-API-Key` header, set by the client |
| Errors | one `Error` enum: `Api`, `Transport`, `BatchTimeout`, `Input` |
| MSRV | Rust 1.71 |
| License | MIT |

## First request

```rust
use selldatatoai::Client;

fn main() -> Result<(), selldatatoai::Error> {
    let key = std::env::var("SDA_API_KEY").expect("set SDA_API_KEY");
    let client = Client::new(&key);

    let s = client.score("example.com")?;
    println!("{} scored {} ({})", s.domain, s.data_asset_score, s.grade);
    println!("status: {}, verified active: {}", s.status, s.verified_active);
    if let Some(y) = s.history.years_online {
        println!("online for {} years", y);
    }
    for sys in &s.data_systems {
        println!("runs {} -> {}", sys.system, sys.data_type);
    }
    Ok(())
}
```

`score` accepts full URLs too. `https://www.example.com/pricing` and `example.com` resolve to the same company.

## Types

```rust
pub struct Score {
    pub domain: String,
    pub data_asset_score: u8,        // 0..=100
    pub grade: String,               // "A".."E"
    pub status: String,              // active | winding_down_or_acquired | parked | unreachable | error
    pub verified_active: bool,
    pub iab_category: Option<String>,
    pub country: Option<String>,
    pub history: History,            // first_seen_year, years_online, founded_year, pre_ai_years
    pub pre_ai_archive_likely: bool,
    pub data_systems: Vec<DataSystem>,      // system, data_type, kind
    pub likely_data_assets: Vec<DataAsset>, // kind, label
    pub strengths: Vec<String>,
    pub data_coverage: Option<String>,      // full | partial | limited
    pub in_index: bool,
    pub checked_at: Option<String>,
    pub cached: bool,
}
```

The JSON key `type` maps to the Rust field `kind`, because `type` is a reserved word. Every struct derives `Serialize` and `Deserialize`, so you can write results straight back out as JSON or into a database row.

`Batch`, `BatchItem` and `Usage` mirror the API answers one to one. See the [batch scoring endpoint](https://www.selldatatoai.com/api/#batch) section of the docs for the wire format.

## Methods

```rust
impl Client {
    pub fn new(api_key: &str) -> Self;
    pub fn with_base(api_key: &str, base: &str) -> Self;

    pub fn score(&self, domain: &str) -> Result<Score, Error>;
    pub fn submit_batch<S: AsRef<str>>(&self, domains: &[S]) -> Result<Batch, Error>;
    pub fn get_batch(&self, id: &str) -> Result<Batch, Error>;
    pub fn wait_for_batch(&self, id: &str) -> Result<Batch, Error>;
    pub fn score_many<S: AsRef<str>>(&self, domains: &[S]) -> Result<Vec<BatchItem>, Error>;
    pub fn usage(&self) -> Result<Usage, Error>;
}
```

Two public fields tune polling: `poll_interval` (default 5 seconds) and `max_wait` (default 5 minutes per batch). Each request has a 60 second timeout.

`Client` is `Clone` and shares one connection pool between clones, so you can hand a copy to each worker thread.

## Error handling

```rust
use selldatatoai::{Client, Error};

match client.score(raw) {
    Ok(s) => println!("{}", s.data_asset_score),
    Err(Error::Api { status: 400, .. }) => eprintln!("skipping invalid domain {}", raw),
    Err(Error::Api { status: 429, code, .. }) if code.as_deref() == Some("monthly_limit_reached") => {
        eprintln!("lookups used up for this month");
        std::process::exit(2);
    }
    Err(Error::Api { status: 401, .. }) => panic!("check SDA_API_KEY and the plan"),
    Err(e) => return Err(e.into()),
}
```

The `code` field carries the API error string:

| HTTP | code | Cause |
|---|---|---|
| 400 | `invalid_domain` | Not a valid domain |
| 400 | `no_domains` / `too_many_domains` | Batch outside 1 to 100 |
| 401 | `missing_api_key` / `invalid_api_key` | No key, or plan inactive |
| 404 | `batch_not_found` | Unknown id or older than 7 days |
| 410 | `not_available` | Lists are not served by the API |
| 429 | `monthly_limit_reached` | Monthly lookups used |
| 429 | `batch_busy` | Three batches already open |

`Error::Input` is returned before any request is made, for example when a batch slice is empty or longer than 100. It never costs a lookup.

## Examples

### A CLI that ranks a file of domains

```rust
// cargo run --release -- domains.txt
use std::{env, fs};
use selldatatoai::Client;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args().nth(1).ok_or("usage: rank <file>")?;
    let domains: Vec<String> = fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();

    let client = Client::new(&env::var("SDA_API_KEY")?);
    let left = client.usage()?.remaining;
    if (domains.len() as u64) > left {
        return Err(format!("{} domains but only {} lookups left", domains.len(), left).into());
    }

    let mut rows: Vec<_> = client
        .score_many(&domains)?
        .into_iter()
        .filter_map(|it| it.result)
        .filter(|s| s.verified_active)
        .collect();
    rows.sort_by(|a, b| b.data_asset_score.cmp(&a.data_asset_score));

    for s in rows.iter().take(50) {
        let systems: Vec<&str> = s.data_systems.iter().map(|d| d.system.as_str()).collect();
        println!("{:<32} {:>3} {}  {}", s.domain, s.data_asset_score, s.grade, systems.join(", "));
    }
    Ok(())
}
```

### Worker threads over a queue

Single lookups in parallel, for streams where domains arrive one by one:

```rust
use std::sync::mpsc;
use std::thread;
use selldatatoai::Client;

let client = Client::new(&key);
let (tx, rx) = mpsc::channel::<String>();
let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));

let workers: Vec<_> = (0..4).map(|_| {
    let c = client.clone();
    let rx = rx.clone();
    thread::spawn(move || loop {
        let next = rx.lock().unwrap().recv();
        let Ok(domain) = next else { break };
        match c.score(&domain) {
            Ok(s) => println!("{} {}", domain, s.data_asset_score),
            Err(e) => eprintln!("{} {}", domain, e),
        }
    })
}).collect();

for d in ["example-one.com", "example-two.com"] { tx.send(d.into()).unwrap(); }
drop(tx);
for w in workers { w.join().unwrap(); }
```

### Inside an async service

The client blocks. In a Tokio service, move each call onto the blocking pool:

```rust
let c = client.clone();
let score = tokio::task::spawn_blocking(move || c.score("example.com")).await??;
```

## Saving results

Because every type derives `Serialize`, persisting a run is short. This writes JSON Lines, one company per line, which most warehouses load directly:

```rust
use std::io::Write;

let mut out = std::io::BufWriter::new(std::fs::File::create("scores.jsonl")?);
for item in client.score_many(&domains)? {
    if let Some(score) = item.result {
        serde_json::to_writer(&mut out, &score)?;
        out.write_all(b"\n")?;
    }
}
```

For a CSV, pick the flat fields you need: domain, score, grade, status, `history.years_online`, and the joined system names. Nested lists such as `data_systems` do not map cleanly to columns, so join them with a separator.

## Testing your integration

Point the client at a local stub with `Client::with_base("test-key", "http://127.0.0.1:8080/v1")`. Any HTTP server that returns the documented JSON shapes works, which keeps your test suite offline and free of lookups.

Three cases are worth a test each:

- A normal score, to check your field mapping.
- A 400 answer, to check that bad rows are skipped rather than fatal.
- A batch that answers `processing` once and `done` on the next poll, to check your wait logic with a short `poll_interval`.

The crate's own tests parse the documented example response and check that oversized batches fail before any request goes out.

## What the score is for

Firms that broker company data to AI labs face a sourcing problem. Thousands of companies could supply support tickets, engineering history or recorded calls, but only some hold enough of it, and only some are still in business. The guide [what is an AI data broker](https://www.selldatatoai.com/what-is-an-ai-data-broker/) lays out where that role sits between sellers and labs.

A score per domain is the first filter. It costs a fraction of a cent to a couple of cents and saves the hour a researcher would spend on each website.

It also anchors a conversation about money. Buyers price data after they see a manifest and samples, but sellers want a rough sense first, and the guide on [how much do AI companies pay for data](https://www.selldatatoai.com/how-much-do-ai-companies-pay-for-data/) collects the price ranges buyers publish.

Teams that already use [firmographic data providers](https://www.selldatatoai.com/firmographic-data-providers/) for revenue and headcount add this score as a separate column. It answers a different question: not "how big is this company" but "how much data AI buyers want does it hold".

Some buyers search by data type rather than sector. For engineering records, there is a ready file of [companies with code history](https://www.selldatatoai.com/lists/data-type/code-with-history/).

## Plans and limits

- No free plan and no trial. The website demo allows 5 checks a day.
- Basic: $99 a month, 5,000 lookups.
- Pro: $299 a month, 25,000 lookups, batch scoring.
- Scale: $799 a month, 100,000 lookups, batch scoring.
- Limits reset on the first day of each month, UTC.
- Results are reused for 30 days; `cached` tells you when.

Compare the tiers on the [subscription plans](https://www.selldatatoai.com/pricing/) page. Company lists are sold separately as one-time files and never through the API.

## FAQ

### What is the selldatatoai crate?

The selldatatoai crate is a Rust client for the Data Asset Score API at selldatatoai.com. It scores company domains from 0 to 100 for the data AI buyers want and returns typed results with grade, status, data systems and history.

### Why blocking and not async?

Most users call it from batch jobs and CLIs, where a runtime is overhead. The batch endpoint already does the parallel work on the server. Use `spawn_blocking` inside async code.

### Does `score_many` respect the open batch limit?

Yes. It submits one batch, waits for it, then sends the next. You never hold more than one open batch from a single call.

### What happens to duplicates in a batch?

They are charged once. Each input row still gets its own item in the answer, in the order you sent.

### Can the score change?

Yes. Each domain is scored again after 30 days, and a company that adds systems, goes quiet or gets acquired will move.

## Links

- Website: https://www.selldatatoai.com/
- API reference: https://www.selldatatoai.com/api/
- Repository: https://github.com/explainableaixai/selldatatoai-rust
- The Rust Programming Language book: [rust-lang.org](https://doc.rust-lang.org/book/)
- Crate documentation: [docs.rs](https://docs.rs/selldatatoai)

## License

MIT. Copyright (c) 2026 Alpha Quantum, info@alpha-quantum.com
