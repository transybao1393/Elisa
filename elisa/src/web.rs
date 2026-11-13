//! The web tools (`web_search`, `web_fetch`), offered to the model only when the connection turns web access on
//! (`web = "duckduckgo" | "searxng" | "brave"`). Search goes to DuckDuckGo's plain HTML page (no key), to a SearXNG
//! server's JSON API, or to the Brave Search API (with the user's key); a fetch returns a page as plain text. They run
//! in the built-in web MCP server (`mcp_web.rs`), so each call asks like any MCP tool (approval key `mcp:web`). A fetch
//! still refuses addresses on this machine or the local network (a page cannot steer the model into local services,
//! also not through a redirect, and an "always" answer must not open them), and the host fences every result as
//! untrusted (`untrusted.rs`).
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;

/// which search engine, the SearXNG server (`searxng_url`) and the Brave Search API key (never printed: `Debug`
/// shows only whether it is set)
#[derive(Clone, Default, PartialEq)]
pub struct WebCfg { pub engine: String, pub searxng: String, pub brave_key: String }

impl std::fmt::Debug for WebCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebCfg").field("engine", &self.engine).field("searxng", &self.searxng)
            .field("brave_key", &if self.brave_key.is_empty() { "" } else { "[set]" }).finish()
    }
}

pub const DUCKDUCKGO: &str = "duckduckgo";
pub const SEARXNG: &str = "searxng";
pub const BRAVE: &str = "brave";

/// the environment variable that holds the Brave Search API key: read by config loading (it wins over config
/// `brave_api_key`) and by the web server process, which gets the key this way and never on its command line
pub const BRAVE_KEY_ENV: &str = "BRAVE_API_KEY";
/// tests only: another address for the Brave Search API, a local stand-in (`http://127.0.0.1…` / `http://localhost…`
/// only, like `ELISA_OSV_URL`; anything else is ignored, so the key never goes to another host)
pub const BRAVE_URL_ENV: &str = "ELISA_BRAVE_URL";
const BRAVE_URL: &str = "https://api.search.brave.com";

/// results given to the model, and the most page text it gets (about 2,400 tokens: an 8K window keeps room)
const HITS: usize = 5;
const PAGE_CHARS: usize = 8000;
const PAGE_BYTES: u64 = 3_000_000;

/// a plain browser User-Agent: DuckDuckGo's HTML page answers a bare client with a bot check
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

#[derive(Clone, Debug, PartialEq)]
pub struct Hit { pub title: String, pub url: String, pub snippet: String }

fn agent(timeout_s: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_global(Some(Duration::from_secs(timeout_s))).build().into()
}

/// the agent for `web_fetch`: its resolver ([`PublicOnly`]) refuses a host when any address it resolves to is on this
/// machine or the local network, and the connection then goes to exactly the addresses it checked (no second lookup
/// a rebinding DNS server could answer differently). No proxy: through a proxy the proxy resolves the host, out of
/// reach of the check.
fn fetch_agent(timeout_s: u64) -> ureq::Agent {
    let cfg = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).proxy(None)
        .timeout_global(Some(Duration::from_secs(timeout_s))).build();
    ureq::Agent::with_parts(cfg, ureq::unversioned::transport::DefaultConnector::default(), PublicOnly)
}

/// the end of the refusal message, also how a refusal from inside ureq (the resolver) is recognised
const NOT_PUBLIC: &str = "is on this machine or the local network: web_fetch only reads public pages";

/// a name resolver that only lets public addresses through (see [`fetch_agent`])
#[derive(Debug)]
struct PublicOnly;

impl ureq::unversioned::resolver::Resolver for PublicOnly {
    fn resolve(&self, uri: &ureq::http::Uri, config: &ureq::config::Config, timeout: ureq::unversioned::transport::NextTimeout)
        -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let addrs = ureq::unversioned::resolver::DefaultResolver::default().resolve(uri, config, timeout)?;
        if let Some(bad) = addrs.iter().find(|a| !is_public_ip(a.ip())) {
            let host = uri.host().unwrap_or("").trim_start_matches('[').trim_end_matches(']');
            return Err(ureq::Error::Io(std::io::Error::other(format!("{host} {NOT_PUBLIC} (it resolves to {})", bad.ip()))));
        }
        Ok(addrs)
    }
}

/// whether an address is on the public internet: not loopback, private, link-local, unique-local, unspecified,
/// CGNAT (100.64/10), 0/8, broadcast, multicast or reserved, also not wrapped in IPv6 (mapped, compatible, NAT64)
pub fn is_public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr};
    fn v4(a: Ipv4Addr) -> bool {
        let o = a.octets();
        !(a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified() || a.is_broadcast() || a.is_multicast()
            || o[0] == 0                                   // 0.0.0.0/8 ("this network", reaches this machine)
            || (o[0] == 100 && (64..128).contains(&o[1]))  // 100.64/10 carrier-grade NAT
            || o[0] >= 240                                 // 240/4 reserved
            || (o[0] == 198 && (o[1] & 0xfe) == 18)        // 198.18/15 benchmarking
            || (o[0] == 192 && o[1] == 0 && o[2] == 0))    // 192.0.0/24 protocol assignments
    }
    match ip {
        IpAddr::V4(a) => v4(a),
        IpAddr::V6(a) => {
            let s = a.segments();
            if let Some(m) = a.to_ipv4_mapped() { return v4(m); }
            if a.is_loopback() || a.is_unspecified() || a.is_multicast() { return false; }
            if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xffc0) == 0xfec0 { return false; }
            let tail = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
            // ::a.b.c.d (IPv4-compatible) and 64:ff9b::a.b.c.d (NAT64) carry an IPv4 address
            if s[..6].iter().all(|&x| x == 0) || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0)) { return v4(tail); }
            true
        }
    }
}

/// `web_search {query, recent}`: the top results as numbered lines. `recent` (the model's choice, for news): only pages
/// from the past week, each engine's own time filter (otherwise "latest news about Apple" finds only the news sites'
/// front pages, whose snippets say "the latest Apple news", so the reply can only list sites). A recent search with
/// no results falls back to a plain one.
pub fn search(query: &str, recent: bool, cfg: &WebCfg) -> Result<String, String> {
    let q = query.trim();
    if q.is_empty() { return Err("missing query".into()); }
    let server = match cfg.engine.as_str() {
        DUCKDUCKGO | BRAVE => String::new(),
        SEARXNG => cfg.searxng.trim_end_matches('/').to_string(),
        e => return Err(format!("web search is off (engine \"{e}\")")),
    };
    // Elisa's result cache (`cache.rs`, 15 min): per engine (and SearXNG server), time filter and query, never the API key
    let key = format!("{}\n{server}\n{}{}", cfg.engine, if recent { "recent\n" } else { "" }, q.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase());
    crate::cache::cached(crate::cache::Kind::WebSearch, &key, || {
        let run = |q: &str, week: bool| match cfg.engine.as_str() {
            DUCKDUCKGO => duckduckgo(q, week),
            SEARXNG => searxng(&cfg.searxng, q, week),
            _ => brave(&brave_base(), &cfg.brave_key, q, week),
        };
        // a news search names the current month and year (from the clock): the plain words find the news sites'
        // front pages, whose snippets can be years-old page text; dated words find this month's articles
        let dated = if recent { dated_query(q, crate::situation::now_secs()) } else { q.to_string() };
        let mut hits = run(&dated, recent)?;
        if recent && hits.is_empty() && dated != q { hits = run(q, true)?; }
        if recent && hits.is_empty() { hits = run(q, false)?; }
        Ok((format_hits(q, &hits), true))
    })
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// (year, month 1..=12) of a unix time, local
fn year_month(t: i64) -> (i32, u32) {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() { return (1970, 1); }
        (tm.tm_year + 1900, tm.tm_mon as u32 + 1)
    }
}

/// A news search's query with the current month and year added ("latest news about Apple November 2026"), unless
/// it already names a year or a month.
pub fn dated_query(q: &str, now: i64) -> String {
    let (y, m) = year_month(now);
    let has_year = q.split(|c: char| !c.is_ascii_digit()).any(|w| w.len() == 4 && (w.starts_with("19") || w.starts_with("20")));
    let low = q.to_lowercase();
    let has_month = MONTHS.iter().any(|n| low.split(|c: char| !c.is_alphabetic()).any(|w| w == n.to_lowercase()));
    if has_year || has_month { return q.to_string(); }
    format!("{q} {} {y}", MONTHS[(m as usize).clamp(1, 12) - 1])
}

/// The day a news search runs, for its note to the model ("2026-11-10")
pub fn today(now: i64) -> String {
    unsafe {
        let tt = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() { return String::new(); }
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}

/// a result's date as the engine gives it (an ISO time or a day), as its day: "2026-11-09"; None when there is none
fn day_of(v: &Value) -> Option<String> {
    let s = v.as_str()?.trim();
    let d: String = s.chars().take(10).collect();
    let ok = d.len() == 10 && d.chars().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    ok.then_some(d)
}

/// a snippet with its result's date in front ("(2026-11-09) …"), so a news answer can tell new from old
fn dated_snippet(day: Option<String>, snippet: String) -> String {
    match day { Some(d) if snippet.is_empty() => format!("({d})"), Some(d) => format!("({d}) {snippet}"), None => snippet }
}

/// each engine's past-week filter as a query parameter (`&…`), or nothing
fn week_param(engine: &str, week: bool) -> &'static str {
    if !week { return ""; }
    match engine { DUCKDUCKGO => "&df=w", SEARXNG => "&time_range=week", _ => "&freshness=pw" }
}

pub fn format_hits(q: &str, hits: &[Hit]) -> String {
    if hits.is_empty() { return format!("no results for \"{q}\""); }
    let mut out = format!("results for \"{q}\":\n");
    for (i, h) in hits.iter().take(HITS).enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, h.title, h.url));
        if !h.snippet.is_empty() { out.push_str(&format!("   {}\n", h.snippet)); }
    }
    out.trim_end().to_string()
}

/// DuckDuckGo rate-limits automated searches: a refusal is its bot check (`anomaly-modal`), or a page without any
/// result markup and without its "no results" notice. One retry after a short pause, then a plain refusal message.
fn duckduckgo(q: &str, week: bool) -> Result<Vec<Hit>, String> {
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 { std::thread::sleep(Duration::from_millis(1500)); }
        let mut r = agent(15).get(format!("https://html.duckduckgo.com/html/?q={}{}", enc(q), week_param(DUCKDUCKGO, week))).header("user-agent", UA)
            .call().map_err(|e| format!("cannot reach DuckDuckGo: {e}"))?;
        let st = r.status().as_u16();
        let html = r.body_mut().with_config().limit(PAGE_BYTES).read_to_string().unwrap_or_default();
        let refused = st != 200 || html.contains("anomaly-modal") || (!html.contains("result__") && !html.contains("No results"));
        if !refused { return Ok(parse_duckduckgo(&html)); }
        last = format!("HTTP {st}");
    }
    Err(format!("DuckDuckGo refused the search ({last}, its bot check for automated searches): try again in a minute, \
or set web = \"searxng\" with your own SearXNG server for reliable search"))
}

/// DuckDuckGo's HTML results: `result__a` links (through its `/l/?uddg=` redirect) and `result__snippet`s; ads
/// (`y.js` links) are left out
pub fn parse_duckduckgo(html: &str) -> Vec<Hit> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?s)class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>(.*?)(?:class="result__snippet"[^>]*>(.*?)</a>|class="result__a")"#).unwrap());
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(c) = re.captures_at(html, at) {
        let m = c.get(0).unwrap();
        // the next result may start inside this match's tail (no snippet): continue from its title link
        at = c.get(3).map_or(m.end(), |t| if c.get(4).is_none() { t.end() } else { m.end() });
        let href = entities(&c[1]);
        if href.contains("duckduckgo.com/y.js") { continue; }
        let url = href.split_once("uddg=").map_or(href.clone(), |(_, rest)| dec(rest.split('&').next().unwrap_or("")));
        let url = if url.starts_with("//") { format!("https:{url}") } else { url };
        out.push(Hit { title: text_of(&c[2]), url, snippet: c.get(4).map_or(String::new(), |s| text_of(s.as_str())) });
    }
    out
}

fn searxng(base: &str, q: &str, week: bool) -> Result<Vec<Hit>, String> {
    let base = base.trim_end_matches('/');
    if base.is_empty() { return Err("set the SearXNG server URL (searxng_url) first".into()); }
    let mut r = agent(15).get(format!("{base}/search?q={}&format=json{}", enc(q), week_param(SEARXNG, week))).header("accept", "application/json")
        .call().map_err(|e| format!("no SearXNG server answers at {base} ({e}): is it running?"))?;
    let st = r.status().as_u16();
    let t = r.body_mut().with_config().limit(PAGE_BYTES).read_to_string().unwrap_or_default();
    if st == 403 { return Err(format!("SearXNG at {base} refuses JSON: add json to search.formats in its settings.yml")); }
    if !(200..300).contains(&st) { return Err(format!("SearXNG at {base}: HTTP {st}")); }
    let v: Value = serde_json::from_str(&t).map_err(|_| format!("SearXNG at {base} did not answer with JSON"))?;
    Ok(parse_searxng(&v))
}

pub fn parse_searxng(v: &Value) -> Vec<Hit> {
    v["results"].as_array().map(|rs| rs.iter().filter_map(|r| Some(Hit {
        title: r["title"].as_str()?.trim().to_string(),
        url: r["url"].as_str()?.to_string(),
        snippet: dated_snippet(day_of(&r["publishedDate"]), one_line(r["content"].as_str().unwrap_or(""))),
    })).collect()).unwrap_or_default()
}

/// the Brave Search API address: a local `ELISA_BRAVE_URL` (tests) or the real one
fn brave_base() -> String { brave_base_from(std::env::var(BRAVE_URL_ENV).ok().as_deref()) }

fn brave_base_from(env: Option<&str>) -> String {
    match env { Some(u) if crate::mcp_sec::local_url(u) => u.trim().trim_end_matches('/').to_string(), _ => BRAVE_URL.into() }
}

/// The Brave Search API (`GET /res/v1/web/search`, key in `X-Subscription-Token`). Errors are one line for the model
/// and the user, and never carry the key (a message that somehow did is scrubbed).
fn brave(base: &str, key: &str, q: &str, week: bool) -> Result<Vec<Hit>, String> {
    if key.is_empty() { return Err(format!("web = \"{BRAVE}\" needs an API key: set {BRAVE_KEY_ENV} or brave_api_key in ~/.config/elisa/config.toml")); }
    let scrub = |e: String| e.replace(key, "[key]");
    let base = base.trim_end_matches('/');
    let mut r = agent(15).get(format!("{base}/res/v1/web/search?q={}&count={HITS}{}", enc(q), week_param(BRAVE, week)))
        .header("accept", "application/json").header("x-subscription-token", key)
        .call().map_err(|e| scrub(format!("cannot reach Brave Search: {e}")))?;
    let st = r.status().as_u16();
    let t = r.body_mut().with_config().limit(PAGE_BYTES).read_to_string().unwrap_or_default();
    match st {
        200..=299 => {}
        401 | 403 => return Err(format!("Brave Search refused the API key (HTTP {st}): check {BRAVE_KEY_ENV} or brave_api_key")),
        429 => return Err("Brave Search rate limit reached (HTTP 429): wait a moment and try again, or check the plan's quota".into()),
        _ => return Err(format!("Brave Search: HTTP {st}")),
    }
    let v: Value = serde_json::from_str(&t).map_err(|_| "Brave Search did not answer with JSON".to_string())?;
    Ok(parse_brave(&v))
}

/// Brave's `web.results[]`: title, url, description (which marks matches with `<strong>`: markup and entities out)
pub fn parse_brave(v: &Value) -> Vec<Hit> {
    v["web"]["results"].as_array().map(|rs| rs.iter().filter_map(|r| Some(Hit {
        title: text_of(r["title"].as_str()?),
        url: r["url"].as_str()?.to_string(),
        snippet: dated_snippet(day_of(&r["page_age"]), text_of(r["description"].as_str().unwrap_or(""))),
    })).collect()).unwrap_or_default()
}

/// `web_fetch {url}`: the page as plain text (the host fences it as untrusted), at most PAGE_CHARS
pub fn fetch(url: &str) -> Result<String, String> { fetch_capped(url, PAGE_CHARS) }

/// `fetch` with its own size cap (a page Elisa adds to a small context gets less room)
/// Through Elisa's result cache (`cache.rs`, 1 h per normalised URL and cap): errors and `Cache-Control: no-store`
/// pages are not stored. The address is checked before the cache is asked.
pub fn fetch_capped(url: &str, max_chars: usize) -> Result<String, String> {
    check_public(url.trim())?;
    let key = format!("{max_chars}\n{}", normalise_url(url.trim()));
    crate::cache::cached(crate::cache::Kind::WebFetch, &key, || fetch_page(url, max_chars, &fetch_agent(20), true))
}

/// Tests only: `fetch_capped` through the cache without the public-address checks, for a local stand-in server.
/// Never reachable from a model's tool call (only Rust code calls it).
#[doc(hidden)]
pub fn fetch_local_for_tests(url: &str, max_chars: usize) -> Result<String, String> {
    let key = format!("{max_chars}\n{}", normalise_url(url.trim()));
    crate::cache::cached(crate::cache::Kind::WebFetch, &key, || fetch_page(url, max_chars, &agent(20), false))
}

/// A URL as the cache keys it: scheme and host lower case, no default port, no fragment, "/" for an empty path.
pub fn normalise_url(url: &str) -> String {
    let url = url.split('#').next().unwrap_or("");
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let scheme = scheme.to_ascii_lowercase();
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(cut);
    let mut auth = auth.to_ascii_lowercase();
    let port = if scheme == "https" { ":443" } else { ":80" };
    if auth.ends_with(port) { auth.truncate(auth.len() - port.len()); }
    let tail = if tail.is_empty() || tail.starts_with('?') { format!("/{tail}") } else { tail.to_string() };
    format!("{scheme}://{auth}{tail}")
}

/// the page as text and whether it may be cached (not when the server said `Cache-Control: no-store`)
fn fetch_page(url: &str, max_chars: usize, a: &ureq::Agent, public_only: bool) -> Result<(String, bool), String> {
    let asked = url.trim().to_string();
    let mut url = asked.clone();
    for _ in 0..6 {
        // every hop: the text check here, the resolved addresses in the agent's resolver
        if public_only { check_public(&url)?; }
        let mut r = a.get(&url).header("user-agent", UA).header("accept", "text/html,text/plain;q=0.9,*/*;q=0.5")
            .call().map_err(|e| {
                let e = e.to_string();
                // the resolver's refusal, without ureq's "io: " prefix
                match e.find(NOT_PUBLIC) { Some(_) => e.strip_prefix("io: ").unwrap_or(&e).to_string(), None => format!("cannot fetch {url}: {e}") }
            })?;
        let st = r.status().as_u16();
        if (300..400).contains(&st) {
            let loc = r.headers().get("location").and_then(|l| l.to_str().ok()).ok_or_else(|| format!("{url}: redirect without a location"))?;
            url = join(&url, loc);
            continue;
        }
        // a site that refuses automated reading: say what to do instead (otherwise the model searches the same query
        // again and is stopped as a loop, with no answer)
        if matches!(st, 401 | 403 | 429) { return Err(format!("{url}: HTTP {st} (this site blocks automated reading: answer from the search results' titles and snippets, or fetch another result)")); }
        if !(200..300).contains(&st) { return Err(format!("{url}: HTTP {st}")); }
        let ct = r.headers().get("content-type").and_then(|c| c.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        let store = !r.headers().get_all("cache-control").iter().any(|v| v.to_str().is_ok_and(|v| v.to_ascii_lowercase().contains("no-store")));
        // image/* out first: image/svg+xml would pass as xml
        if ct.trim_start().starts_with("image/") || !(ct.is_empty() || ct.contains("text/") || ct.contains("json") || ct.contains("xml")) {
            return Err(format!("{url} is not a text page ({ct})"));
        }
        let body = r.body_mut().with_config().limit(PAGE_BYTES).read_to_string().map_err(|e| format!("{url}: {e}"))?;
        let (title, text) = if ct.contains("html") || (ct.is_empty() && body.trim_start().starts_with('<')) { html_to_text(&body) } else { (String::new(), body) };
        let text = if text.chars().count() > max_chars {
            format!("{}\n[… page cut at {max_chars} characters]", text.chars().take(max_chars).collect::<String>())
        } else { text };
        // no "untrusted" line of its own: the host's `<untrusted>` fence (with the url) says it
        let mut head = String::new();
        if url != asked { head.push_str(&format!("redirected to {url}\n")); }
        if !title.is_empty() { head.push_str(&format!("title: {title}\n")); }
        return Ok((if head.is_empty() { text } else { format!("{head}\n{text}") }, store));
    }
    Err(format!("{url}: too many redirects"))
}

/// a relative redirect against the page it came from
fn join(base: &str, loc: &str) -> String {
    if loc.contains("://") { return loc.to_string(); }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let host = rest.split('/').next().unwrap_or("");
    if let Some(p) = loc.strip_prefix("//") { return format!("{scheme}://{p}"); }
    if loc.starts_with('/') { return format!("{scheme}://{host}{loc}"); }
    let dir = rest.rsplit_once('/').map_or(rest, |(d, _)| d);
    format!("{scheme}://{dir}/{loc}")
}

/// http(s) only, and not this machine or the local network by its text: names without a dot, localhost, .local /
/// .internal (a trailing dot ignored), and address literals that [`is_public_ip`] refuses. A first pass only: names
/// that resolve to local addresses (`localtest.me`, `127.0.0.1.nip.io`) and literals in forms std does not parse
/// (`127.1`, `0x7f.0.0.1`) are caught by the fetch agent's resolver, which checks every address it connects to.
pub fn check_public(url: &str) -> Result<(), String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("not a web address: {url}"))?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") { return Err(format!("only http and https pages: {url}")); }
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let host = if let Some(v6) = auth.strip_prefix('[') { v6.split(']').next().unwrap_or("").to_string() }
        else { auth.split(':').next().unwrap_or("").to_string() }.to_ascii_lowercase();
    let local = || Err(format!("{host} {NOT_PUBLIC}"));
    // "localhost." is localhost: the checks see the name without its trailing dot(s)
    let name = host.trim_end_matches('.');
    if name.is_empty() { return if host.is_empty() { Err(format!("no host in {url}")) } else { local() }; }
    if name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local") || name.ends_with(".internal") { return local(); }
    if let Ok(ip) = name.parse::<std::net::IpAddr>() { return if is_public_ip(ip) { Ok(()) } else { local() }; }
    if !name.contains('.') { return local(); }
    Ok(())
}

/// (title, readable text): scripts, styles and markup out, blocks to lines, entities decoded
pub fn html_to_text(html: &str) -> (String, String) {
    static DROP: OnceLock<Regex> = OnceLock::new();
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LI: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    let title = TITLE.get_or_init(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap())
        .captures(html).map_or(String::new(), |c| one_line(&entities(&c[1])));
    let t = DROP.get_or_init(|| Regex::new(r"(?is)<(script|style|noscript|svg|head|template|iframe)\b.*?</(script|style|noscript|svg|head|template|iframe)\s*>|<!--.*?-->").unwrap()).replace_all(html, " ");
    let t = LI.get_or_init(|| Regex::new(r"(?i)<li\b[^>]*>").unwrap()).replace_all(&t, "\n- ");
    let t = BLOCK.get_or_init(|| Regex::new(r"(?i)<br\s*/?>|</?(p|div|section|article|header|footer|main|nav|aside|h[1-6]|tr|table|ul|ol|pre|blockquote|dd|dt)\b[^>]*>").unwrap()).replace_all(&t, "\n");
    let t = TAG.get_or_init(|| Regex::new(r"(?s)<[^>]*>").unwrap()).replace_all(&t, "");
    let t = entities(&t);
    let mut out = String::new();
    let mut blank = false;
    for line in t.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() { blank = !out.is_empty(); continue; }
        if blank { out.push('\n'); blank = false; }
        out.push_str(&l);
        out.push('\n');
    }
    (title, out.trim_end().to_string())
}

/// a fragment of markup as one line of text
fn text_of(s: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    one_line(&entities(&TAG.get_or_init(|| Regex::new(r"(?s)<[^>]*>").unwrap()).replace_all(s, "")))
}

/// the http(s) links in a message, in order, without trailing punctuation, at most `max`
pub fn urls_in(text: &str, max: usize) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for w in text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '(' | ')')) {
        let w = w.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);
        if (w.starts_with("http://") || w.starts_with("https://")) && w.len() > 10 && !v.iter().any(|u| u == w) {
            v.push(w.to_string());
            if v.len() == max { break; }
        }
    }
    v
}

/// words that carry no search meaning in a request to an assistant ("can you please search the web for …")
const FILLER: &[&str] = &["a", "an", "the", "please", "pls", "can", "could", "would", "will", "you", "u", "me", "my", "i",
    "i'm", "im", "we", "us", "our", "tell", "show", "give", "find", "search", "look", "lookup", "google", "web", "internet",
    "online", "up", "for", "about", "regarding", "on", "of", "to", "and", "or", "is", "are", "was", "be", "do", "does",
    "did", "it", "this", "that", "some", "any", "info", "information", "know", "let", "hey", "hi", "elisa", "thanks",
    "thank", "just", "quickly", "briefly", "what's", "whats"];

/// a search query from a message (the `/web` search; no model involved): quoted phrases kept whole, links dropped
/// (they are fetched instead), filler words dropped, at most 12 words. Names, versions and code identifiers survive
/// because they are not filler. Falls back to the message itself when nothing is left.
pub fn query_terms(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text.to_string();
    // "quoted phrases" first, kept as they are
    while let Some(a) = rest.find('"') {
        let Some(len) = rest[a + 1..].find('"') else { break };
        let ph = rest[a + 1..a + 1 + len].trim().to_string();
        if !ph.is_empty() { out.push(format!("\"{ph}\"")); }
        rest.replace_range(a..a + len + 2, " ");
    }
    for w in rest.split_whitespace() {
        if w.starts_with("http://") || w.starts_with("https://") { continue; }
        let t = w.trim_matches(|c: char| matches!(c, ',' | ';' | '!' | '?' | '(' | ')' | '[' | ']' | '\'') || (c == '.' && !w.chars().any(|d| d.is_ascii_digit())));
        if t.is_empty() || FILLER.contains(&t.to_lowercase().as_str()) { continue; }
        out.push(t.to_string());
    }
    out.truncate(12);
    let q = out.join(" ");
    if q.is_empty() { one_line(text).chars().take(200).collect() } else { q }
}

fn one_line(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }

/// the HTML entities pages use most, and numeric ones
pub fn entities(s: &str) -> String {
    if !s.contains('&') { return s.to_string(); }
    static NUM: OnceLock<Regex> = OnceLock::new();
    let named = s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
        .replace("&#39;", "'").replace("&apos;", "'").replace("&ndash;", "–").replace("&mdash;", "—").replace("&hellip;", "…")
        .replace("&rsquo;", "’").replace("&lsquo;", "‘").replace("&rdquo;", "”").replace("&ldquo;", "“");
    let n = NUM.get_or_init(|| Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").unwrap()).replace_all(&named, |c: &regex::Captures| {
        let v = &c[1];
        let code = if let Some(h) = v.strip_prefix('x') { u32::from_str_radix(h, 16).ok() } else { v.parse().ok() };
        code.and_then(char::from_u32).map_or(String::new(), String::from)
    });
    n.replace("&amp;", "&")
}

/// percent-encoding for a query value
pub fn enc(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        b' ' => "+".into(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// percent-decoding (DuckDuckGo's `uddg=` target)
pub fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => { out.push(v); i += 3; continue; }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_drop_filler_and_keep_names() {
        assert_eq!(query_terms("Can you please search the web for the latest Ollama release?"), "latest Ollama release");
        assert_eq!(query_terms("what is new in Rust 1.92"), "what new in Rust 1.92");
        assert_eq!(query_terms("tell me about \"tokio::spawn\" panics in main"), "\"tokio::spawn\" panics in main");
        assert_eq!(query_terms("read https://example.com/a and summarise"), "read summarise");
        assert_eq!(query_terms("can you"), "can you");
    }

    #[test]
    fn urls_in_finds_links_without_punctuation() {
        assert_eq!(urls_in("see https://a.example/x, and (https://b.example/y). also https://a.example/x", 5),
            ["https://a.example/x", "https://b.example/y"]);
        assert_eq!(urls_in("no links here, just http:// text", 5), Vec::<String>::new());
        assert_eq!(urls_in("https://a.example/1 https://a.example/2 https://a.example/3", 2).len(), 2);
    }

    /// A news search asks each engine for the past week only
    #[test]
    fn a_recent_search_uses_each_engines_week_filter() {
        assert_eq!(week_param(DUCKDUCKGO, true), "&df=w");
        assert_eq!(week_param(SEARXNG, true), "&time_range=week");
        assert_eq!(week_param(BRAVE, true), "&freshness=pw");
        for e in [DUCKDUCKGO, SEARXNG, BRAVE] { assert_eq!(week_param(e, false), ""); }
    }

    #[test]
    fn duckduckgo_results_are_read_and_ads_left_out() {
        let html = r#"
<div class="result results_links result--ad"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad=1">Ad</a>
<a class="result__snippet" href="x">buy</a></div>
<div class="result"><h2><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2Flearn&amp;rut=abc">Learn <b>Rust</b></a></h2>
<a class="result__snippet" href="//duckduckgo.com/l/?uddg=x">The <b>Rust</b> book &amp; more</a></div>
<div class="result"><a rel="nofollow" class="result__a" href="https://example.org/">No snippet</a></div>
<div class="result"><a rel="nofollow" class="result__a" href="https://example.net/">Third</a><a class="result__snippet" href="y">s3</a></div>"#;
        let h = parse_duckduckgo(html);
        assert_eq!(h.len(), 3, "{h:?}");
        assert_eq!(h[0], Hit { title: "Learn Rust".into(), url: "https://www.rust-lang.org/learn".into(), snippet: "The Rust book & more".into() });
        assert_eq!((h[1].url.as_str(), h[1].snippet.as_str()), ("https://example.org/", ""));
        assert_eq!((h[2].title.as_str(), h[2].snippet.as_str()), ("Third", "s3"));
    }

    #[test]
    fn searxng_results_are_read() {
        let v = serde_json::json!({"results": [{"title": " A ", "url": "https://a.org", "content": "one\n two"}, {"url": "https://no-title"}]});
        assert_eq!(parse_searxng(&v), vec![Hit { title: "A".into(), url: "https://a.org".into(), snippet: "one two".into() }]);
    }

    #[test]
    fn brave_results_are_read_without_markup() {
        let v = serde_json::json!({"web": {"results": [
            {"title": "Learn <strong>Rust</strong>", "url": "https://www.rust-lang.org/learn", "description": "The <strong>Rust</strong> book &amp; more"},
            {"title": "no url"}]}});
        assert_eq!(parse_brave(&v), vec![Hit { title: "Learn Rust".into(), url: "https://www.rust-lang.org/learn".into(), snippet: "The Rust book & more".into() }]);
        assert!(parse_brave(&serde_json::json!({"type": "search"})).is_empty());
    }

    /// A news search names this month and year (from the clock), unless the words already name a date
    #[test]
    fn a_news_query_names_the_current_month() {
        // 2026-11-10 12:00 UTC: November 2026 in every zone
        let now = 1_794_312_000;
        assert_eq!(dated_query("latest news about Apple", now), "latest news about Apple November 2026");
        assert_eq!(dated_query("Apple news 2025", now), "Apple news 2025");
        assert_eq!(dated_query("Apple news in march", now), "Apple news in march");
        // a number that is no year: still dated
        assert_eq!(dated_query("iPhone 17 news", now), "iPhone 17 news November 2026");
        assert_eq!(today(now), "2026-11-10");
    }

    /// The engines that give a result's date: it leads the snippet
    #[test]
    fn result_dates_lead_the_snippet() {
        let v = serde_json::json!({"results": [{"title": "A", "url": "https://a.org", "content": "x", "publishedDate": "2026-11-09T08:00:00"},
            {"title": "B", "url": "https://b.org", "content": "y", "publishedDate": null}]});
        let h = parse_searxng(&v);
        assert_eq!((h[0].snippet.as_str(), h[1].snippet.as_str()), ("(2026-11-09) x", "y"));
        let v = serde_json::json!({"web": {"results": [{"title": "A", "url": "https://a.org", "description": "x", "page_age": "2026-11-08T10:00:00"}]}});
        assert_eq!(parse_brave(&v)[0].snippet, "(2026-11-08) x");
    }

    #[test]
    fn brave_without_a_key_says_so_and_the_key_never_shows_in_debug() {
        let e = brave("http://127.0.0.1:9", "", "x", false).unwrap_err();
        assert!(e.contains("needs an API key") && e.contains(BRAVE_KEY_ENV), "{e}");
        let cfg = WebCfg { engine: BRAVE.into(), searxng: String::new(), brave_key: "sekret-123".into() };
        assert!(!format!("{cfg:?}").contains("sekret"), "{cfg:?}");
    }

    #[test]
    fn brave_url_override_is_local_only() {
        // only a local host is honoured: any other would get the key
        assert_eq!(brave_base_from(None), BRAVE_URL);
        assert_eq!(brave_base_from(Some("http://127.0.0.1:8123/")), "http://127.0.0.1:8123");
        assert_eq!(brave_base_from(Some("http://localhost:9000")), "http://localhost:9000");
        for bad in ["", "https://evil.example", "http://evil.example", "http://127.0.0.1@evil.example/", "http://localhost:80@evil.example",
                    "https://127.0.0.1:8123", "http://127.0.0.1.nip.io/", "http://localhost.evil.example/", "ftp://127.0.0.1/"] {
            assert_eq!(brave_base_from(Some(bad)), BRAVE_URL, "{bad}");
        }
    }

    #[test]
    fn pages_become_text_without_scripts_and_markup() {
        let (t, x) = html_to_text("<html><head><title>T &amp; U</title><script>evil()</script></head><body><h1>Head</h1><p>One&nbsp;<b>two</b></p><ul><li>a</li><li>b</li></ul><style>p{}</style>x &#169; &#x41;</body></html>");
        assert_eq!(t, "T & U");
        assert!(!x.contains("evil") && !x.contains("p{}"), "{x}");
        // blocks are lines, at most one blank line between them
        assert!(x.contains("Head\n\nOne two") && !x.contains("\n\n\n") && x.contains("- a\n- b") && x.contains("x © A"), "{x}");
    }

    #[test]
    fn only_public_web_addresses_are_fetched() {
        for ok in ["https://www.rust-lang.org/learn", "http://93.184.216.34/", "https://[2606:4700::1111]/"] { assert!(check_public(ok).is_ok(), "{ok}"); }
        for bad in ["http://localhost:8080", "http://127.0.0.1/", "http://192.168.1.1", "http://10.0.0.5/admin", "http://172.20.1.1",
                    "http://169.254.169.254/latest/meta-data", "http://[::1]:11434/api/tags", "http://router/", "http://printer.local/",
                    "file:///etc/passwd", "ftp://example.com", "http://user@127.0.0.1/", "http://100.64.0.1/", "http://[fd00::1]/"] {
            assert!(check_public(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn local_addresses_are_classified_as_not_public() {
        use std::net::{IpAddr, ToSocketAddrs};
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for bad in ["127.0.0.1", "127.255.0.9", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.0.1", "169.254.169.254",
                    "100.64.0.1", "100.127.255.254", "0.0.0.0", "0.1.2.3", "255.255.255.255", "224.0.0.1", "240.0.0.1",
                    "::1", "::", "fe80::1", "fd00::1", "fc00::5", "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1",
                    "::ffff:100.64.0.1", "::127.0.0.1", "::ffff:0.0.0.0", "64:ff9b::7f00:1", "fec0::1"] {
            assert!(!is_public_ip(ip(bad)), "{bad}");
        }
        for ok in ["93.184.216.34", "1.1.1.1", "8.8.8.8", "100.128.0.1", "172.32.0.1", "2606:4700::1", "2a00:1450:4001::200e",
                   "::ffff:93.184.216.34", "64:ff9b::808:808"] {
            assert!(is_public_ip(ip(ok)), "{ok}");
        }
        // what the system resolver makes of the short and hex forms std does not parse (no internet involved)
        for h in ["127.1:80", "0x7f.0.0.1:80", "localhost.:80"] {
            let addrs: Vec<_> = h.to_socket_addrs().map(|a| a.collect()).unwrap_or_default();
            assert!(addrs.iter().all(|a: &std::net::SocketAddr| !is_public_ip(a.ip())), "{h}: {addrs:?}");
        }
    }

    #[test]
    fn local_hosts_are_refused_also_when_only_their_resolution_shows_it() {
        for bad in ["http://localhost.:9/", "http://LOCALHOST..:9/", "http://printer.local./", "http://router./", "http://[::ffff:127.0.0.1]/",
                    "http://0.0.0.0/", "http://[fe80::1]/"] {
            assert!(check_public(bad).is_err(), "{bad}");
        }
        // 127.1 and 0x7f.0.0.1 pass the text check; the fetch agent's resolver refuses them before any connection
        for bad in ["http://127.1:9/", "http://0x7f.0.0.1:9/", "http://localhost.:9/"] {
            let e = fetch(bad).unwrap_err();
            assert!(e.contains(NOT_PUBLIC) && !e.starts_with("io:"), "{bad}: {e}");
        }
    }

    #[test]
    fn queries_and_redirect_targets_are_encoded() {
        assert_eq!(enc("rust ureq 3 & json"), "rust+ureq+3+%26+json");
        assert_eq!(dec("https%3A%2F%2Fa.org%2Fx%3Fq%3D1"), "https://a.org/x?q=1");
        assert_eq!(join("https://a.org/x/y", "/z"), "https://a.org/z");
        assert_eq!(join("https://a.org/x/y", "z"), "https://a.org/x/z");
        assert_eq!(join("https://a.org/x", "https://b.org/"), "https://b.org/");
    }
}
