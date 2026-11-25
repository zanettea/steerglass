//! Reuses Steerglass's core Rust modules from `src-tauri/src/` without pulling in Tauri or WebKit.

#[path = "../../src-tauri/src/console.rs"]
pub mod console;
#[path = "../../src-tauri/src/error.rs"]
pub mod error;
#[path = "../../src-tauri/src/health.rs"]
pub mod health;
#[path = "../../src-tauri/src/kubeconfig.rs"]
pub mod kubeconfig;
#[path = "../../src-tauri/src/rows.rs"]
pub mod rows;
#[path = "../../src-tauri/src/sandbox.rs"]
pub mod sandbox;
#[path = "../../src-tauri/src/state.rs"]
pub mod state;
#[path = "../../src-tauri/src/table.rs"]
pub mod table;

pub mod ai {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    pub struct AiSettings {
        pub enabled: bool,
        pub provider: String,
        pub model: String,
        pub base_url: Option<String>,
        pub allow_prod: bool,
    }
}

pub mod metrics {
    #[derive(Default)]
    pub struct Metrics;

    pub fn parse_cpu(q: &str) -> Option<f64> {
        let q = q.trim();
        let (num, scale) = match q.char_indices().last()? {
            (i, 'n') => (&q[..i], 1e-9),
            (i, 'u') => (&q[..i], 1e-6),
            (i, 'm') => (&q[..i], 1e-3),
            (i, 'k') => (&q[..i], 1e3),
            _ => (q, 1.0),
        };
        num.parse::<f64>().ok().map(|n| n * scale)
    }

    pub fn parse_bytes(q: &str) -> Option<f64> {
        let q = q.trim();
        const BIN: [(&str, f64); 6] = [
            ("Ki", 1024.0),
            ("Mi", 1048576.0),
            ("Gi", 1073741824.0),
            ("Ti", 1099511627776.0),
            ("Pi", 1125899906842624.0),
            ("Ei", 1152921504606846976.0),
        ];
        for (suffix, mult) in BIN {
            if let Some(n) = q.strip_suffix(suffix) {
                return n.parse::<f64>().ok().map(|n| n * mult);
            }
        }
        const DEC: [(&str, f64); 7] = [
            ("k", 1e3),
            ("K", 1e3),
            ("M", 1e6),
            ("G", 1e9),
            ("T", 1e12),
            ("P", 1e15),
            ("E", 1e18),
        ];
        for (suffix, mult) in DEC {
            if let Some(n) = q.strip_suffix(suffix) {
                return n.parse::<f64>().ok().map(|n| n * mult);
            }
        }
        if let Some(n) = q.strip_suffix('m') {
            return n.parse::<f64>().ok().map(|n| n / 1000.0);
        }
        q.parse::<f64>().ok()
    }
}

pub mod commands {
    use std::sync::Arc;

    use kube::Client;
    use kube::api::{Api, DynamicObject};
    use kube::discovery::{Discovery, Scope};

    use crate::error::Result;
    use crate::state::{AppState, KindRef};

    pub fn dyn_api(client: Client, kind: &KindRef, namespace: Option<&str>) -> Api<DynamicObject> {
        let ar = kind.api_resource();
        match (kind.namespaced, namespace.filter(|n| !n.is_empty())) {
            (true, Some(ns)) => Api::namespaced_with(client, ns, &ar),
            _ => Api::all_with(client, &ar),
        }
    }

    async fn run_discovery(client: Client) -> Result<Vec<KindRef>> {
        let discovery = match Discovery::new(client.clone()).run_aggregated().await {
            Ok(d) => d,
            Err(_) => Discovery::new(client).run().await?,
        };
        let mut out = Vec::new();
        for group in discovery.groups() {
            for (ar, caps) in group.recommended_resources() {
                if !caps.supports_operation("list") || !caps.supports_operation("watch") {
                    continue;
                }
                out.push(KindRef {
                    group: ar.group.clone(),
                    version: ar.version.clone(),
                    kind: ar.kind.clone(),
                    plural: ar.plural.clone(),
                    namespaced: matches!(caps.scope, Scope::Namespaced),
                    verbs: caps.operations.clone(),
                });
            }
        }
        out.retain(|k| !(k.group == "events.k8s.io" && k.kind == "Event"));
        out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.group.cmp(&b.group)));
        Ok(out)
    }

    pub async fn kinds_for(state: &AppState, context: &str, refresh: bool) -> Result<Arc<Vec<KindRef>>> {
        if !refresh {
            if let Some(k) = state.discovery.lock().get(context) {
                return Ok(k.clone());
            }
        }
        let client = state.client(context).await?;
        let kinds = Arc::new(run_discovery(client).await?);
        state.discovery.lock().insert(context.to_string(), kinds.clone());
        Ok(kinds)
    }
}
