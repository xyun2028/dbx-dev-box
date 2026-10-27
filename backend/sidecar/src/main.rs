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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use devbox_core::job::{Job, Phase};
use devbox_core::platform::PlatformKey;
use devbox_core::CORE_VERSION;
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

        "devbox/catalog" => Ok(json!({
            "revision": "m0-static",
            "components": mock_catalog(platform),
        })),

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

            // 异步：立刻返回 jobId，进度走事件。UI 刷新后凭 jobId 还能接上。
            spawn_fake_install(job_id.clone(), jobs.clone(), out.clone());
            Ok(json!({ "jobId": job_id }))
        }

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
