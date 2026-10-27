//! dev-box sidecar（M0）
//!
//! 形态：DBX 启动的子进程，stdin/stdout 上跑 JSONL。
//! 职责：协议翻译 + 请求分发。**不在这里写业务逻辑**，业务在 devbox-core。
//!
//! M0 的边界（写清楚，免得后面误以为已完成）：
//! - 安装是**假实现**：只推进状态机 + 发进度事件，不下载任何东西。
//!   目的是先把"异步任务 + 断线重连 + 事件流"这条链路跑通。
//! - 还没有 daemon 模式（`detached` / `autostart` 依赖它），M2 才做。
//! - catalog 还是常量表，M1 接真实 YAML 与签名校验。

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use devbox_core::job::{Job, Phase};
use devbox_core::platform::PlatformKey;
use devbox_core::READY_MARKER;
use devbox_core::CORE_VERSION;
use devbox_engine::catalog::Catalog;
use devbox_engine::{install, InstallSpec};
use serde_json::{json, Value};

type SharedWriter = Arc<Mutex<std::io::Stdout>>;
type JobTable = Arc<Mutex<HashMap<String, Job>>>;

static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

fn main() {
    let out: SharedWriter = Arc::new(Mutex::new(std::io::stdout()));
    let jobs: JobTable = Arc::new(Mutex::new(HashMap::new()));
    let platform = PlatformKey::current();

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                emit_error(&out, Value::Null, -32700, &format!("parse error: {e}"));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match dispatch(method, &params, &jobs, &out, platform) {
            Ok(result) => emit_result(&out, id, result),
            Err((code, message)) => emit_error(&out, id, code, &message),
        }
    }
}

fn dispatch(
    method: &str,
    params: &Value,
    jobs: &JobTable,
    out: &SharedWriter,
    platform: PlatformKey,
) -> Result<Value, (i32, String)> {
    match method {
        // 宿主握手：协议版本 + 能力 + 身份，字段名按官方 sidecar 协议
        "plugin/initialize" => Ok(json!({
            "protocolVersion": 1,
            "capabilities": { "jsonl": true, "framed": false },
            "plugin": { "id": "com.devbox.manager", "version": env!("CARGO_PKG_VERSION") },
        })),

        "devbox/ping" => Ok(json!({
            "ok": true,
            "coreVersion": CORE_VERSION,
            "platform": platform.key(),
            "mode": "stdio",
        })),

        "devbox/catalog" => {
            // 优先返回真实目录；读不到才退回演示数据（并在 rev 上标出来）
            match catalog_payload(&data_root(), platform) {
                Ok((revision, components)) => Ok(json!({
                    "revision": revision,
                    "components": components,
                    "shimDir": data_root().join("bin").display().to_string(),
                    "shims": count_shims(&data_root()),
                })),
                Err(message) => Ok(json!({
                    "revision": format!("mock ({message})"),
                    "components": mock_catalog(platform),
                })),
            }
        }

        "devbox/install" => {
            let component = params
                .get("component")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing component".to_string()))?;
            let version = params
                .get("version")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing version".to_string()))?;

            let job_id = format!("job-{}", JOB_SEQ.fetch_add(1, Ordering::Relaxed));
            let job = Job::new(job_id.clone(), component, version);
            jobs.lock().unwrap().insert(job_id.clone(), job);

            // 有真实配方就走真实下载；没有（比如 MySQL 还没配镜像）就走演示路径。
            // 这样 UI 的行为是一致的，只是数据真假不同。
            match resolve_spec(component, version, platform) {
                Ok(spec) => spawn_real_install(job_id.clone(), spec, jobs.clone(), out.clone()),
                Err(message) => {
                    // 开发时想走演示动画可以设 DEVBOX_FAKE=1；默认如实失败
                    if std::env::var("DEVBOX_FAKE").as_deref() == Ok("1") {
                        spawn_fake_install(job_id.clone(), jobs.clone(), out.clone());
                    } else {
                        spawn_catalog_failure(job_id.clone(), message, jobs.clone(), out.clone());
                    }
                }
            }
            Ok(json!({ "jobId": job_id }))
        }

        // 切换当前版本：只改指针，不重新下载
        "devbox/activate" => {
            let component = params
                .get("component")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing component".to_string()))?;
            let version = params
                .get("version")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing version".to_string()))?;
            match devbox_engine::activate(&data_root(), component, version, &platform.key()) {
                Ok(a) => {
                    // 入口是幂等的，切完顺手刷新一次；失败不影响激活本身
                    let shims = refresh_shims(&data_root(), platform).unwrap_or(0);
                    Ok(json!({
                        "ok": true,
                        "component": a.component,
                        "version": a.version,
                        "previous": a.previous,
                        "shims": shims,
                    }))
                }
                Err(e) => Err((-32001, e.to_string())),
            }
        }

        // 真实状态：磁盘占用、计数、目录路径。界面不再自己编。
        "devbox/state" => {
            let root = data_root();
            let (total, by_component) = store_usage(&root);
            let catalog = catalog_path().and_then(|p| Catalog::from_file(&p).ok());
            let (revision, comp_count, unverified) = match &catalog {
                Some(c) => (
                    c.revision.clone(),
                    c.components.len(),
                    c.components
                        .iter()
                        .map(|comp| {
                            comp.versions
                                .keys()
                                .filter(|v| c.install_spec(&comp.id, v, &platform.key()).is_err())
                                .count()
                        })
                        .sum::<usize>(),
                ),
                None => (String::new(), 0, 0),
            };
            let mut installed_count = 0usize;
            let mut active_count = 0usize;
            if let Some(c) = &catalog {
                for comp in &c.components {
                    installed_count += installed_versions(&root, &comp.id, &platform.key()).len();
                    if devbox_engine::current(&root, &comp.id).is_some() {
                        active_count += 1;
                    }
                }
            }
            Ok(json!({
                "data": root.display().to_string(),
                "paths": {
                    "store": root.join("store").display().to_string(),
                    "current": root.join("current").display().to_string(),
                    "bin": root.join("bin").display().to_string(),
                },
                "store": { "totalBytes": total, "byComponent": by_component },
                "counts": {
                    "installed": installed_count,
                    "active": active_count,
                    "shims": count_shims(&root),
                },
                "catalog": { "revision": revision, "components": comp_count, "unverified": unverified },
                "docker": { "available": in_path("docker"), "version": Value::Null },
                "persist": "not-implemented",
            }))
        }

        // 诊断：只报告事实，不自动修
        "devbox/doctor" => Ok(json!({ "checks": doctor_checks(&data_root(), platform) })),

        "devbox/job.get" => {
            let job_id = params
                .get("jobId")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing jobId".to_string()))?;
            let jobs = jobs.lock().unwrap();
            let job = jobs
                .get(job_id)
                .ok_or((-32001, format!("unknown job: {job_id}")))?;
            Ok(json!({
                "jobId": job.id,
                "component": job.component,
                "version": job.version,
                "phase": job.phase.as_str(),
                "percent": job.percent(),
                "bytesDone": job.bytes_done,
                "bytesTotal": job.bytes_total,
                "error": job.error,
            }))
        }

        "devbox/job.cancel" => {
            let job_id = params
                .get("jobId")
                .and_then(Value::as_str)
                .ok_or((-32602, "missing jobId".to_string()))?;
            let mut jobs = jobs.lock().unwrap();
            let job = jobs
                .get_mut(job_id)
                .ok_or((-32001, format!("unknown job: {job_id}")))?;
            Ok(json!({ "accepted": job.request_cancel() }))
        }

        other => Err((-32601, format!("method not found: {other}"))),
    }
}

/// 统计 store 下每个组件的占用（字节）。
fn store_usage(root: &std::path::Path) -> (u64, std::collections::BTreeMap<String, u64>) {
    let mut total = 0u64;
    let mut by = std::collections::BTreeMap::new();
    let Ok(components) = std::fs::read_dir(root.join("store")) else {
        return (0, by);
    };
    for component in components.flatten() {
        let name = component.file_name().to_string_lossy().to_string();
        let mut size = 0u64;
        if let Ok(versions) = std::fs::read_dir(component.path()) {
            for version in versions.flatten() {
                if let Ok(platforms) = std::fs::read_dir(version.path()) {
                    for platform in platforms.flatten() {
                        size += dir_size(&platform.path());
                    }
                }
            }
        }
        total += size;
        by.insert(name, size);
    }
    (total, by)
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => total += dir_size(&entry.path()),
            Ok(_) => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => {}
        }
    }
    total
}

fn in_path(exe: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        dir.join(exe).is_file() || dir.join(format!("{exe}.exe")).is_file()
    })
}

fn doctor_checks(
    root: &std::path::Path,
    platform: PlatformKey,
) -> Vec<Value> {
    let mut checks: Vec<Value> = Vec::new();

    // 1) 目录能否解析
    match catalog_path().and_then(|p| Catalog::from_file(&p).ok()) {
        Some(catalog) => {
            let placeholders: Vec<String> = catalog
                .components
                .iter()
                .flat_map(|c| {
                    c.versions
                        .keys()
                        .filter(|v| catalog.install_spec(&c.id, v, &platform.key()).is_err())
                        .map(move |v| format!("{} {}", c.id, v))
                })
                .collect();
            checks.push(json!({
                "id": "catalog",
                "level": "ok",
                "title": "组件目录可解析",
                "detail": format!("rev {} · {} 个组件", catalog.revision, catalog.components.len()),
            }));
            if !placeholders.is_empty() {
                checks.push(json!({
                    "id": "recipes-unverified",
                    "level": "warn",
                    "title": format!("{} 个配方还没验过", placeholders.len()),
                    "detail": format!("校验值仍是占位符，安装会被拒绝：{}", placeholders.join("、")),
                    "fix": "拿到上游 .sha256/.sha512 后填进 catalog/jdk.yaml",
                }));
            }
        }
        None => checks.push(json!({
            "id": "catalog",
            "level": "error",
            "title": "找不到或无法解析组件目录",
            "detail": "catalog/jdk.yaml 缺失或格式错误",
            "fix": "确认包内 catalog 目录存在，且 proxy 平台字段正确",
        })),
    }

    // 2) 数据目录可写
    let probe = root.join(".write-probe");
    let writable = std::fs::create_dir_all(root).is_ok()
        && std::fs::write(&probe, b"ok").is_ok()
        && std::fs::remove_file(&probe).is_ok();
    checks.push(json!({
        "id": "store-writable",
        "level": if writable { "ok" } else { "error" },
        "title": if writable { "数据目录可写" } else { "数据目录不可写" },
        "detail": root.display().to_string(),
        "fix": if writable { Value::Null } else { json!("检查目录权限，或换一个数据目录") },
    }));

    // 3) 入口是否覆盖了全部已激活组件
    let active: Vec<String> = match catalog_path().and_then(|p| Catalog::from_file(&p).ok()) {
        Some(c) => c
            .components
            .iter()
            .filter(|comp| devbox_engine::current(root, &comp.id).is_some())
            .map(|comp| comp.id.clone())
            .collect(),
        None => Vec::new(),
    };
    let shims = count_shims(root);
    if active.is_empty() {
        checks.push(json!({
            "id": "shims",
            "level": "info",
            "title": "还没有激活任何组件",
            "detail": "激活一个版本后会生成命令行入口",
        }));
    } else if shims == 0 {
        checks.push(json!({
            "id": "shims",
            "level": "warn",
            "title": "已激活但没有命令行入口",
            "detail": format!("{} 个组件处于激活状态，$DATA/bin 是空的", active.len()),
            "fix": "重新切换一次版本，或运行 devbox-shim",
        }));
    } else {
        checks.push(json!({
            "id": "shims",
            "level": "ok",
            "title": format!("命令行入口 {shims} 个"),
            "detail": format!("覆盖已激活组件：{}", active.join("、")),
        }));
    }

    // 4) 容器后端是否可用（M2 才用到，先如实报告）
    let docker = in_path("docker");
    checks.push(json!({
        "id": "docker",
        "level": if docker { "ok" } else { "info" },
        "title": if docker { "检测到 docker 可执行文件" } else { "未检测到 docker" },
        "detail": if docker { "容器后端仍计划中，尚未调用" } else { "服务类组件目前没有可用的本机分发，容器是后续方向" },
    }));

    // 5) 服务实例：明确未实现，不留假状态
    checks.push(json!({
        "id": "instances",
        "level": "info",
        "title": "服务实例尚未实现",
        "detail": "MySQL / Redis 等需要实例启停与探活，属于下一阶段",
    }));

    checks
}

/// 刷新 `$DATA/bin` 里的命令行入口，返回生成的个数。
fn refresh_shims(root: &std::path::Path, platform: PlatformKey) -> Option<usize> {
    let path = catalog_path()?;
    let catalog = Catalog::from_file(&path).ok()?;
    devbox_engine::shim::refresh(root, &catalog, platform)
        .ok()
        .map(|report| report.created.len())
}

fn count_shims(root: &std::path::Path) -> usize {
    std::fs::read_dir(root.join("bin"))
        .map(|entries| entries.flatten().count())
        .unwrap_or(0)
}

/// 已装版本扫描：只看 `store/<id>/<ver>/<platform>/.devbox-ok`。
/// 没有标记的目录都算未完成，不能出现在"已安装"里。
fn installed_versions(root: &std::path::Path, component: &str, platform: &str) -> Vec<String> {
    let dir = root.join("store").join(component);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join(platform).join(READY_MARKER).is_file())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    out
}

/// 把 catalog 翻译成 UI 能直接渲染的结构。
/// `unverified` 是关键信息：这些版本的校验值还是占位符，装了也会被拒绝。
fn catalog_payload(
    root: &std::path::Path,
    platform: PlatformKey,
) -> Result<(String, Value), String> {
    let path = catalog_path().ok_or("未找到组件目录")?;
    let catalog = Catalog::from_file(&path).map_err(|e| e.to_string())?;
    let key = platform.key();

    let mut components: Vec<Value> = Vec::new();
    for component in &catalog.components {
        let mut versions: Vec<String> = Vec::new();
        let mut unverified: Vec<String> = Vec::new();

        for (version, entry) in &component.versions {
            if !entry.artifacts.contains_key(&key) {
                continue;
            }
            versions.push(version.clone());
            if catalog.install_spec(&component.id, version, &key).is_err() {
                unverified.push(version.clone());
            }
        }
        if versions.is_empty() {
            continue; // 当前平台没有制品，就不出现在列表里
        }

        components.push(json!({
            "id": component.id,
            "kind": component.kind,
            "label": component.label.clone().unwrap_or_else(|| component.id.clone()),
            "category": component.category.clone().unwrap_or_else(|| "other".to_string()),
            "versions": versions,
            "unverified": unverified,
            "installed": installed_versions(root, &component.id, &key),
            "active": devbox_engine::current(root, &component.id),
            "docker": component.kind == "service",
        }));
    }

    Ok((catalog.revision.clone(), Value::Array(components)))
}

/// 组件目录随插件包发布，所以按"包内相对路径"找：
/// 1. `DEVBOX_CATALOG` 环境变量（开发/排障用）
/// 2. 可执行文件旁边的 `../catalog/jdk.yaml`（.dbxp 解包后的布局）
/// 3. 当前工作目录下的 `catalog/jdk.yaml`（源码里直接跑）
fn catalog_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("DEVBOX_CATALOG") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    // 可执行文件在包里的位置是 bin/<target>/<name>，往上找几层才到包根目录
    if let Some(exe) = std::env::current_exe().ok() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            let Some(current) = dir else { break };
            let candidate = current.join("catalog").join("jdk.yaml");
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = current.parent().map(|p| p.to_path_buf());
        }
    }
    let cwd = std::env::current_dir().ok()?.join("catalog/jdk.yaml");
    cwd.is_file().then_some(cwd)
}

fn resolve_spec(
    component: &str,
    version: &str,
    platform: PlatformKey,
) -> Result<InstallSpec, String> {
    let path = catalog_path().ok_or("未找到组件目录（catalog/jdk.yaml）")?;
    let catalog = Catalog::from_file(&path).map_err(|e| e.to_string())?;
    let mut spec = catalog
        .install_spec(component, version, &platform.key())
        .map_err(|e| e.to_string())?;
    spec.mirrors = std::env::var("DEVBOX_MIRRORS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(spec)
}

/// 目录里查不到 / 校验值是占位符，就直接失败，不假装在装。
fn spawn_catalog_failure(job_id: String, message: String, jobs: JobTable, out: SharedWriter) {
    thread::spawn(move || {
        {
            let mut guard = jobs.lock().unwrap();
            if let Some(job) = guard.get_mut(&job_id) {
                let _ = job.fail(message.clone());
            }
        }
        emit_event(
            &out,
            "evt/job.progress",
            json!({ "jobId": job_id, "phase": "failed", "percent": 0, "error": message }),
        );
    });
}

fn data_root() -> PathBuf {
    std::env::var("DBX_PLUGIN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("devbox-data"))
}

fn set_phase(jobs: &JobTable, out: &SharedWriter, job_id: &str, phase: Phase) {
    let pct = {
        let mut guard = jobs.lock().unwrap();
        let Some(job) = guard.get_mut(job_id) else { return };
        // 非法跳转直接忽略：说明引擎和状态机脱节了，宁可不动
        if job.advance(phase).is_err() {
            return;
        }
        job.percent()
    };
    emit_event(
        out,
        "evt/job.progress",
        json!({ "jobId": job_id, "phase": phase.as_str(), "percent": pct }),
    );
}

/// 真实安装：下载 → 换源降级 → 校验 → 解压 → 原子提交，全在引擎里。
fn spawn_real_install(job_id: String, spec: InstallSpec, jobs: JobTable, out: SharedWriter) {
    thread::spawn(move || {
        let root = data_root();
        set_phase(&jobs, &out, &job_id, Phase::Plan);
        // 必须显式进 Download：否则后面 Plan -> Verify 是非法跳转，
        // 状态机会静默忽略，job.get 永远停在 plan（这个坑踩过一次）
        set_phase(&jobs, &out, &job_id, Phase::Download);

        let j = jobs.clone();
        let o = out.clone();
        let id = job_id.clone();
        let mut last_pct: i16 = -1;
        let mut last_at = std::time::Instant::now();
        let mut progress = move |done: u64, total: Option<u64>| {
            let pct = {
                let mut guard = j.lock().unwrap();
                let Some(job) = guard.get_mut(&id) else { return };
                job.set_progress(done, total.unwrap_or(done.max(1)));
                job.percent()
            };
            // 每 8KB 一条事件会把桥接打爆（9MB 就是上千条）。只在百分比变化
            // 或 1 秒心跳时推，慢速下载时仍有反馈。
            let pct_i = pct as i16;
            if pct_i == last_pct && last_at.elapsed().as_millis() < 1000 {
                return;
            }
            last_pct = pct_i;
            last_at = std::time::Instant::now();
            emit_event(
                &o,
                "evt/job.progress",
                json!({
                    "jobId": id,
                    "phase": "download",
                    "percent": pct,
                    "bytesDone": done,
                    "bytesTotal": total,
                }),
            );
        };

        let result = install(&spec, &root, &mut progress);

        match result {
            Ok(outcome) => {
                // 校验/解压/提交都很短，这里按顺序补齐阶段，UI 才能看到完整过程
                for phase in [Phase::Verify, Phase::Extract, Phase::Commit, Phase::Ready] {
                    set_phase(&jobs, &out, &job_id, phase);
                }
                emit_event(
                    &out,
                    "evt/job.progress",
                    json!({
                        "jobId": job_id,
                        "phase": "ready",
                        "percent": 100,
                        "files": outcome.files,
                        "bytes": outcome.bytes,
                        "source": outcome.source,
                        "path": outcome.path.display().to_string(),
                    }),
                );
            }
            Err(error) => {
                {
                    let mut guard = jobs.lock().unwrap();
                    if let Some(job) = guard.get_mut(&job_id) {
                        let _ = job.fail(error.to_string());
                    }
                }
                emit_event(
                    &out,
                    "evt/job.progress",
                    json!({ "jobId": job_id, "phase": "failed", "percent": 0, "error": error.to_string() }),
                );
            }
        }
    });
}

/// M0 假安装：按状态机走一遍并持续发进度事件。
/// M1 会换成真实的 download → verify → extract → commit。
fn spawn_fake_install(job_id: String, jobs: JobTable, out: SharedWriter) {
    thread::spawn(move || {
        let steps = [
            (Phase::Plan, 0u64, 0u64),
            (Phase::Download, 12_000_000, 48_000_000),
            (Phase::Verify, 48_000_000, 48_000_000),
            (Phase::Extract, 48_000_000, 48_000_000),
            (Phase::Commit, 48_000_000, 48_000_000),
        ];

        for (target, done, total) in steps {
            // 取消检查点：只在阶段边界响应，不在写盘中途打断
            let canceled = {
                let jobs = jobs.lock().unwrap();
                jobs.get(&job_id).map(|j| j.cancel_requested()).unwrap_or(true)
            };
            if canceled {
                if let Some(job) = jobs.lock().unwrap().get_mut(&job_id) {
                    let _ = job.advance(Phase::Canceled);
                }
                emit_event(&out, "evt/job.progress", json!({ "jobId": job_id, "phase": "canceled" }));
                return;
            }

            {
                let mut jobs = jobs.lock().unwrap();
                let Some(job) = jobs.get_mut(&job_id) else { return };
                job.set_progress(done, total);
                if job.advance(target).is_err() {
                    return;
                }
            }
            emit_event(
                &out,
                "evt/job.progress",
                json!({
                    "jobId": job_id,
                    "phase": target.as_str(),
                    "percent": if total == 0 { 0 } else { done * 100 / total },
                }),
            );
            thread::sleep(Duration::from_millis(600));
        }

        if let Some(job) = jobs.lock().unwrap().get_mut(&job_id) {
            let _ = job.advance(Phase::Ready);
        }
        emit_event(&out, "evt/job.progress", json!({ "jobId": job_id, "phase": "ready", "percent": 100 }));
    });
}

/// 真实 catalog 在 M1 从 `catalog/*.yaml` 解析并验签；这里先给 UI 一个可渲染的壳。
fn mock_catalog(platform: PlatformKey) -> Value {
    json!([
        {
            "id": "jdk",
            "kind": "toolchain",
            "label": "JDK",
            "versions": ["21.0.5", "17.0.13", "8.0.432"],
            "installed": [],
            "active": null,
            "artifact": platform.key(),
        },
        {
            "id": "maven",
            "kind": "toolchain",
            "label": "Maven",
            "versions": ["3.9.9", "3.8.8"],
            "installed": [],
            "active": null,
            "artifact": platform.key(),
        },
        {
            "id": "mysql",
            "kind": "service",
            "label": "MySQL",
            "versions": ["8.4.3", "8.0.40", "5.7.44"],
            "installed": [],
            "instances": [],
            "artifact": platform.key(),
        },
        {
            "id": "redis",
            "kind": "service",
            "label": "Redis",
            "versions": ["7.4.1", "7.2.6", "6.2.16"],
            "installed": [],
            "instances": [],
            "artifact": platform.key(),
        },
    ])
}

fn emit_result(out: &SharedWriter, id: Value, result: Value) {
    write_line(out, &json!({ "id": id, "result": result }));
}

fn emit_error(out: &SharedWriter, id: Value, code: i32, message: &str) {
    write_line(out, &json!({ "id": id, "error": { "code": code, "message": message } }));
}

fn emit_event(out: &SharedWriter, event: &str, data: Value) {
    write_line(out, &json!({ "event": event, "data": data }));
}

fn write_line(out: &SharedWriter, value: &Value) {
    if let Ok(mut w) = out.lock() {
        let _ = writeln!(w, "{value}");
        let _ = w.flush();
    }
}
