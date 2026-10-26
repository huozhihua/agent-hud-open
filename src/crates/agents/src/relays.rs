//! Relays (中转站) the clients are configured with, and their balances
//! (docs/proposals/no-plan-usage.md §5). A relay is found where a client
//! keeps its address and key: Claude Code's settings.json `env` or this
//! process's environment, Codex's `config.toml` providers. Nothing is asked
//! of a relay until the owner agrees to it, address by address
//! (`relayReading` in Agent HUD's settings); then the key the client already
//! sends it is sent to that same address, and nowhere else, to read its
//! balance: OpenRouter's credits, a New API site (one-api / new-api, told by
//! its public `/api/status`), or a Sub2API site's `/v1/usage`.
use crate::account::{read_json, variable, Host, HttpFailure, HttpRequest, Reading};
use crate::endpoints::{base_url_host, host_kind, HostKind};
use crate::l10n;
use serde_json::{json, Value};
use std::time::Duration;

/// The setting that lists the relay addresses the owner agreed to read.
pub const CONSENT: &str = "relayReading";

/// The consent key `Host::consented` answers for one relay address.
pub fn consent_key(host: &str) -> String {
    format!("{CONSENT}:{host}")
}

/// One relay a client is configured with.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    /// The client's source id: whose calls the relay carries.
    client: &'static str,
    /// The relay's host, as calls are recorded under it.
    host: String,
    /// `https://host[:port]`, where its own endpoints are.
    origin: String,
    key: String,
}

/// A relay's balance as it reports it.
struct Balance {
    kind: &'static str,
    label: String,
    amount: f64,
    currency: &'static str,
}

pub fn read(host: &dyn Host) -> Result<Reading, String> {
    let mut reading = Reading::empty("relay");
    for target in targets(host) {
        let consented = host.consented(&consent_key(&target.host));
        let mut service = json!({"client": target.client, "provider": target.host, "product": "relay", "consented": consented});
        if consented && !host.cancelled() {
            let (balance, prices) = balance(host, &target);
            if let Some(prices) = prices {
                service["kind"] = json!("newapi");
                service["prices"] = prices;
            }
            match balance {
                Ok(found) => {
                    service["kind"] = json!(found.kind);
                    reading.billing.push(json!({
                        "id": format!("relay:{}:{}", target.client, target.host),
                        "vendor": target.client,
                        "consumerRoutes": [target.host],
                        "label": found.label,
                        "amount": (found.amount * 100.0).round() / 100.0,
                        "currency": found.currency,
                        "available": true,
                    }));
                }
                Err(notice) => service["notice"] = json!(notice),
            }
        }
        reading.services.push(service);
    }
    Ok(reading)
}

/// Every relay the clients are configured with, once per client and host.
fn targets(host: &dyn Host) -> Vec<Target> {
    let mut found = Vec::<Target>::new();
    let mut add = |client: &'static str, base: Option<&str>, key: Option<&str>| {
        let (Some(base), Some(key)) = (base, key.map(str::trim).filter(|key| !key.is_empty()))
        else {
            return;
        };
        let Some(relay) = base_url_host(base).filter(|relay| host_kind(relay) == HostKind::Relay)
        else {
            return;
        };
        if key.bytes().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
            || found.iter().any(|t| t.client == client && t.host == relay)
        {
            return;
        }
        found.push(Target {
            client,
            origin: format!("https://{relay}"),
            host: relay,
            key: key.to_owned(),
        });
    };
    // Claude Code: its settings' env, then this process's own environment.
    let claude = variable(host, "CLAUDE_CONFIG_DIR").unwrap_or_else(|| host.home().join(".claude"));
    for name in ["settings.json", "settings.local.json"] {
        if let Some(settings) = read_json(host, &claude.join(name)) {
            let env = &settings["env"];
            let key = env["ANTHROPIC_AUTH_TOKEN"]
                .as_str()
                .or_else(|| env["ANTHROPIC_API_KEY"].as_str());
            add("claude", env["ANTHROPIC_BASE_URL"].as_str(), key);
        }
    }
    let key = host
        .env("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| host.env("ANTHROPIC_API_KEY"));
    add("claude", host.env("ANTHROPIC_BASE_URL"), key);
    // Codex: the providers its config.toml defines.
    let codex = variable(host, "CODEX_HOME").unwrap_or_else(|| host.home().join(".codex"));
    if let Some(bytes) = host
        .read_file(&codex.join("config.toml"), 1024 * 1024)
        .found()
    {
        for provider in codex_providers(&String::from_utf8_lossy(&bytes)) {
            let key = provider.bearer.clone().or_else(|| {
                provider
                    .env_key
                    .as_deref()
                    .and_then(|name| host.env(name))
                    .map(str::to_owned)
            });
            add("codex", Some(&provider.base_url), key.as_deref());
        }
    }
    found
}

/// One `[model_providers.<id>]` table of a Codex `config.toml`.
#[derive(Debug, Default, PartialEq)]
struct CodexProvider {
    base_url: String,
    env_key: Option<String>,
    bearer: Option<String>,
}

/// The model providers a Codex `config.toml` defines: each
/// `[model_providers.<id>]` table's `base_url`, `env_key` and
/// `experimental_bearer_token`, read as plain string values.
fn codex_providers(toml: &str) -> Vec<CodexProvider> {
    let mut providers = Vec::new();
    let mut current: Option<CodexProvider> = None;
    let mut flush = |current: &mut Option<CodexProvider>| {
        if let Some(provider) = current.take().filter(|p| !p.base_url.is_empty()) {
            providers.push(provider);
        }
    };
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush(&mut current);
            let table = line.trim_start_matches('[').trim_end_matches(']').trim();
            if let Some(id) = table.strip_prefix("model_providers.") {
                if !id.trim_matches('"').contains('.') {
                    current = Some(CodexProvider::default());
                }
            }
            continue;
        }
        let Some(provider) = current.as_mut() else {
            continue;
        };
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
            .map(|(text, _)| text.to_owned())
        else {
            continue;
        };
        match name.trim() {
            "base_url" => provider.base_url = value,
            "env_key" => provider.env_key = Some(value),
            "experimental_bearer_token" => provider.bearer = Some(value),
            _ => {}
        }
    }
    flush(&mut current);
    providers
}

/// What the relay reports of the key's balance, and a New API site's price
/// list (`new_api_prices`), which it shows whatever the key's limit.
fn balance(host: &dyn Host, target: &Target) -> (Result<Balance, String>, Option<Value>) {
    let ask = |url: &str, key: bool| {
        let authorization = format!("Bearer {}", target.key);
        let secrets: &[(&str, &str)] = if key {
            &[("Authorization", &authorization)]
        } else {
            &[]
        };
        host.http_json(&HttpRequest {
            url,
            secrets,
            headers: &[],
            body: None,
            timeout: Duration::from_secs(12),
        })
    };
    if target.host == "openrouter.ai" {
        return (openrouter(&ask), None);
    }
    // A New API site says so on its public status page.
    if let Ok(status) = ask(&format!("{}/api/status", target.origin), false) {
        if status["data"]["quota_per_unit"]
            .as_f64()
            .is_some_and(|per| per > 0.0)
        {
            let balance = new_api(&status["data"], &target.host, &|url| {
                ask(&format!("{}{url}", target.origin), true)
            });
            let prices = ask(&format!("{}/api/pricing", target.origin), false)
                .ok()
                .and_then(|body| new_api_prices(&body, &status["data"]));
            return (balance, prices);
        }
    }
    if let Ok(usage) = ask(&format!("{}/v1/usage", target.origin), true) {
        let remaining = usage["remaining"]
            .as_f64()
            .or_else(|| usage["quota"]["remaining"].as_f64())
            .or_else(|| usage["balance"].as_f64());
        if let Some(amount) = remaining {
            let unit = usage["unit"]
                .as_str()
                .or_else(|| usage["quota"]["unit"].as_str())
                .unwrap_or("USD");
            let balance = currency(unit)
                .ok_or_else(unsupported_unit)
                .map(|currency| Balance {
                    kind: "sub2api",
                    label: target.host.clone(),
                    amount,
                    currency,
                });
            return (balance, None);
        }
    }
    (
        Err(l10n::text(
            "还不认识这个中转站的余额接口",
            "This relay's balance endpoint is not one Agent HUD knows yet.",
        )),
        None,
    )
}

/// A New API site's price list, in what calls through it cost per million
/// tokens in the money the site shows: `{currency, models: {name: [input,
/// output, cache read, cache write]}}`. A ratio of 1 is $2 per million input
/// tokens; output is input times the completion ratio, cache reads and writes
/// input times their ratios, all times the group's ratio. Models priced per
/// call or by a billing script are left out, and so is everything when the
/// group cannot be told (several groups, none called `default`).
fn new_api_prices(body: &Value, status: &Value) -> Option<Value> {
    let shown = status["quota_display_type"]
        .as_str()
        .map(str::to_ascii_uppercase);
    let (rate, currency) = match shown.as_deref() {
        Some("CNY") => (status["usd_exchange_rate"].as_f64()?, "CNY"),
        Some("USD") => (1.0, "USD"),
        None if status["display_in_currency"] != false => (1.0, "USD"),
        _ => return None,
    };
    let group = match body["group_ratio"].as_object() {
        None => 1.0,
        Some(groups) if groups.is_empty() => 1.0,
        Some(groups) if groups.len() == 1 => groups.values().next()?.as_f64()?,
        Some(groups) => groups.get("default")?.as_f64()?,
    };
    let mut models = serde_json::Map::new();
    for item in body["data"].as_array()? {
        let (Some(name), Some(ratio)) = (item["model_name"].as_str(), item["model_ratio"].as_f64())
        else {
            continue;
        };
        let scripted = item["billing_expr"]
            .as_str()
            .is_some_and(|expr| !expr.is_empty());
        if item["quota_type"].as_i64() != Some(0) || scripted || ratio <= 0.0 {
            continue;
        }
        let input = ratio * 2.0 * group * rate;
        let times = |key: &str| item[key].as_f64().unwrap_or(1.0);
        models.insert(
            name.to_owned(),
            json!([
                input,
                input * times("completion_ratio"),
                input * times("cache_ratio"),
                input * times("create_cache_ratio")
            ]),
        );
    }
    (!models.is_empty()).then(|| json!({"currency": currency, "models": models}))
}

/// A New API site: the key's account balance when the site lets the key read
/// it, else what the key may still spend, in the money the site shows.
fn new_api(
    status: &Value,
    relay: &str,
    ask: &dyn Fn(&str) -> Result<Value, HttpFailure>,
) -> Result<Balance, String> {
    let usage = ask("/api/usage/token/").map_err(failure)?;
    let data = &usage["data"];
    let quota = data["user_quota"]["total_available"].as_f64().or_else(|| {
        (data["unlimited_quota"] != true)
            .then(|| data["total_available"].as_f64())
            .flatten()
    });
    let Some(quota) = quota else {
        return Err(l10n::text(
            "这个 key 不限额度，站点也没有开放账户余额",
            "This key has no spending limit and the site does not share the account balance.",
        ));
    };
    let per_unit = status["quota_per_unit"].as_f64().unwrap_or(500_000.0);
    let shown = status["quota_display_type"]
        .as_str()
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| {
            if status["display_in_currency"] == false {
                "TOKENS".into()
            } else {
                "USD".into()
            }
        });
    let (amount, currency) = match shown.as_str() {
        "CNY" => (
            quota / per_unit * status["usd_exchange_rate"].as_f64().unwrap_or(1.0),
            "CNY",
        ),
        "USD" => (quota / per_unit, "USD"),
        _ => {
            return Err(l10n::text(
                "这个中转站以 Token 计额度，没有金额",
                "This relay counts its quota in tokens, not money.",
            ))
        }
    };
    let name = status["system_name"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.chars().count() <= 40);
    Ok(Balance {
        kind: "newapi",
        label: name.unwrap_or(relay).to_owned(),
        amount,
        currency,
    })
}

/// OpenRouter: the account's credits left, else the key's remaining limit.
fn openrouter(ask: &dyn Fn(&str, bool) -> Result<Value, HttpFailure>) -> Result<Balance, String> {
    let credits = ask("https://openrouter.ai/api/v1/credits", true).ok();
    let left = credits.as_ref().and_then(|body| {
        Some(body["data"]["total_credits"].as_f64()? - body["data"]["total_usage"].as_f64()?)
    });
    let amount = match left {
        Some(left) => left,
        None => {
            let key = ask("https://openrouter.ai/api/v1/key", true).map_err(failure)?;
            key["data"]["limit_remaining"].as_f64().ok_or_else(|| {
                l10n::text(
                    "这个 OpenRouter key 不限额度，读不到账户余额",
                    "This OpenRouter key has no limit, and the account's credits could not be read.",
                )
            })?
        }
    };
    Ok(Balance {
        kind: "openrouter",
        label: "OpenRouter".into(),
        amount,
        currency: "USD",
    })
}

fn currency(unit: &str) -> Option<&'static str> {
    match unit.trim().to_ascii_uppercase().as_str() {
        "USD" | "$" => Some("USD"),
        "CNY" | "RMB" | "¥" => Some("CNY"),
        _ => None,
    }
}

fn unsupported_unit() -> String {
    l10n::text(
        "这个中转站的余额单位无法识别",
        "This relay reports its balance in a unit Agent HUD does not know.",
    )
}

fn failure(failure: HttpFailure) -> String {
    match failure {
        HttpFailure::Unreachable | HttpFailure::BadHeader => l10n::text(
            "中转站余额读取失败，请检查网络后重试",
            "The relay's balance could not be reached; check the connection.",
        ),
        other => other.message(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::fake::FakeHost;
    use std::sync::atomic::Ordering;

    const CLAUDE: &str = "/u/.claude/settings.json";

    fn claude_relay(host: FakeHost, base: &str) -> FakeHost {
        host.file(
            CLAUDE,
            &json!({"env": {"ANTHROPIC_BASE_URL": base, "ANTHROPIC_AUTH_TOKEN": "sk-relay"}})
                .to_string(),
        )
    }

    #[test]
    fn a_relay_is_listed_but_not_asked_before_consent() {
        let host = claude_relay(FakeHost::new("/u"), "https://relay.example.com/api");
        let reading = read(&host).unwrap();
        assert!(reading.billing.is_empty());
        assert!(host.asked.borrow().is_empty(), "nothing is sent");
        assert_eq!(
            reading.services,
            [
                json!({"client": "claude", "provider": "relay.example.com", "product": "relay", "consented": false})
            ]
        );
    }

    #[test]
    fn official_and_vendor_addresses_are_no_relays() {
        let host = claude_relay(FakeHost::new("/u"), "https://api.z.ai/api/anthropic")
            .env("ANTHROPIC_BASE_URL", "https://api.anthropic.com")
            .env("ANTHROPIC_API_KEY", "sk-ant");
        assert!(read(&host).unwrap().services.is_empty());
    }

    #[test]
    fn a_new_api_site_reports_the_balance_in_the_money_it_shows() {
        let status = |shown: &str| json!({"success": true, "data": {"system_name": "Relay One", "quota_per_unit": 500000, "quota_display_type": shown, "usd_exchange_rate": 7.0}});
        let usage = json!({"code": true, "data": {"total_available": 2_500_000, "total_used": 100, "unlimited_quota": false}});
        let host = claude_relay(
            FakeHost::new("/u"),
            "https://relay.example.com/api/anthropic",
        )
        .answer("https://relay.example.com/api/status", Ok(status("USD")))
        .answer(
            "https://relay.example.com/api/usage/token/",
            Ok(usage.clone()),
        );
        host.consent.store(true, Ordering::Release);
        let reading = read(&host).unwrap();
        assert_eq!(reading.billing.len(), 1);
        let row = &reading.billing[0];
        assert_eq!(row["vendor"], "claude");
        assert_eq!(row["consumerRoutes"], json!(["relay.example.com"]));
        assert_eq!(row["label"], "Relay One");
        assert_eq!(row["amount"], 5.0);
        assert_eq!(row["currency"], "USD");
        assert_eq!(reading.services[0]["kind"], "newapi");
        let asked = host.asked.borrow();
        assert_eq!(
            asked[0],
            ("https://relay.example.com/api/status".into(), None),
            "the status page is public"
        );
        assert_eq!(
            asked[1],
            (
                "https://relay.example.com/api/usage/token/".into(),
                Some("Bearer sk-relay".into())
            )
        );
        drop(asked);

        let cny = claude_relay(FakeHost::new("/u"), "https://relay.example.com")
            .answer("https://relay.example.com/api/status", Ok(status("CNY")))
            .answer(
                "https://relay.example.com/api/usage/token/",
                Ok(json!({"data": {"total_available": 0, "unlimited_quota": true, "user_quota": {"total_available": 1_000_000, "total_used": 5}}})),
            );
        cny.consent.store(true, Ordering::Release);
        let row = &read(&cny).unwrap().billing[0];
        assert_eq!(
            (row["amount"].as_f64(), row["currency"].as_str()),
            (Some(14.0), Some("CNY")),
            "the account's balance, in yuan"
        );
    }

    #[test]
    fn a_new_api_sites_price_list_is_read_in_the_money_it_shows() {
        let pricing = json!({"success": true, "group_ratio": {"default": 0.5, "vip": 0.3}, "data": [
            {"model_name": "claude-sonnet-4-5", "quota_type": 0, "model_ratio": 1.5, "completion_ratio": 5.0, "cache_ratio": 0.1, "create_cache_ratio": 1.25},
            {"model_name": "gpt-6-sol", "quota_type": 0, "model_ratio": 0.625, "completion_ratio": 8.0},
            {"model_name": "image-gen", "quota_type": 1, "model_price": 0.04, "model_ratio": 0}
        ]});
        let host = claude_relay(FakeHost::new("/u"), "https://relay.example.com")
            .answer(
                "https://relay.example.com/api/status",
                Ok(json!({"data": {"quota_per_unit": 500000, "quota_display_type": "USD"}})),
            )
            .answer("https://relay.example.com/api/pricing", Ok(pricing));
        host.consent.store(true, Ordering::Release);
        let reading = read(&host).unwrap();
        let service = &reading.services[0];
        assert_eq!(
            service["kind"], "newapi",
            "known even though the key's usage could not be read"
        );
        assert!(service["notice"].is_string());
        assert_eq!(service["prices"]["currency"], "USD");
        assert_eq!(
            service["prices"]["models"]["claude-sonnet-4-5"],
            json!([1.5, 7.5, 0.15000000000000002, 1.875])
        );
        assert_eq!(
            service["prices"]["models"]["gpt-6-sol"],
            json!([0.625, 5.0, 0.625, 0.625])
        );
        assert!(
            service["prices"]["models"]["image-gen"].is_null(),
            "priced per call"
        );
        let asked = host.asked.borrow();
        assert!(
            asked
                .iter()
                .any(|(url, auth)| url.ends_with("/api/pricing") && auth.is_none()),
            "the price list is public"
        );
    }

    #[test]
    fn a_sub2api_site_and_openrouter_report_what_is_left() {
        let sub2api = claude_relay(FakeHost::new("/u"), "https://pool.example.net").answer(
            "https://pool.example.net/v1/usage",
            Ok(json!({"remaining": 12.34, "unit": "USD", "is_active": true})),
        );
        sub2api.consent.store(true, Ordering::Release);
        let reading = read(&sub2api).unwrap();
        assert_eq!(reading.billing[0]["amount"], 12.34);
        assert_eq!(reading.services[0]["kind"], "sub2api");

        let openrouter = claude_relay(FakeHost::new("/u"), "https://openrouter.ai/api").answer(
            "https://openrouter.ai/api/v1/credits",
            Ok(json!({"data": {"total_credits": 20.0, "total_usage": 7.5}})),
        );
        openrouter.consent.store(true, Ordering::Release);
        let row = &read(&openrouter).unwrap().billing[0];
        assert_eq!(
            (row["amount"].as_f64(), row["label"].as_str()),
            (Some(12.5), Some("OpenRouter"))
        );
    }

    #[test]
    fn an_unknown_relay_says_so_without_a_balance() {
        let host = claude_relay(FakeHost::new("/u"), "https://odd.example.org");
        host.consent.store(true, Ordering::Release);
        let reading = read(&host).unwrap();
        assert!(reading.billing.is_empty());
        assert_eq!(
            reading.services[0]["notice"],
            "This relay's balance endpoint is not one Agent HUD knows yet."
        );
    }

    #[test]
    fn codex_providers_come_from_its_config() {
        let toml = r#"
model = "gpt-6-sol"
model_provider = "relay"

[model_providers.relay]
name = "My relay"
base_url = "https://relay.example.com/v1"
env_key = "RELAY_KEY"

[model_providers.inline]
base_url = "https://other.example.com/v1" # comment
experimental_bearer_token = "sk-inline"

[profiles.fast]
base_url = "https://ignored.example.com"
"#;
        assert_eq!(
            codex_providers(toml),
            [
                CodexProvider {
                    base_url: "https://relay.example.com/v1".into(),
                    env_key: Some("RELAY_KEY".into()),
                    bearer: None
                },
                CodexProvider {
                    base_url: "https://other.example.com/v1".into(),
                    env_key: None,
                    bearer: Some("sk-inline".into())
                },
            ]
        );
        let host = FakeHost::new("/u")
            .file("/u/.codex/config.toml", toml)
            .env("RELAY_KEY", "sk-env");
        let listed = read(&host).unwrap().services;
        let hosts: Vec<_> = listed
            .iter()
            .map(|s| (s["client"].clone(), s["provider"].clone()))
            .collect();
        assert_eq!(
            hosts,
            [
                (json!("codex"), json!("relay.example.com")),
                (json!("codex"), json!("other.example.com"))
            ]
        );
    }
}
