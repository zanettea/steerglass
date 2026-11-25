use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::{AsyncBufReadExt, StreamExt};
use k8s_openapi::api::core::v1::{Namespace, Node, Pod};
use kube::Client;
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, LogParams, Patch, PatchParams};
use kube::runtime::{WatchStreamExt, watcher};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::core::commands::{dyn_api, kinds_for};
use crate::core::error::{Error, Result};
use crate::core::health::{self, Issue, Scan};
use crate::core::rows::{self, Row};
use crate::core::state::{AppState, KindRef};
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
