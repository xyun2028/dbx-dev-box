// dev-box 控制台
//
// 信息架构按"开发者打开它时想干什么"来组织，而不是按"插件能干什么"：
//   概览  —— 我的环境现在什么样（运行中的服务、生效的工具链、能一键拉起的环境包）
//   服务  —— 实例的完整生命周期：启停、端口、常驻、日志、连接串
//   组件  —— 装新东西的地方，是次要入口，不是首页
//   诊断  —— 出问题时的去处，平时不该刷存在感
//
// 这一层只画状态、发意图、接事件。能不能装、能不能取消，都由 sidecar 决定。

const $ = (id) => document.getElementById(id);
const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

const STATUS_TEXT = {
  running: "运行中",
  starting: "启动中",
  stopped: "已停止",
  degraded: "异常",
  failed: "失败",
};
const PERSIST_TEXT = { session: "随 DBX", detached: "常驻", autostart: "开机自启" };
const RUNTIME_TEXT = { native: "本机", docker: "容器" };
const PHASE_TEXT = {
  queued: "排队中",
  plan: "解析依赖",
  download: "下载中",
  verify: "校验中",
  extract: "解压中",
  commit: "提交中",
  ready: "已完成",
  failed: "失败",
  canceled: "已取消",
};

// ------------------------------------------------------------------ 数据

const state = {
  view: "overview",
  mode: "mock",
  platform: "windows-x64",
  runtime: "—",
  // 真实状态来自 devbox/state，不再由界面编造
  info: null,
  checks: [],
  theme: "dark",
  query: "",
  filter: "all",
  revision: "",
  shimDir: "",
  shims: 0,
  showNewInstance: false,
  newPrefill: null,
  openLogs: null,
  jobs: new Map(),
  drawerOpen: false,

  // 与 catalog/jdk.yaml 保持一致：只有 maven 有真实校验值。
  // 演示数据也不许比真实情况"多能给"，否则截图和产品会对不上。
  components: [
    { id: "jdk", label: "JDK", kind: "toolchain", category: "java", desc: "Eclipse Temurin，多版本共存，切换只是改指针", versions: ["21.0.5", "17.0.13"], installed: [], unverified: ["21.0.5", "17.0.13"], active: null, sizeMb: 190 },
    { id: "maven", label: "Maven", kind: "toolchain", category: "java", desc: "构建工具，运行时需要 JDK，会自动关联", versions: ["3.9.9", "3.8.8"], installed: ["3.8.8", "3.9.9"], unverified: [], active: "3.9.9", sizeMb: 9, requires: [{ component: "jdk", range: ">=8" }] },
    { id: "node", label: "Node.js", kind: "toolchain", category: "runtime", desc: "LTS 运行时，自带 npm/npx；切换版本同样只改指针", versions: ["22.11.0"], installed: ["22.11.0"], unverified: [], active: null, sizeMb: 31 },
    { id: "mysql", label: "MySQL", kind: "service", category: "database", desc: "实例化管理，同版本可跑多个库，数据与程序分离", versions: ["8.4.3"], installed: [], unverified: ["8.4.3"], sizeMb: 245, docker: true },
    { id: "nacos", label: "Nacos", kind: "service", category: "middleware", desc: "注册中心与配置中心，依赖 JDK 17+，自动先装", versions: ["2.3.2"], installed: [], unverified: ["2.3.2"], sizeMb: 130, docker: true, requires: [{ component: "jdk", range: ">=17 <22" }] },
  ],

  instances: [],

};

// ------------------------------------------------------------------ 宿主桥接

async function connectHost() {
  const real = window.dbxPlugin;
  if (real && typeof real.invoke === "function") {
    if (real.ready) await real.ready;
    return {
      mode: "real",
      invoke: (m, p) => real.invoke(m, p),
      onEvent: (fn) => real.onEvent?.(fn),
    };
  }
  // 浏览器直接打开时的演示宿主：界面和交互都能跑，顶栏会亮 MOCK 标记
  const listeners = new Set();
  let seq = 1;
  return {
    mode: "mock",
    onEvent: (fn) => listeners.add(fn),
    async invoke(method, params = {}) {
      if (method === "devbox/ping") return { platform: state.platform, mode: "mock" };
      // 演示宿主也要走同一条目录接口，否则界面在浏览器里和真实宿主里行为不一致
      if (method === "devbox/catalog") {
        return {
          revision: "2026.10.27-m0 (mock)",
          components: state.components,
          shimDir: "<data>/bin",
          shims: state.components.filter((c) => c.active && c.installed?.length).length * 2,
        };
      }
      if (method === "devbox/install") {
        const jobId = `mock-${seq++}`;
        const phases = ["plan", "download", "verify", "extract", "commit", "ready"];
        let i = 0;
        const tick = () => {
          if (i >= phases.length) return;
          const phase = phases[i++];
          const data = { jobId, phase, percent: Math.round((i / phases.length) * 100) };
          listeners.forEach((fn) => fn({ event: "evt/job.progress", data }));
          setTimeout(tick, 620);
        };
        setTimeout(tick, 200);
        return { jobId };
      }
      if (method === "devbox/state") {
        const byComponent = {};
        let total = 0;
        let installed = 0;
        for (const c of state.components) {
          for (const _v of c.installed ?? []) {
            const bytes = (c.sizeMb ?? 0) * 1048576;
            byComponent[c.id] = (byComponent[c.id] ?? 0) + bytes;
            total += bytes;
            installed += 1;
          }
        }
        const active = state.components.filter((c) => c.active).length;
        return {
          data: "<mock-data>",
          paths: { store: "<mock-data>/store", current: "<mock-data>/current", bin: "<mock-data>/bin" },
          store: { totalBytes: total, byComponent },
          counts: { installed, active, shims: active * 2 },
          catalog: {
            revision: "2026.10.27-m0 (mock)",
            components: state.components.length,
            unverified: state.components.reduce((n, c) => n + (c.unverified?.length ?? 0), 0),
          },
          docker: { available: false, version: null },
          persist: "not-implemented",
        };
      }
      if (method === "devbox/doctor") {
        const unv = state.components.reduce((n, c) => n + (c.unverified?.length ?? 0), 0);
        const active = state.components.filter((c) => c.active);
        return {
          checks: [
            { id: "catalog", level: "ok", title: "组件目录可解析", detail: `rev 2026.10.27-m0 (mock) · ${state.components.length} 个组件` },
            ...(unv ? [{ id: "recipes-unverified", level: "warn", title: `${unv} 个配方还没验过`, detail: "校验值仍是占位符，安装会被拒绝", fix: "拿到上游 .sha256/.sha512 后填进 catalog/jdk.yaml" }] : []),
            { id: "store-writable", level: "ok", title: "数据目录可写", detail: "<mock-data>" },
            active.length
              ? { id: "shims", level: "ok", title: `命令行入口 ${active.length * 2} 个`, detail: `覆盖已激活组件：${active.map((c) => c.label).join("、")}` }
              : { id: "shims", level: "info", title: "还没有激活任何组件", detail: "激活一个版本后会生成命令行入口" },
            { id: "docker", level: "info", title: "未检测到 docker", detail: "容器后端仍计划中，尚未调用" },
            { id: "instances", level: "info", title: "服务实例尚未实现", detail: "MySQL / Redis 等需要实例启停与探活，属于下一阶段" },
          ],
        };
      }
      if (method === "devbox/activate") {
        const c = state.components.find((x) => x.id === params.component);
        const previous = c?.active ?? null;
        if (c) c.active = params.version;
        return { ok: true, previous };
      }
      if (method === "devbox/job.cancel") return { accepted: true };
      throw new Error(`mock: ${method} 未实现`);
    },
  };
}

let host;

// ------------------------------------------------------------------ 动作

function setView(view) {
  state.view = view;
  state.showNewInstance = false;
  render();
}

async function install(componentId, version) {
  const c = state.components.find((x) => x.id === componentId);
  if (!c) return;
  try {
    const { jobId } = await host.invoke("devbox/install", { component: componentId, version });
    state.jobs.set(jobId, { name: `${c.label} ${version}`, phase: "queued", percent: 0 });
    state.drawerOpen = true;
    render();
  } catch (err) {
    toast(`安装失败：${err.message ?? err}`);
  }
}

function runPack(packId) {
  const pack = state.packs.find((p) => p.id === packId);
  if (!pack) return;
  const missing = pack.items.filter((i) => !i.installed);
  if (missing.length === 0) {
    toast("这个环境包里的组件都已安装");
    return;
  }
  // 真实实现由 sidecar 按依赖拓扑编排，这里只是演示任务归组
  missing.forEach((item, index) => {
    const jobId = `pack-${packId}-${index}`;
    state.jobs.set(jobId, { name: item.label, phase: "queued", percent: 0 });
    const phases = ["plan", "download", "verify", "extract", "commit", "ready"];
    phases.forEach((phase, i) => {
      setTimeout(() => {
        const job = state.jobs.get(jobId);
        if (!job) return;
        job.phase = phase;
        job.percent = Math.round(((i + 1) / phases.length) * 100);
        renderDrawer();
      }, 900 * (i + 1) + index * 500);
    });
  });
  state.drawerOpen = true;
  render();
  toast(`已创建 ${missing.length} 个安装任务，将按依赖顺序执行`);
}

function instanceAction(id, action) {
  const inst = state.instances.find((i) => i.id === id);
  if (!inst) return;
  const now = new Date().toTimeString().slice(0, 8);
  if (action === "start" || action === "restart") {
    inst.status = "starting";
    inst.uptime = "0h 00m";
    inst.logs.unshift(`[${now}] ${action === "restart" ? "restarting" : "starting"} ${inst.component} ${inst.version}`);
    setTimeout(() => {
      inst.status = "running";
      inst.logs.unshift(`[${now}] ready for connections on port ${inst.port}`);
      render();
    }, 1400);
  } else if (action === "stop") {
    inst.status = "stopped";
    inst.uptime = "—";
    inst.logs.unshift(`[${now}] shutdown complete`);
  }
  render();
}

function setPersist(id, mode) {
  const inst = state.instances.find((i) => i.id === id);
  if (!inst) return;
  inst.persist = mode;
  inst.logs.unshift(`[config] persistence = ${mode}`);
  render();
  toast(mode === "session" ? "已改为随 DBX 退出而停止" : `已设为${PERSIST_TEXT[mode]}，退出 DBX 不会中断服务`);
}

function activate(componentId, version) {
  const c = state.components.find((x) => x.id === componentId);
  if (!c) return;
  if (!c.installed.includes(version)) {
    install(componentId, version);
    return;
  }
  c.active = version;
  render();
  toast(`${c.label} 已切到 ${version}（只改指针，不重新下载）`);
}

async function copyConn(id) {
  const inst = state.instances.find((i) => i.id === id);
  if (!inst) return;
  const scheme = inst.component === "Redis" ? "redis" : inst.component === "MySQL" ? "mysql" : "http";
  const url = `${scheme}://127.0.0.1:${inst.port}`;
  try {
    await navigator.clipboard.writeText(url);
    toast(`已复制 ${url}`);
  } catch {
    toast(url);
  }
}

function createInstance(form) {
  const data = new FormData(form);
  const componentId = String(data.get("component"));
  const c = state.components.find((x) => x.id === componentId);
  if (!c) return;
  const runtime = String(data.get("runtime"));
  if (runtime === "docker" && !state.docker.available) {
    toast("未检测到 Docker 引擎，无法使用容器运行");
    return;
  }
  if (runtime === "native" && c.nativeUnsupported) {
    toast(`${c.nativeUnsupported}，请改用容器运行`);
    return;
  }
  const name = String(data.get("name") || "").trim() || `${componentId}-dev`;
  const port = Number(data.get("port")) || 0;
  const volume = runtime === "docker" ? String(data.get("volume") || "bind") : "bind";
  if (state.instances.some((i) => i.id === name)) {
    toast(`实例 ${name} 已存在，换个名字`);
    return;
  }
  if (state.instances.some((i) => i.port === port)) {
    toast(`端口 ${port} 已被其他实例占用，请换一个`);
    return;
  }
  state.instances.unshift({
    id: name,
    name,
    component: c.label,
    version: String(data.get("version")),
    port,
    status: "stopped",
    persist: String(data.get("persist")),
    runtime,
    volume,
    uptime: "—",
    logs: [
      `[create] instance ${name} created on ${runtime}, port ${port}`,
      runtime === "docker"
        ? `[create] data = ${volume === "bind" ? `instances/${name}/data` : `volume devbox-${name}`}`
        : `[create] data = instances/${name}/data`,
    ],
  });
  state.showNewInstance = false;
  state.newPrefill = null;
  render();
  toast(
    runtime !== "docker"
      ? `已创建 ${name}，数据目录独立于其他实例`
      : volume === "bind"
        ? `已创建 ${name}（容器），数据可直接在 instances/${name}/data 备份`
        : `已创建 ${name}（容器），数据在卷 devbox-${name}，删实例前记得导出`
  );
}

// ------------------------------------------------------------------ 主题

// 宿主的主题只有 window.dbxPlugin.theme 这一个入口（字符串或函数）。
// 没有宿主时退回系统配色，而不是永远深色——否则亮色系统里会突然一块黑。
function applyTheme() {
  let mode = null;
  try {
    let t = window.dbxPlugin?.theme;
    if (typeof t === "function") t = t();
    if (t === "dark" || t === "light") mode = t;
    else if (t && typeof t === "object" && (t.mode === "dark" || t.mode === "light")) mode = t.mode;
  } catch (_) {}
  if (!mode) {
    mode = window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  state.theme = mode;
  document.documentElement.dataset.theme = mode;
}

// ------------------------------------------------------------------ 渲染

function runningCount() {
  return state.instances.filter((i) => i.status === "running" || i.status === "starting").length;
}
function activeJobs() {
  return [...state.jobs.values()].filter((j) => !["ready", "failed", "canceled"].includes(j.phase));
}

function render() {
  applyTheme();
  document.querySelectorAll(".nav-item").forEach((b) =>
    b.setAttribute("aria-current", String(b.dataset.view === state.view))
  );
  const titles = { overview: "概览", instances: "服务实例", catalog: "组件目录", doctor: "诊断" };
  $("view-title").textContent = titles[state.view];
  for (const v of Object.keys(titles)) $(`view-${v}`).hidden = v !== state.view;
  $("search-wrap").hidden = state.view !== "catalog";
  $("btn-new-instance").hidden = true; // 服务实例尚未实现，不摆一个点了会失败的入口

  const counts = state.info?.counts ?? { installed: 0, active: 0, shims: 0 };
  const troubled = state.checks.filter((c) => c.level === "error" || c.level === "warn").length;

  // 这个徽标属于"服务实例"，不是"已激活组件"——语义不能混
  $("nav-running").textContent = "0";
  $("nav-running").hidden = true;
  $("nav-problems").hidden = troubled === 0;
  $("nav-problems").textContent = String(troubled);
  $("live-summary").innerHTML = `<b>${counts.installed}</b> 个版本 · <b>${counts.active}</b> 个激活`;
  $("platform-badge").textContent = state.platform;
  $("runtime-mode").textContent = state.runtime;

  const dockerOn = state.info?.docker?.available;
  $("docker-status").textContent = state.info ? (dockerOn ? "已检测到" : "未检测到") : "—";
  $("docker-status").style.color = dockerOn ? "" : "var(--warn)";
  $("disk-usage").textContent = state.info ? fmtBytes(state.info.store.totalBytes) : "—";

  if (state.view === "overview") renderOverview();
  if (state.view === "instances") renderInstances();
  if (state.view === "catalog") renderCatalog();
  if (state.view === "doctor") renderDoctor();
  renderDrawer();
}

function fmtBytes(n) {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${i === 0 || v >= 10 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

function renderOverview() {
  const counts = state.info?.counts ?? { installed: 0, active: 0, shims: 0 };
  const chains = state.components.filter(
    (c) => c.kind === "toolchain" && (c.installed ?? []).length > 0
  );
  const activeNames = state.components.filter((c) => c.active).map((c) => c.label);
  const installedBytes = state.info?.store?.totalBytes ?? 0;

  $("view-overview").innerHTML = `
    <div class="kpi-grid">
      ${kpi("已安装版本", counts.installed, "只读制品，切版本不动数据", "", " 个")}
      ${kpi("已激活", counts.active, activeNames.length ? activeNames.join(" / ") : "还没有激活任何组件", counts.active ? "ok" : "", " 个")}
      ${kpi("命令行入口", counts.shims, counts.shims ? "经由 current/ 转发，切版本不用改 PATH" : "激活版本后自动生成", "", " 个")}
      ${kpi("磁盘占用", fmtBytes(installedBytes), "store 目录合计", "", "")}
    </div>

    <section class="section">
      <div class="section-head">
        <h2 class="section-title">工具链</h2>
        <span class="section-note">切换只改激活指针，已装版本无需重新下载</span>
        <button class="btn-link section-more" data-action="goto" data-id="catalog">查看全部组件 →</button>
      </div>
      ${chains.length === 0
        ? `<div class="panel notice">
             <div class="notice-title">还没有装任何工具链</div>
             <p>去「组件」里选一个版本安装。装完会自动激活并生成命令行入口。</p>
           </div>`
        : `<div class="chains">
            ${chains.map((c) => `
              <div class="chain">
                <div>
                  <div class="chain-name">${esc(c.label)}</div>
                  <div class="chain-meta">${(c.installed ?? []).length} 个版本 · 当前 ${esc(c.active ?? "未激活")}</div>
                </div>
                <div class="chain-right">
                  <select class="select" data-change="activate" data-id="${c.id}" aria-label="${esc(c.label)} 当前版本">
                    ${(c.installed ?? []).map((v) => `<option value="${v}" ${v === c.active ? "selected" : ""}>${v}</option>`).join("")}
                  </select>
                </div>
              </div>`).join("")}
          </div>`}
      ${counts.shims > 0
        ? `<p class="path-hint">命令行入口 ${counts.shims} 个：<code>${esc(state.shimDir || "…")}</code> —— 把这一层加进 PATH 后，${activeNames.join(" / ")} 就能直接在终端里用；切换版本不需要重新配置。</p>`
        : `<p class="path-hint">还没有生成命令行入口。装好并激活一个组件后，这里会出现 $DATA/bin 的路径。</p>`}
    </section>

    <section class="section">
      <div class="section-head">
        <h2 class="section-title">服务</h2>
        <span class="section-note">还没实现，不假装有</span>
      </div>
      <div class="panel notice">
        <div class="notice-title">MySQL / Redis 这类服务目前无法启动</div>
        <p>已经能用的：下载 → 校验 → 安全解压 → 版本激活 → 命令行入口。</p>
        <p>还缺的：实例模型（独立数据目录 / 端口 / 配置渲染）、进程启停与健康检查、常驻三态。
           这些属于 M2，做之前不会在界面上摆出「运行中的实例」。</p>
      </div>
    </section>`;
}

function kpi(label, value, sub, tone, unit) {
  return `<div class="kpi" data-tone="${tone}">
    <span class="kpi-label">${label}</span>
    <span class="kpi-value">${value}<small>${unit}</small></span>
    <span class="kpi-sub">${sub}</span>
  </div>`;
}

function serviceTable(instances, withLogs) {
  if (instances.length === 0) return `<div class="empty">还没有实例，点右上角"新建实例"创建一个</div>`;
  return `<table class="table">
    <thead><tr>
      <th>状态</th><th>实例</th><th>组件</th><th>运行方式</th><th>端口</th>
      <th class="col-optional">常驻</th><th class="col-optional">运行时长</th><th class="col-actions">操作</th>
    </tr></thead>
    <tbody>
      ${instances.map((i) => `
        <tr>
          <td><span class="status status-${i.status}">${STATUS_TEXT[i.status]}</span></td>
          <td><div class="svc-name">${esc(i.name)}</div></td>
          <td><span class="mono dim">${esc(i.component)} ${esc(i.version)}</span></td>
          <td><span class="chip ${i.runtime === "docker" ? "chip-docker" : ""}">${RUNTIME_TEXT[i.runtime] ?? "本机"}</span></td>
          <td class="num">${i.port}</td>
          <td class="col-optional">
            <select class="select" data-change="persist" data-id="${i.id}" aria-label="常驻方式">
              ${Object.entries(PERSIST_TEXT).map(([k, v]) => `<option value="${k}" ${k === i.persist ? "selected" : ""}>${v}</option>`).join("")}
            </select>
          </td>
          <td class="num col-optional dim">${i.uptime}</td>
          <td class="col-actions">
            <span class="row-actions">
              ${["running", "starting", "degraded"].includes(i.status)
                ? `<button class="btn btn-sm btn-ghost" data-action="inst" data-op="restart" data-id="${i.id}">重启</button>
                   <button class="btn btn-sm btn-ghost" data-action="inst" data-op="stop" data-id="${i.id}">停止</button>`
                : `<button class="btn btn-sm" data-action="inst" data-op="start" data-id="${i.id}">启动</button>`}
              <button class="btn btn-sm btn-ghost" data-action="conn" data-id="${i.id}">连接串</button>
              ${withLogs ? `<button class="btn btn-sm btn-ghost" data-action="logs" data-id="${i.id}">${state.openLogs === i.id ? "收起" : "日志"}</button>` : ""}
            </span>
          </td>
        </tr>
        ${withLogs && state.openLogs === i.id ? `<tr><td colspan="8" style="padding:0"><pre class="log-view">${esc(i.logs.join("\n") || "暂无日志")}</pre></td></tr>` : ""}
      `).join("")}
    </tbody>
  </table>`;
}

const DEFAULT_PORT = { mysql: 3306, redis: 6379, nginx: 80, nacos: 8848, rocketmq: 9876, "xxl-job-admin": 8080 };

function renderInstances() {
  const services = state.components.filter((c) => c.kind === "service");

  $("view-instances").innerHTML = `
    <div class="panel notice">
      <div class="notice-title">服务实例还没实现</div>
      <p>这里现在不会出现任何「运行中」的实例——因为还没有能力真的把它们跑起来。</p>
      <p>下一阶段（M2）要做的是：实例模型、端口分配、配置渲染、进程启停与探活、常驻三态。
         在那些做完之前，这个页面只列组件声明，不摆假状态。</p>
    </div>

    <section class="section">
      <div class="section-head">
        <h2 class="section-title">服务类组件</h2>
        <span class="section-note">来自组件目录的声明，仅此而已</span>
      </div>
      <div class="panel">
        <table class="table">
          <thead><tr>
            <th>组件</th><th>目录里的版本</th><th class="col-optional">校验</th><th class="col-optional">容器</th><th>实例能力</th>
          </tr></thead>
          <tbody>
            ${services.length === 0
              ? `<tr><td colspan="5" class="dim">目录里没有服务类组件</td></tr>`
              : services.map((c) => `
                <tr>
                  <td><div class="svc-name">${esc(c.label)}</div></td>
                  <td><span class="mono dim">${(c.versions ?? []).join(" / ")}</span></td>
                  <td class="col-optional">${(c.unverified ?? []).length === (c.versions ?? []).length
                    ? `<span class="chip chip-warn">未验证</span>`
                    : `<span class="chip chip-ok">已校验</span>`}</td>
                  <td class="col-optional">${c.docker ? `<span class="chip chip-docker">可用</span>` : `<span class="faint">—</span>`}</td>
                  <td><span class="faint">未实现</span></td>
                </tr>`).join("")}
          </tbody>
        </table>
      </div>
    </section>`;
}

function syncVersionOptions(preselect) {
  const c = state.components.find((x) => x.id === $("ni-component").value);
  const vSel = $("ni-version");
  vSel.innerHTML = c.versions
    .map((v) => `<option value="${v}">${v}${c.installed.includes(v) ? " · 已装" : " · 需下载"}</option>`)
    .join("");
  const want = preselect ?? c.versions[0];
  if (c.versions.includes(want)) vSel.value = want;

  const rSel = $("ni-runtime");
  const nativeOpt = rSel.querySelector('option[value="native"]');
  if (c.nativeUnsupported) {
    nativeOpt.disabled = true;
    nativeOpt.textContent = "本机进程（该平台不提供）";
    rSel.value = "docker";
  } else {
    nativeOpt.disabled = false;
    nativeOpt.textContent = "本机进程";
    if (!preselect) rSel.value = "native";
  }
  updateRuntimeHint();
}

function updateRuntimeHint() {
  const c = state.components.find((x) => x.id === $("ni-component").value);
  const runtime = $("ni-runtime").value;
  const dockerOpt = $("ni-runtime").querySelector('option[value="docker"]');
  dockerOpt.disabled = !state.docker.available;
  dockerOpt.textContent = state.docker.available
    ? `容器 · Docker ${state.docker.version}`
    : "容器（未检测到 Docker 引擎）";

  // 数据位置只在容器模式下有意义：本机进程本来就写在实例目录里
  $("ni-volume-field").hidden = runtime !== "docker";

  const name = ($("new-instance-form").querySelector('[name="name"]').value || "").trim() || "实例名";
  const port = $("ni-port").value || "—";

  // 依赖说明：本机要准备宿主运行时并钉死；容器镜像自带运行时，只补服务依赖
  let depNote = "";
  if (c.requires?.length) {
    const fmt = (r) => `${r.component.toUpperCase()} ${r.range}`;
    const hostDeps = c.requires.filter((r) => r.component === "jdk");
    const svcDeps = c.requires.filter((r) => r.component !== "jdk");
    if (runtime === "native") {
      depNote = `创建时会自动准备 ${c.requires.map(fmt).join("、")}，并把版本固定给这个实例——之后全局切 JDK 不会影响它。`;
    } else {
      depNote = svcDeps.length
        ? `镜像自带运行时，不需要额外装 JDK；仍会自动准备 ${svcDeps.map(fmt).join("、")}。`
        : "镜像自带运行时（JRE），不需要额外装 JDK。";
    }
    if (c.initSql) depNote += "首次启动会自动建库并执行组件自带的初始化脚本。";
  }

  if (runtime === "docker") {
    const vol = $("ni-volume").value;
    const base = vol === "bind"
      ? `镜像按 digest 钉死，端口只绑 127.0.0.1:${port}。数据挂载到 instances/${name}/data，可直接备份；注意 Docker Desktop 上数据库会明显变慢，介意就换成卷。`
      : `镜像按 digest 钉死，端口只绑 127.0.0.1:${port}。数据放在 Docker 卷 devbox-${name}，速度更快；删除实例前需要先导出，否则数据跟着容器一起没。`;
    $("ni-hint").textContent = depNote ? `${base} ${depNote}` : base;
  } else {
    const base = `作为本机进程运行，日志与数据都在实例目录内；常驻为"开机自启"时注册用户级自启项，不需要管理员权限。`;
    $("ni-hint").textContent = depNote ? `${base} ${depNote}` : base;
  }
}

function renderCatalog() {
  const categories = [
    { id: "all", label: "全部" },
    { id: "java", label: "Java 工具链" },
    { id: "database", label: "数据库与缓存" },
    { id: "middleware", label: "中间件" },
    { id: "web", label: "Web 服务" },
  ];
  const q = state.query.trim().toLowerCase();
  const items = state.components.filter(
    (c) =>
      (state.filter === "all" || c.category === state.filter) &&
      (!q || `${c.id} ${c.label} ${c.desc ?? ""}`.toLowerCase().includes(q))
  );
  const unverifiedCount = state.components.reduce(
    (n, c) => n + (c.unverified?.length ?? 0),
    0
  );

  $("view-catalog").innerHTML = `
    <p class="section-note">
      目录随插件版本发布（rev ${esc(state.revision || "—")}），新增组件版本需等商店审核。
      ${unverifiedCount
        ? `其中 ${unverifiedCount} 个版本的校验值仍是占位符，会被引擎拒绝——先把配方验过才能装。`
        : ""}
    </p>
    <div class="filters">
      ${categories.map((f) => `<button class="filter" data-action="filter" data-id="${f.id}" aria-pressed="${state.filter === f.id}">${f.label}</button>`).join("")}
    </div>
    ${items.length === 0 ? `<div class="empty">没有匹配的组件</div>` : `
    <div class="cards">
      ${items.map((c) => {
        const unv = c.unverified ?? [];
        const installed = c.installed ?? [];
        const allUnverified = c.versions.length > 0 && unv.length >= c.versions.length;
        const toolchain = c.kind !== "service";
        return `
        <div class="card">
          <div class="card-top">
            <h3 class="card-name">${esc(c.label)}</h3>
            <span class="chip">${toolchain ? "工具链" : "服务"}</span>
            ${installed.length ? `<span class="chip chip-ok">已装 ${esc(installed.join(", "))}</span>` : ""}
            ${unv.length ? `<span class="chip chip-warn">未验证</span>` : ""}
          </div>
          <p class="card-desc">${esc(c.desc ?? "")}</p>
          <div class="card-meta">
            ${c.sizeMb ? `<span>约 ${c.sizeMb} MB</span>` : ""}
            <span>${c.versions.length} 个版本</span>
            ${c.docker ? `<span>支持容器</span>` : `<span>仅本机</span>`}
            ${c.requires?.length ? `<span>依赖 ${c.requires.map((r) => `${r.component.toUpperCase()} ${r.range}`).join("、")}</span>` : ""}
            ${c.initSql ? `<span>自动建库</span>` : ""}
            ${c.active ? `<span>当前 ${c.active}</span>` : ""}
          </div>
          <div class="card-foot">
            <select class="select" data-role="version" data-id="${c.id}" aria-label="${esc(c.label)} 版本">
              ${c.versions
                .map((v) => {
                  const bad = unv.includes(v);
                  const have = installed.includes(v);
                  return `<option value="${v}"${bad ? ' data-unv="1"' : ""}>${v}${have ? " · 已装" : ""}${bad ? " · 未验证" : ""}</option>`;
                })
                .join("")}
            </select>
            ${toolchain
              ? `<button class="btn btn-sm btn-primary" data-action="install" data-id="${c.id}"
                   ${allUnverified ? 'disabled title="这份配方的校验值还是占位符，装了也会被拒绝"' : ""}>
                   安装
                 </button>`
              : `<button class="btn btn-sm btn-primary" data-action="new-for" data-id="${c.id}">新建实例</button>`}
          </div>
        </div>`;
      }).join("")}
    </div>`}`;
}

function renderDoctor() {
  const checks = state.checks;
  const errors = checks.filter((c) => c.level === "error").length;
  const warns = checks.filter((c) => c.level === "warn").length;
  const LABEL = { ok: "OK", warn: "WARN", error: "ERR", info: "INFO" };

  $("view-doctor").innerHTML = `
    <section class="section">
      <div class="section-head">
        <h2 class="section-title">${checks.length} 项检查 · ${errors} 个错误 · ${warns} 个提醒</h2>
        <span class="section-note">只报告事实，不自动修复</span>
      </div>
      <div class="panel">
        ${checks.length === 0
          ? `<div class="problem"><div class="problem-body"><div class="problem-fix">还没有拿到检查结果。</div></div></div>`
          : checks.map((c) => `
            <div class="problem">
              <span class="problem-tag" data-level="${c.level}">${LABEL[c.level] ?? c.level}</span>
              <div class="problem-body">
                <div class="problem-title">${esc(c.title)}</div>
                ${c.detail ? `<div class="problem-fix">${esc(c.detail)}</div>` : ""}
                ${c.fix ? `<div class="problem-fix"><b>怎么处理：</b>${esc(c.fix)}</div>` : ""}
              </div>
            </div>`).join("")}
      </div>
    </section>
    <section class="section">
      <div class="section-head"><h2 class="section-title">这套检查是谁做的</h2></div>
      <div class="panel notice">
        <p>全部由侧车在本地计算：解析组件目录、试写数据目录、比对已激活组件与命令行入口、
           检测 PATH 里有没有 docker。</p>
        <p class="faint">没有远程上报，也不会替用户改环境。留空的是还没实现的部分，
          不是"检查通过"。</p>
      </div>
    </section>`;
}

function renderDrawer() {
  const jobs = [...state.jobs.entries()];
  const active = activeJobs();
  const drawer = $("drawer");
  drawer.dataset.active = active.length ? "1" : "0";
  drawer.dataset.open = state.drawerOpen ? "1" : "0";
  $("drawer-toggle").textContent = state.drawerOpen ? "收起" : "展开";

  if (jobs.length === 0) {
    $("drawer-summary").textContent = "暂无任务";
  } else if (active.length > 0) {
    const current = jobs.find(([, j]) => !["ready", "failed", "canceled"].includes(j.phase));
    $("drawer-summary").textContent = current
      ? `${active.length} 个任务进行中 · ${current[1].name} ${PHASE_TEXT[current[1].phase] ?? current[1].phase} ${current[1].percent}%`
      : `${active.length} 个任务进行中`;
  } else {
    $("drawer-summary").textContent = `${jobs.length} 个任务已结束`;
  }

  $("drawer-body").innerHTML = jobs.length
    ? jobs.map(([id, j]) => `
        <div class="job" data-phase="${j.phase}">
          <span class="job-name">${esc(j.name)}</span>
          <span class="job-meta">${PHASE_TEXT[j.phase] ?? j.phase} · ${j.percent}%${
        !["ready", "failed", "canceled"].includes(j.phase)
          ? ` · <button class="btn-link" data-action="cancel-job" data-id="${id}">取消</button>`
          : ""
      }</span>
          <span class="job-bar"><i style="width:${j.percent}%"></i></span>
        </div>`).join("")
    : `<div class="empty">安装任务会显示在这里，切换视图不会中断</div>`;
}

// ------------------------------------------------------------------ 反馈

let toastTimer;
function toast(message) {
  document.querySelector(".toast")?.remove();
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = message;
  document.body.appendChild(el);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.remove(), 2600);
}

// ------------------------------------------------------------------ 事件绑定（委托，重渲染不用重新绑）

document.addEventListener("click", (e) => {
  const nav = e.target.closest(".nav-item");
  if (nav) return setView(nav.dataset.view);

  if (e.target.closest("#drawer-bar")) {
    state.drawerOpen = !state.drawerOpen;
    return renderDrawer();
  }
  if (e.target.closest("#btn-new-instance")) {
    state.showNewInstance = true;
    state.newPrefill = null;
    return renderInstances();
  }

  const btn = e.target.closest("[data-action]");
  if (!btn) return;
  const { action, id, op } = btn.dataset;

  switch (action) {
    case "goto": return setView(id);
    case "new-for": {
      const sel = document.querySelector(`[data-role="version"][data-id="${id}"]`);
      state.newPrefill = { component: id, version: sel?.value };
      state.view = "instances";
      state.showNewInstance = true;
      return render();
    }
    case "filter":
      state.filter = id;
      return renderCatalog();
    case "install": {
      const select = document.querySelector(`[data-role="version"][data-id="${id}"]`);
      return install(id, select?.value);
    }
    case "run-pack": return runPack(id);
    case "inst": return instanceAction(id, op);
    case "conn": return copyConn(id);
    case "logs":
      state.openLogs = state.openLogs === id ? null : id;
      return renderInstances();
    case "new-cancel":
      state.showNewInstance = false;
      return renderInstances();
    case "cancel-job": {
      const job = state.jobs.get(id);
      if (job) job.phase = "canceled";
      host.invoke("devbox/job.cancel", { jobId: id }).catch(() => {});
      return renderDrawer();
    }
  }
});

document.addEventListener("change", (e) => {
  // 选到"未验证"的版本就把安装按钮禁掉，别让人点了才看到红色报错
  const versionSelect = e.target.closest('[data-role="version"]');
  if (versionSelect) {
    const btn = versionSelect.closest(".card")?.querySelector('[data-action="install"]');
    if (btn) btn.disabled = versionSelect.selectedOptions[0]?.dataset.unv === "1";
    return;
  }

  const el = e.target.closest("[data-change]");
  if (!el) return;
  if (el.dataset.change === "persist") setPersist(el.dataset.id, el.value);
  if (el.dataset.change === "activate") activate(el.dataset.id, el.value);
});

$("search").addEventListener("input", (e) => {
  state.query = e.target.value;
  renderCatalog();
});

// ------------------------------------------------------------------ 启动

async function boot() {
  // 支持 ?view=instances 深链：宿主或外部链接可以直接落到某个视图
  // ?new=redis 则直接打开新建表单并预选组件（右键菜单/连接复刻会用到）
  const params = new URLSearchParams(location.search);
  const initial = params.get("view");
  if (["overview", "instances", "catalog", "doctor"].includes(initial)) {
    state.view = initial;
  }
  const newFor = params.get("new");
  if (newFor) {
    state.view = "instances";
    state.showNewInstance = true;
    state.newPrefill = { component: newFor };
  }

  host = await connectHost();
  state.mode = host.mode;
  $("mode-badge").hidden = state.mode !== "mock";

  const ping = await host.invoke("devbox/ping", {}).catch(() => null);
  if (ping?.platform) state.platform = ping.platform;
  state.runtime = ping?.mode ?? state.mode;

  // 目录来自 sidecar；拿不到才保留内置的演示数据
  const catalog = await host.invoke("devbox/catalog", {}).catch(() => null);
  if (catalog?.components?.length) {
    state.components = catalog.components;
    state.revision = catalog.revision ?? "";
    state.shimDir = catalog.shimDir ?? "";
    state.shims = catalog.shims ?? 0;
  }
  const info = await host.invoke("devbox/state", {}).catch(() => null);
  if (info) state.info = info;
  const doctor = await host.invoke("devbox/doctor", {}).catch(() => null);
  if (doctor?.checks) state.checks = doctor.checks;

  // 排障用：宿主里可以直接看到界面拿到的是哪个目录版本
  document.getElementById("app").dataset.revision = state.revision || "none";
  document.getElementById("app").dataset.hostmode = host.mode;

  host.onEvent(({ event, data }) => {
    if (event !== "evt/job.progress" || !data?.jobId) return;
    const job = state.jobs.get(data.jobId);
    if (!job) return;
    job.phase = data.phase;
    job.percent = data.percent ?? job.percent;
    renderDrawer();
  });

  render();
}

// 启动失败也要说出来：白屏比报错更难排查，用户只能猜
boot().catch((err) => {
  const box = document.createElement("div");
  box.className = "boot-error";
  box.innerHTML = `<div class="notice-title">dev-box 启动失败</div>
    <pre>${String(err && err.stack ? err.stack : err)}</pre>
    <p class="faint">把这段发回给开发者即可。界面本身没问题，是初始化时抛了异常。</p>`;
  document.body.appendChild(box);
  console.error(err);
});
