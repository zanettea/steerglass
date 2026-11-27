use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::{AsyncBufReadExt, StreamExt};
use k8s_openapi::api::core::v1::{Namespace, Node, Pod, Service};
use kube::Client;
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, LogParams, Patch, PatchParams};
use kube::runtime::{WatchStreamExt, watcher};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::core::commands::{dyn_api, kinds_for};
use crate::core::error::{Error, Result};
use crate::core::health::{self, Issue, Scan};
use crate::core::metrics::{parse_bytes, parse_cpu};
use crate::core::rows::{self, Row};
use crate::core::state::{AppState, ForwardInfo, KindRef, StreamHandle};
use crate::core::table::{self, Ev};

const FIELD_MANAGER: &str = "steerglass";
const FLUSH_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct ClusterSummary {
    pub version: String,
    pub nodes: usize,
    pub nodes_ready: usize,
    pub pods: Option<i64>,
    pub latency_ms: u128,
}

#[derive(Debug, Clone)]
pub struct NodeUsageEntry {
    pub name: String,
    pub cpu_used: Option<f64>,
    pub cpu_req: f64,
    pub cpu_alloc: f64,
    pub mem_used: Option<f64>,
    pub mem_req: f64,
    pub mem_alloc: f64,
    pub pods: usize,
}

#[derive(Debug, Clone)]
pub struct PodUsageEntry {
    pub namespace: String,
    pub name: String,
    pub node: Option<String>,
    pub cpu_used: Option<f64>,
    pub cpu_req: f64,
    pub cpu_lim: Option<f64>,
    pub mem_used: Option<f64>,
    pub mem_req: f64,
    pub mem_lim: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ClusterUtilization {
    pub metrics_available: bool,
    pub nodes: Vec<NodeUsageEntry>,
    pub pods: Vec<PodUsageEntry>,
}

#[derive(Debug, Clone)]
pub struct AvailablePort {
    pub port: u16,
    pub name: Option<String>,
    pub protocol: String,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum WatchUpdate {
    Upsert {
        generation: u64,
        context: String,
        rows: Vec<Row>,
    },
    Delete {
        generation: u64,
        context: String,
        uids: Vec<String>,
    },
    Synced {
        generation: u64,
        context: String,
        uids: Vec<String>,
    },
    Error {
        generation: u64,
        context: String,
        message: String,
    },
}

pub enum AppEvent {
    SummaryLoaded {
        context: String,
        result: std::result::Result<ClusterSummary, String>,
    },
    UtilizationLoaded {
        context: String,
        result: std::result::Result<ClusterUtilization, String>,
    },
    PortForwardStarted(std::result::Result<ForwardInfo, String>),
    PortsLoaded {
        context: String,
        namespace: String,
        kind: String,
        name: String,
        result: std::result::Result<Vec<AvailablePort>, String>,
    },
    IssuesLoaded {
        context: String,
        result: std::result::Result<Vec<Issue>, String>,
    },
    ScanFinished {
        context: String,
        result: std::result::Result<Scan, String>,
    },
    NamespacesLoaded {
        result: std::result::Result<Vec<String>, String>,
    },
    KindsLoaded {
        context: String,
        result: std::result::Result<Vec<KindRef>, String>,
    },
    Watch(WatchUpdate),
    YamlLoaded {
        context: String,
        name: String,
        namespace: Option<String>,
        kind: KindRef,
        result: std::result::Result<String, String>,
    },
    ContainersLoaded {
        context: String,
        namespace: String,
        pod: String,
        containers: Vec<String>,
    },
    LogLine {
        generation: u64,
        line: String,
    },
    LogError {
        generation: u64,
        message: String,
    },
    ActionDone {
        message: std::result::Result<String, String>,
        refresh_yaml: bool,
    },
}

pub fn builtin_kinds() -> Vec<KindRef> {
    let defs: &[(&str, &str, &str, &str, bool)] = &[
        ("", "v1", "Pod", "pods", true),
        ("apps", "v1", "Deployment", "deployments", true),
        ("apps", "v1", "StatefulSet", "statefulsets", true),
        ("apps", "v1", "DaemonSet", "daemonsets", true),
        ("apps", "v1", "ReplicaSet", "replicasets", true),
        ("batch", "v1", "Job", "jobs", true),
        ("batch", "v1", "CronJob", "cronjobs", true),
        ("", "v1", "Service", "services", true),
        ("networking.k8s.io", "v1", "Ingress", "ingresses", true),
        ("", "v1", "ConfigMap", "configmaps", true),
        ("", "v1", "Secret", "secrets", true),
        ("", "v1", "PersistentVolumeClaim", "persistentvolumeclaims", true),
        ("", "v1", "PersistentVolume", "persistentvolumes", false),
        ("", "v1", "Node", "nodes", false),
        ("", "v1", "Namespace", "namespaces", false),
        ("", "v1", "Event", "events", true),
        ("autoscaling", "v2", "HorizontalPodAutoscaler", "horizontalpodautoscalers", true),
        ("", "v1", "ServiceAccount", "serviceaccounts", true),
        ("", "v1", "Endpoints", "endpoints", true),
        ("", "v1", "ResourceQuota", "resourcequotas", true),
        ("", "v1", "LimitRange", "limitranges", true),
    ];
    defs.iter()
        .map(|(g, v, k, p, ns)| KindRef {
            group: (*g).into(),
            version: (*v).into(),
            kind: (*k).into(),
            plural: (*p).into(),
            namespaced: *ns,
            verbs: vec![],
        })
        .collect()
}

async fn node_counts(client: &Client) -> Result<(usize, usize)> {
    let req = http::Request::get("/api/v1/nodes?includeObject=None")
        .header(
            http::header::ACCEPT,
            "application/json;as=Table;v=v1;g=meta.k8s.io, application/json",
        )
        .body(Vec::new())
        .map_err(|e| Error::msg(e.to_string()))?;
    let table: Value = client.request(req).await?;
    let status_col = table["columnDefinitions"]
        .as_array()
        .and_then(|cols| cols.iter().position(|c| c["name"] == "Status"));
    let (Some(col), Some(rows)) = (status_col, table["rows"].as_array()) else {
        let list = Api::<Node>::all(client.clone()).list(&ListParams::default()).await?;
        let ready = list
            .items
            .iter()
            .filter(|n| {
                n.status
                    .as_ref()
                    .and_then(|s| s.conditions.as_ref())
                    .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
            })
            .count();
        return Ok((list.items.len(), ready));
    };
    let ready = rows
        .iter()
        .filter(|r| r["cells"][col].as_str().is_some_and(|s| s.split(',').any(|p| p == "Ready")))
        .count();
    Ok((rows.len(), ready))
}

pub fn spawn_summary(state: Arc<AppState>, context: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let fut = async {
            let client = state.client(&context).await?;
            let started = std::time::Instant::now();
            let info = client.apiserver_version().await?;
            let latency_ms = started.elapsed().as_millis();

            let pods_api: Api<Pod> = Api::all(client.clone());
            let one = ListParams::default().limit(1);
            let (nodes, pods) = tokio::join!(node_counts(&client), pods_api.list_metadata(&one));
            let (nodes_total, nodes_ready) = nodes.unwrap_or((0, 0));
            let pods = pods
                .ok()
                .map(|l| l.metadata.remaining_item_count.unwrap_or(0) + l.items.len() as i64);
            Ok::<_, Error>(ClusterSummary {
                version: info.git_version,
                nodes: nodes_total,
                nodes_ready,
                pods,
                latency_ms,
            })
        };
        let result = match tokio::time::timeout(Duration::from_secs(15), fut).await {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => {
                state.forget_client(&context);
                Err(e.to_string())
            }
            Err(_) => {
                state.forget_client(&context);
                Err("timed out".into())
            }
        };
        let _ = tx.send(AppEvent::SummaryLoaded { context, result });
    });
}

pub fn spawn_issues(state: Arc<AppState>, context: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let fut = async {
            let client = state.client(&context).await?;
            health::quick(&client).await
        };
        let result = match tokio::time::timeout(Duration::from_secs(30), fut).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("timed out".into()),
        };
        let _ = tx.send(AppEvent::IssuesLoaded { context, result });
    });
}

pub fn spawn_scan(state: Arc<AppState>, context: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = match tokio::time::timeout(Duration::from_secs(120), health::scan(&state, &context)).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("scan timed out".into()),
        };
        let _ = tx.send(AppEvent::ScanFinished { context, result });
    });
}

pub fn spawn_namespaces(state: Arc<AppState>, context: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let fut = async {
            let client = state.client(&context).await?;
            let api: Api<Namespace> = Api::all(client);
            match api.list_metadata(&ListParams::default()).await {
                Ok(list) => {
                    let mut names: Vec<String> = list.items.into_iter().filter_map(|n| n.metadata.name).collect();
                    names.sort();
                    Ok(names)
                }
                Err(kube::Error::Api(s)) if s.code == 403 => {
                    Ok(state.default_namespace(&context).into_iter().collect())
                }
                Err(e) => Err(Error::from(e)),
            }
        };
        let result = fut.await.map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::NamespacesLoaded { result });
    });
}

pub fn spawn_kinds(state: Arc<AppState>, context: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = kinds_for(&state, &context, false)
            .await
            .map(|arc| arc.as_ref().clone())
            .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::KindsLoaded { context, result });
    });
}

fn object_row(group: &str, kind: &str, mut obj: DynamicObject) -> Row {
    obj.metadata.managed_fields = None;
    let mut map = match obj.data {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    if let Ok(meta) = serde_json::to_value(&obj.metadata) {
        map.insert("metadata".into(), meta);
    }
    rows::to_row(group, kind, &Value::Object(map))
}

async fn forward_rows(
    mut rx: futures::channel::mpsc::Receiver<Ev>,
    generation: u64,
    context: String,
    tx: UnboundedSender<AppEvent>,
) {
    let mut upserts: HashMap<String, Row> = HashMap::new();
    let mut deletes: HashSet<String> = HashSet::new();
    let mut sent: HashMap<String, Row> = HashMap::new();
    let mut init_uids: Option<HashSet<String>> = None;
    let mut tick = tokio::time::interval(FLUSH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let flush = |upserts: &mut HashMap<String, Row>,
                 deletes: &mut HashSet<String>,
                 sent: &mut HashMap<String, Row>|
     -> bool {
        if !upserts.is_empty() {
            let rows: Vec<Row> = upserts.drain().map(|(_, r)| r).collect();
            for r in &rows {
                sent.insert(r.uid.clone(), r.clone());
            }
            if tx
                .send(AppEvent::Watch(WatchUpdate::Upsert {
                    generation,
                    context: context.clone(),
                    rows,
                }))
                .is_err()
            {
                return false;
            }
        }
        if !deletes.is_empty() {
            let uids: Vec<String> = deletes.drain().collect();
            for u in &uids {
                sent.remove(u);
            }
            if tx
                .send(AppEvent::Watch(WatchUpdate::Delete {
                    generation,
                    context: context.clone(),
                    uids,
                }))
                .is_err()
            {
                return false;
            }
        }
        true
    };

    loop {
        tokio::select! {
            ev = rx.next() => {
                let Some(ev) = ev else { break };
                match ev {
                    Ev::Init => init_uids = Some(HashSet::new()),
                    Ev::Row(row) => {
                        if let Some(set) = init_uids.as_mut() {
                            set.insert(row.uid.clone());
                        }
                        deletes.remove(&row.uid);
                        if sent.get(&row.uid) == Some(&row) {
                            upserts.remove(&row.uid);
                        } else {
                            upserts.insert(row.uid.clone(), row);
                        }
                    }
                    Ev::InitDone => {
                        if let Some(seen) = init_uids.as_ref() {
                            sent.retain(|uid, _| seen.contains(uid));
                        }
                        if !flush(&mut upserts, &mut deletes, &mut sent) { break }
                        let uids = init_uids.take().unwrap_or_default().into_iter().collect();
                        if tx.send(AppEvent::Watch(WatchUpdate::Synced {
                            generation,
                            context: context.clone(),
                            uids,
                        })).is_err() { break }
                    }
                    Ev::Delete(uid) => {
                        upserts.remove(&uid);
                        deletes.insert(uid);
                    }
                    Ev::Error(message) => {
                        if tx.send(AppEvent::Watch(WatchUpdate::Error {
                            generation,
                            context: context.clone(),
                            message,
                        })).is_err() { break }
                    }
                }
            }
            _ = tick.tick() => {
                if !flush(&mut upserts, &mut deletes, &mut sent) { break }
            }
        }
    }
}

pub fn spawn_watch(
    state: Arc<AppState>,
    generation: u64,
    context: String,
    kind: KindRef,
    namespace: Option<String>,
    tx: UnboundedSender<AppEvent>,
) -> tokio::task::AbortHandle {
    let handle = tokio::spawn(async move {
        let client = match state.client(&context).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(AppEvent::Watch(WatchUpdate::Error {
                    generation,
                    context,
                    message: e.to_string(),
                }));
                return;
            }
        };

        let ns = if kind.namespaced {
            namespace.as_deref().filter(|s| !s.is_empty())
        } else {
            None
        };
        let api = dyn_api(client.clone(), &kind, ns);
        let url = api.resource_url().to_string();

        let (mut ev_tx, ev_rx) = futures::channel::mpsc::channel(1024);
        let fwd = tokio::spawn(forward_rows(ev_rx, generation, context, tx));

        if rows::uses_table(&kind.group, &kind.kind) {
            let target = table::Target {
                client: api.clone().into_client(),
                url: &url,
                group: &kind.group,
                kind: &kind.kind,
                labels: None,
                fields: None,
            };
            if table::run(target, ev_tx.clone()).await == table::Outcome::Closed {
                fwd.abort();
                return;
            }
        }

        let mut stream = watcher(api, watcher::Config::default().page_size(500))
            .default_backoff()
            .boxed();
        while let Some(ev) = stream.next().await {
            use futures::SinkExt;
            let msg = match ev {
                Ok(watcher::Event::Init) => Ev::Init,
                Ok(watcher::Event::InitApply(obj)) | Ok(watcher::Event::Apply(obj)) => {
                    Ev::Row(object_row(&kind.group, &kind.kind, obj))
                }
                Ok(watcher::Event::InitDone) => Ev::InitDone,
                Ok(watcher::Event::Delete(obj)) => Ev::Delete(match obj.metadata.uid {
                    Some(uid) => uid,
                    None => format!(
                        "{}/{}",
                        obj.metadata.namespace.unwrap_or_default(),
                        obj.metadata.name.unwrap_or_default()
                    ),
                }),
                Err(e) => Ev::Error(match e {
                    watcher::Error::InitialListFailed(k)
                    | watcher::Error::WatchStartFailed(k)
                    | watcher::Error::WatchFailed(k) => Error::from(k).to_string(),
                    watcher::Error::WatchError(st) => format!("{} ({})", st.message, st.reason),
                    other => other.to_string(),
                }),
            };
            if ev_tx.send(msg).await.is_err() {
                break;
            }
        }
        fwd.abort();
    });
    handle.abort_handle()
}

pub fn spawn_yaml(
    state: Arc<AppState>,
    context: String,
    kind: KindRef,
    namespace: Option<String>,
    name: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let api = dyn_api(state.client(&context).await?, &kind, namespace.as_deref());
            let obj = api.get(&name).await?;
            let mut v = serde_json::to_value(obj)?;
            if let Some(meta) = v.get_mut("metadata").and_then(|m| m.as_object_mut()) {
                meta.remove("managedFields");
            }
            Ok::<_, Error>(serde_yaml::to_string(&v)?)
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::YamlLoaded {
            context,
            name,
            namespace,
            kind,
            result: res,
        });
    });
}

pub fn spawn_containers(
    state: Arc<AppState>,
    context: String,
    namespace: String,
    pod_name: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let Ok(client) = state.client(&context).await else {
            return;
        };
        let api: Api<Pod> = Api::namespaced(client, &namespace);
        if let Ok(pod) = api.get(&pod_name).await {
            let mut names = Vec::new();
            if let Some(spec) = pod.spec {
                for c in spec.containers {
                    names.push(c.name);
                }
                for c in spec.init_containers.unwrap_or_default() {
                    names.push(c.name);
                }
                for c in spec.ephemeral_containers.unwrap_or_default() {
                    names.push(c.name);
                }
            }
            let _ = tx.send(AppEvent::ContainersLoaded {
                context,
                namespace,
                pod: pod_name,
                containers: names,
            });
        }
    });
}

pub fn spawn_logs(
    state: Arc<AppState>,
    generation: u64,
    context: String,
    namespace: String,
    pod: String,
    container: Option<String>,
    follow: bool,
    timestamps: bool,
    tx: UnboundedSender<AppEvent>,
) -> tokio::task::AbortHandle {
    let handle = tokio::spawn(async move {
        let client = match state.client(&context).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(AppEvent::LogError {
                    generation,
                    message: e.to_string(),
                });
                return;
            }
        };
        let api: Api<Pod> = Api::namespaced(client, &namespace);
        let lp = LogParams {
            container,
            follow,
            timestamps,
            tail_lines: Some(500),
            ..Default::default()
        };
        match api.log_stream(&pod, &lp).await {
            Ok(stream) => {
                let mut lines = stream.lines();
                while let Some(res) = lines.next().await {
                    match res {
                        Ok(line) => {
                            if tx.send(AppEvent::LogLine { generation, line }).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(AppEvent::LogError {
                                generation,
                                message: e.to_string(),
                            });
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::LogError {
                    generation,
                    message: Error::from(e).to_string(),
                });
            }
        }
    });
    handle.abort_handle()
}

pub fn spawn_delete(
    state: Arc<AppState>,
    context: String,
    kind: KindRef,
    namespace: Option<String>,
    name: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let api = dyn_api(state.client(&context).await?, &kind, namespace.as_deref());
            api.delete(&name, &DeleteParams::default()).await?;
            Ok::<_, Error>(format!("Deleted {} {}", kind.kind, name))
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::ActionDone {
            message: res,
            refresh_yaml: false,
        });
    });
}

pub fn spawn_restart(
    state: Arc<AppState>,
    context: String,
    kind: KindRef,
    namespace: Option<String>,
    name: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let api = dyn_api(state.client(&context).await?, &kind, namespace.as_deref());
            let now = chrono::Utc::now().to_rfc3339();
            let patch = json!({"spec": {"template": {"metadata": {"annotations": {"kubectl.kubernetes.io/restartedAt": now}}}}});
            api.patch(
                &name,
                &PatchParams {
                    field_manager: Some(FIELD_MANAGER.into()),
                    ..Default::default()
                },
                &Patch::Merge(&patch),
            )
            .await?;
            Ok::<_, Error>(format!("Restarted {} {}", kind.kind, name))
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::ActionDone {
            message: res,
            refresh_yaml: false,
        });
    });
}

pub fn spawn_scale(
    state: Arc<AppState>,
    context: String,
    kind: KindRef,
    namespace: Option<String>,
    name: String,
    replicas: i32,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let api = dyn_api(state.client(&context).await?, &kind, namespace.as_deref());
            let patch = json!({"spec": {"replicas": replicas}});
            api.patch(
                &name,
                &PatchParams {
                    field_manager: Some(FIELD_MANAGER.into()),
                    ..Default::default()
                },
                &Patch::Merge(&patch),
            )
            .await?;
            Ok::<_, Error>(format!("Scaled {} {} to {}", kind.kind, name, replicas))
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::ActionDone {
            message: res,
            refresh_yaml: false,
        });
    });
}

pub fn spawn_cordon(
    state: Arc<AppState>,
    context: String,
    name: String,
    cordon: bool,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let kind = KindRef {
                group: String::new(),
                version: "v1".into(),
                kind: "Node".into(),
                plural: "nodes".into(),
                namespaced: false,
                verbs: vec![],
            };
            let api = dyn_api(state.client(&context).await?, &kind, None);
            let patch = json!({"spec": {"unschedulable": cordon}});
            api.patch(
                &name,
                &PatchParams {
                    field_manager: Some(FIELD_MANAGER.into()),
                    ..Default::default()
                },
                &Patch::Merge(&patch),
            )
            .await?;
            let verb = if cordon { "Cordoned" } else { "Uncordoned" };
            Ok::<_, Error>(format!("{verb} node {name}"))
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::ActionDone {
            message: res,
            refresh_yaml: false,
        });
    });
}

pub fn spawn_suspend_cronjob(
    state: Arc<AppState>,
    context: String,
    namespace: String,
    name: String,
    suspend: bool,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let kind = KindRef {
                group: "batch".into(),
                version: "v1".into(),
                kind: "CronJob".into(),
                plural: "cronjobs".into(),
                namespaced: true,
                verbs: vec![],
            };
            let api = dyn_api(state.client(&context).await?, &kind, Some(&namespace));
            let patch = json!({"spec": {"suspend": suspend}});
            api.patch(
                &name,
                &PatchParams {
                    field_manager: Some(FIELD_MANAGER.into()),
                    ..Default::default()
                },
                &Patch::Merge(&patch),
            )
            .await?;
            let verb = if suspend { "Suspended" } else { "Resumed" };
            Ok::<_, Error>(format!("{verb} CronJob {name}"))
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::ActionDone {
            message: res,
            refresh_yaml: false,
        });
    });
}

pub fn spawn_replace_yaml(
    state: Arc<AppState>,
    context: String,
    kind: KindRef,
    namespace: Option<String>,
    name: String,
    yaml_text: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let value: Value = serde_yaml::from_str(&yaml_text)?;
            let api = dyn_api(state.client(&context).await?, &kind, namespace.as_deref());
            api.patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&value),
            )
            .await?;
            Ok::<_, Error>(format!("Applied {} {}", kind.kind, name))
        }
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::ActionDone {
            message: res,
            refresh_yaml: true,
        });
    });
}

pub fn spawn_utilization(
    state: Arc<AppState>,
    context: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = fetch_utilization(state, &context)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::UtilizationLoaded {
            context,
            result: res,
        });
    });
}

async fn fetch_utilization(state: Arc<AppState>, context: &str) -> Result<ClusterUtilization> {
    let client = state.client(context).await?;
    let get_req = |path: &str| -> Result<http::Request<Vec<u8>>> {
        http::Request::builder()
            .method("GET")
            .uri(path)
            .body(Vec::new())
            .map_err(|e| Error::msg(e.to_string()))
    };

    let (node_metrics_res, pod_metrics_res, nodes_res, pods_res) = tokio::join!(
        client.request::<Value>(get_req("/apis/metrics.k8s.io/v1beta1/nodes")?),
        client.request::<Value>(get_req("/apis/metrics.k8s.io/v1beta1/pods")?),
        client.request::<Value>(get_req("/api/v1/nodes")?),
        client.request::<Value>(get_req("/api/v1/pods")?),
    );

    let metrics_available = node_metrics_res.is_ok() || pod_metrics_res.is_ok();
    let mut node_used: HashMap<String, (f64, f64)> = HashMap::new();
    if let Ok(nm) = node_metrics_res {
        for item in nm["items"].as_array().into_iter().flatten() {
            let name = item["metadata"]["name"].as_str().unwrap_or_default().to_string();
            let cpu = item["usage"]["cpu"].as_str().and_then(parse_cpu).unwrap_or(0.0);
            let mem = item["usage"]["memory"].as_str().and_then(parse_bytes).unwrap_or(0.0);
            node_used.insert(name, (cpu, mem));
        }
    }

    let mut pod_used: HashMap<(String, String), (f64, f64)> = HashMap::new();
    if let Ok(pm) = pod_metrics_res {
        for item in pm["items"].as_array().into_iter().flatten() {
            let ns = item["metadata"]["namespace"].as_str().unwrap_or_default().to_string();
            let name = item["metadata"]["name"].as_str().unwrap_or_default().to_string();
            let mut cpu = 0.0;
            let mut mem = 0.0;
            for c in item["containers"].as_array().into_iter().flatten() {
                cpu += c["usage"]["cpu"].as_str().and_then(parse_cpu).unwrap_or(0.0);
                mem += c["usage"]["memory"].as_str().and_then(parse_bytes).unwrap_or(0.0);
            }
            pod_used.insert((ns, name), (cpu, mem));
        }
    }

    let pods_val = pods_res?;
    let mut node_reqs: HashMap<String, (f64, f64, usize)> = HashMap::new();
    let mut pods = Vec::new();
    for p in pods_val["items"].as_array().into_iter().flatten() {
        let ns = p["metadata"]["namespace"].as_str().unwrap_or_default().to_string();
        let name = p["metadata"]["name"].as_str().unwrap_or_default().to_string();
        let phase = p["status"]["phase"].as_str().unwrap_or_default();
        let active = matches!(phase, "Running" | "Pending");
        let node = p["spec"]["nodeName"].as_str().map(ToString::to_string);

        let mut req_cpu = 0.0;
        let mut req_mem = 0.0;
        let mut lim_cpu: Option<f64> = Some(0.0);
        let mut lim_mem: Option<f64> = Some(0.0);
        for c in p["spec"]["containers"].as_array().into_iter().flatten() {
            req_cpu += c["resources"]["requests"]["cpu"]
                .as_str()
                .and_then(parse_cpu)
                .unwrap_or(0.0);
            req_mem += c["resources"]["requests"]["memory"]
                .as_str()
                .and_then(parse_bytes)
                .unwrap_or(0.0);
            match c["resources"]["limits"]["cpu"].as_str().and_then(parse_cpu) {
                Some(v) => {
                    if let Some(acc) = lim_cpu.as_mut() {
                        *acc += v;
                    }
                }
                None => lim_cpu = None,
            }
            match c["resources"]["limits"]["memory"].as_str().and_then(parse_bytes) {
                Some(v) => {
                    if let Some(acc) = lim_mem.as_mut() {
                        *acc += v;
                    }
                }
                None => lim_mem = None,
            }
        }

        if active {
            if let Some(n) = &node {
                let entry = node_reqs.entry(n.clone()).or_default();
                entry.0 += req_cpu;
                entry.1 += req_mem;
                entry.2 += 1;
            }
        }

        let used = pod_used.get(&(ns.clone(), name.clone()));
        pods.push(PodUsageEntry {
            namespace: ns,
            name,
            node,
            cpu_used: used.map(|u| u.0),
            cpu_req: req_cpu,
            cpu_lim: lim_cpu,
            mem_used: used.map(|u| u.1),
            mem_req: req_mem,
            mem_lim: lim_mem,
        });
    }

    let nodes_val = nodes_res?;
    let mut nodes = Vec::new();
    for n in nodes_val["items"].as_array().into_iter().flatten() {
        let name = n["metadata"]["name"].as_str().unwrap_or_default().to_string();
        let cpu_alloc = n["status"]["allocatable"]["cpu"]
            .as_str()
            .and_then(parse_cpu)
            .unwrap_or(0.0);
        let mem_alloc = n["status"]["allocatable"]["memory"]
            .as_str()
            .and_then(parse_bytes)
            .unwrap_or(0.0);
        let (cpu_req, mem_req, pod_count) = node_reqs.get(&name).copied().unwrap_or_default();
        let used = node_used.get(&name);
        nodes.push(NodeUsageEntry {
            name,
            cpu_used: used.map(|u| u.0),
            cpu_req,
            cpu_alloc,
            mem_used: used.map(|u| u.1),
            mem_req,
            mem_alloc,
            pods: pod_count,
        });
    }

    Ok(ClusterUtilization {
        metrics_available,
        nodes,
        pods,
    })
}

async fn resolve_service_port(api_pods: &Api<Pod>, svc: &Service, port: u16) -> Result<(String, u16)> {
    let spec = svc.spec.as_ref().ok_or_else(|| Error::msg("service has no spec"))?;
    let selector = spec
        .selector
        .as_ref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::msg("service has no selector"))?;
    let sel = selector
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    let pods = api_pods.list(&ListParams::default().labels(&sel)).await?;
    let pod = pods
        .items
        .iter()
        .find(|p| {
            p.status
                .as_ref()
                .and_then(|s| s.conditions.as_ref())
                .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
        })
        .or(pods.items.first())
        .ok_or_else(|| Error::msg("no pods behind this service"))?;
    let svc_port = spec
        .ports
        .as_ref()
        .and_then(|ps| ps.iter().find(|p| p.port == port as i32))
        .ok_or_else(|| Error::msg(format!("service has no port {port}")))?;
    use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
    let target = match &svc_port.target_port {
        None => port,
        Some(IntOrString::Int(n)) => *n as u16,
        Some(IntOrString::String(named)) => pod
            .spec
            .as_ref()
            .and_then(|s| {
                s.containers
                    .iter()
                    .flat_map(|c| c.ports.iter().flatten())
                    .find(|p| p.name.as_deref() == Some(named.as_str()))
            })
            .map(|p| p.container_port as u16)
            .ok_or_else(|| Error::msg(format!("named port {named} not found")))?,
    };
    Ok((pod.metadata.name.clone().unwrap_or_default(), target))
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_port_forward(
    state: Arc<AppState>,
    context: String,
    namespace: String,
    kind: String,
    name: String,
    port: u16,
    local_port: Option<u16>,
    bind_addr: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = start_port_forward(
            state,
            context,
            namespace,
            kind,
            name,
            port,
            local_port,
            bind_addr,
        )
        .await
        .map_err(|e| e.to_string());
        let _ = tx.send(AppEvent::PortForwardStarted(res));
    });
}

#[allow(clippy::too_many_arguments)]
async fn start_port_forward(
    state: Arc<AppState>,
    context: String,
    namespace: String,
    kind: String,
    name: String,
    port: u16,
    local_port: Option<u16>,
    bind_addr: String,
) -> Result<ForwardInfo> {
    let target = format!("{}/{}", kind.to_lowercase(), name);
    let running = state
        .streams
        .lock()
        .values()
        .filter_map(|h| h.forward.clone())
        .find(|f| {
            f.context == context
                && f.namespace == namespace
                && f.target == target
                && f.remote_port == port
                && local_port.is_none_or(|l| l == f.local_port)
        });
    if let Some(f) = running {
        return Ok(f);
    }

    let client = state.client(&context).await?;
    let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
    let (pod, remote_port) = if kind == "Service" {
        let svc = Api::<Service>::namespaced(client.clone(), &namespace)
            .get(&name)
            .await?;
        resolve_service_port(&pods, &svc, port).await?
    } else {
        (name, port)
    };

    let addr: std::net::IpAddr = bind_addr
        .parse()
        .map_err(|_| Error::msg(format!("invalid bind address: {bind_addr}")))?;
    let listener = tokio::net::TcpListener::bind((addr, local_port.unwrap_or(0))).await?;
    let bound = listener.local_addr()?.port();
    let id = state.next_id();
    let info = ForwardInfo {
        id,
        context,
        namespace,
        target,
        remote_port: port,
        local_port: bound,
    };

    let state_drop = state.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let pods = pods.clone();
            let pod = pod.clone();
            tokio::spawn(async move {
                let Ok(mut pf) = pods.portforward(&pod, &[remote_port]).await else {
                    return;
                };
                let Some(mut upstream) = pf.take_stream(remote_port) else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
                drop(upstream);
                let _ = pf.join().await;
            });
        }
        state_drop.streams.lock().remove(&id);
    });

    state.streams.lock().insert(
        id,
        StreamHandle {
            abort: task.abort_handle(),
            stdin: None,
            resize: None,
            forward: Some(info.clone()),
        },
    );
    Ok(info)
}

pub fn list_port_forwards(state: &AppState) -> Vec<ForwardInfo> {
    let mut v: Vec<ForwardInfo> = state
        .streams
        .lock()
        .values()
        .filter_map(|h| h.forward.clone())
        .collect();
    v.sort_by_key(|f| f.id);
    v
}

pub fn stop_port_forward(state: &AppState, id: u64) -> Option<ForwardInfo> {
    if let Some(handle) = state.streams.lock().remove(&id) {
        handle.abort.abort();
        return handle.forward;
    }
    None
}

pub fn spawn_available_ports(
    state: Arc<AppState>,
    context: String,
    namespace: String,
    kind: String,
    name: String,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let res = async {
            let client = state.client(&context).await?;
            let mut out = Vec::new();
            if kind == "Service" {
                let svc = Api::<Service>::namespaced(client, &namespace).get(&name).await?;
                if let Some(ports) = svc.spec.and_then(|s| s.ports) {
                    for p in ports {
                        if let Ok(port) = u16::try_from(p.port) {
                            let target = p.target_port.map(|tp| match tp {
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(n) => {
                                    format!("-> {n}")
                                }
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(s) => {
                                    format!("-> {s}")
                                }
                            });
                            out.push(AvailablePort {
                                port,
                                name: p.name,
                                protocol: p.protocol.unwrap_or_else(|| "TCP".into()),
                                detail: target,
                            });
                        }
                    }
                }
            } else {
                let pod = Api::<Pod>::namespaced(client, &namespace).get(&name).await?;
                if let Some(containers) = pod.spec.map(|s| s.containers) {
                    for c in containers {
                        for p in c.ports.into_iter().flatten() {
                            if let Ok(port) = u16::try_from(p.container_port) {
                                out.push(AvailablePort {
                                    port,
                                    name: p.name,
                                    protocol: p.protocol.unwrap_or_else(|| "TCP".into()),
                                    detail: Some(c.name.clone()),
                                });
                            }
                        }
                    }
                }
            }
            Ok::<_, Error>(out)
        }
        .await
        .map_err(|e| e.to_string());

        let _ = tx.send(AppEvent::PortsLoaded {
            context,
            namespace,
            kind,
            name,
            result: res,
        });
    });
}
