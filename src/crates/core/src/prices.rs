//! The price page (docs/proposals/no-plan-usage.md §3): every model the
//! selected logs used in the last 30 days, what it costs and where that price
//! comes from, and the relays and local hosts their calls went through, with
//! what the owner set for each.
use crate::{pricing, AgentHudCore};
use agent_hud_agents::endpoints::{host_kind, HostKind};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const DAYS_30: i64 = 30 * 86_400_000;

/// What kind of route a call went through, as the page groups them.
fn route_kind(route: &str) -> &'static str {
    if pricing::is_local_route(route) {
        "local-server"
    } else if matches!(route, "bedrock" | "vertex" | "foundry") {
        "cloud"
    } else if route.contains('.') || route.contains(':') {
        match host_kind(route) {
            HostKind::Local => "local",
            _ => "relay",
        }
    } else {
        "provider"
    }
}

impl AgentHudCore {
    pub(crate) fn price_list(&mut self) -> Result<Value, String> {
        self.ensure_usage_projection();
        self.prepare_meters()?;
        let now = chrono::Utc::now().timestamp_millis();
        let Some(view) = self.meters.view.as_ref() else {
            return Ok(json!({"kind":"prices","models":[],"relays":[],"localHosts":[]}));
        };
        let settings = &self.settings;
        let mut relays = BTreeMap::<String, Value>::new();
        let mut locals = BTreeMap::<String, Value>::new();
        let models = view
            .models_since(now - DAYS_30)
            .into_iter()
            .filter(|(_, tokens)| *tokens > 0)
            .map(|((source, model), tokens)| {
                let id = format!("{source}-model:{model}");
                let route = model.split_once('#').map(|(_, route)| route);
                let kind = route.map(route_kind);
                let quote = pricing::price_quote(&id);
                if let (Some(route), Some(kind)) = (route, kind) {
                    if kind == "relay" {
                        relays.entry(route.to_owned()).or_insert_with(|| {
                            json!({"host": route, "multiplier": settings["relayMultipliers"][route],
                                   "listed": quote.as_ref().is_some_and(|(origin, _, _)| *origin == pricing::Origin::Relay)})
                        });
                    }
                    if kind == "local" {
                        locals
                            .entry(route.to_owned())
                            .or_insert_with(|| json!({"host": route, "kind": settings["localKinds"][route]}));
                    }
                }
                json!({
                    "id": id,
                    "source": source,
                    "client": agent_hud_agents::by_id(&source).map_or(source.as_str(), |agent| agent.name),
                    "model": model,
                    "name": crate::naming::model_names::consumer_name(&id),
                    "route": route,
                    "routeKind": kind,
                    "tokens": tokens,
                    "price": quote.map(|(origin, currency, rates)| json!({
                        "origin": origin.name(),
                        "currency": currency,
                        "input": rates[0],
                        "output": rates[1],
                        "cacheRead": rates[2],
                        "cacheWrite": rates[3],
                    })),
                    "custom": settings["customPrices"][model.as_str()],
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "kind": "prices",
            "models": models,
            "relays": relays.into_values().collect::<Vec<_>>(),
            "localHosts": locals.into_values().collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_grouped_by_what_they_reach() {
        assert_eq!(route_kind("ollama"), "local-server");
        assert_eq!(route_kind("bedrock"), "cloud");
        assert_eq!(route_kind("relay.example.com"), "relay");
        assert_eq!(route_kind("localhost:4000"), "local");
        assert_eq!(route_kind("192.168.1.9:8000"), "local");
        assert_eq!(route_kind("openrouter"), "provider");
    }
}
