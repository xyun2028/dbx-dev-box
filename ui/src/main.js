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
  disk: { usedGb: 12.4, totalGb: 80 },
  // 容器是可选后端：不可用时只提示原因，不隐藏功能
  docker: { available: true, version: "27.3.1" },
  query: "",
  filter: "all",
  showNewInstance: false,
  newPrefill: null,
  openLogs: null,
  jobs: new Map(),
  drawerOpen: false,

  packs: [
    {
      id: "java-backend",
      name: "Java 后端全家桶",
      desc: "一套能直接跑 Spring Boot 的本地环境，含依赖自动编排",
      size: "约 1.1 GB",
      items: [
        { label: "JDK 17", installed: true },
        { label: "Maven 3.9.9", installed: true },
        { label: "MySQL 8.4", installed: false },
        { label: "Redis 7.4", installed: false },
        { label: "Nacos 2.3", installed: false },
      ],
    },
    {
      id: "data-only",
      name: "只要数据库",
      desc: "不碰工具链，只把开发库和缓存拉起来",
      size: "约 620 MB",
      items: [
        { label: "MySQL 8.4", installed: false },
        { label: "Redis 7.4", installed: false },
      ],
    },
    {
      id: "middleware",
      name: "中间件一套",
      desc: "注册中心、调度、消息队列，依赖的 JDK 会自动先装",
      size: "约 780 MB",
      items: [
        { label: "Nacos 2.3", installed: false },
        { label: "xxl-job 2.4", installed: false },
        { label: "RocketMQ 5.3", installed: false },
      ],
    },
  ],

  components: [
    { id: "jdk", label: "JDK", kind: "toolchain", category: "java", desc: "Eclipse Temurin，多版本共存，切换只是改指针", versions: ["21.0.5", "17.0.13", "8.0.432"], installed: ["21.0.5", "17.0.13"], active: "21.0.5", sizeMb: 190 },
    { id: "maven", label: "Maven", kind: "toolchain", category: "java", desc: "构建工具，运行时需要 JDK，会自动关联", versions: ["3.9.9", "3.8.8"], installed: ["3.9.9"], active: "3.9.9", sizeMb: 9, requires: [{ component: "jdk", range: ">=8" }] },
    { id: "mysql", label: "MySQL", kind: "service", category: "database", desc: "实例化管理，同版本可跑多个库，数据与程序分离", versions: ["8.4.3", "8.0.40", "5.7.44"], installed: ["8.4.3"], sizeMb: 245, docker: true },
    { id: "redis", label: "Redis", kind: "service", category: "database", desc: "键值缓存，支持多实例与独立端口", versions: ["7.4.1", "7.2.6"], installed: [], sizeMb: 12, docker: true, nativeUnsupported: "Windows 本机无官方发行版" },
    { id: "nginx", label: "Nginx", kind: "service", category: "web", desc: "反向代理与静态服务，配置由模板渲染", versions: ["1.27.2", "1.26.2"], installed: [], sizeMb: 5, docker: true },
    { id: "nacos", label: "Nacos", kind: "service", category: "middleware", desc: "注册中心与配置中心，依赖 JDK 17+，自动先装", versions: ["2.3.2", "2.2.3"], installed: [], sizeMb: 130, docker: true, requires: [{ component: "jdk", range: ">=17 <22" }] },
    { id: "rocketmq", label: "RocketMQ", kind: "service", category: "middleware", desc: "消息队列，NameServer 与 Broker 作为一组实例管理", versions: ["5.3.1", "4.9.8"], installed: [], sizeMb: 60, docker: true },
    { id: "xxl-job-admin", label: "xxl-job-admin", kind: "service", category: "middleware", desc: "分布式任务调度，初始化需要 MySQL，会自动建库", versions: ["2.4.2", "2.3.1"], installed: [], sizeMb: 45, docker: true, requires: [{ component: "jdk", range: ">=8 <22" }, { component: "mysql", range: ">=8" }], initSql: true },
  ],

  instances: [
    { id: "mysql-dev", name: "mysql-dev", component: "MySQL", version: "8.4.3", port: 3306, status: "running", persist: "detached", runtime: "native", uptime: "3h 12m", logs: ["[12:04:11] ready for connections", "[12:04:11] port: 3306, socket: /tmp/mysql.sock", "[11:58:02] mysqld: starting 8.4.3"] },
    { id: "nacos-dev", name: "nacos-dev", component: "Nacos", version: "2.3.2", port: 8848, status: "degraded", persist: "session", runtime: "docker", uptime: "0h 04m", logs: ["[12:08:40] health check failed (3/3)", "[12:08:10] java: unsupported class file version", "[12:08:02] container devbox-nacos-dev started"] },
    { id: "redis-dev", name: "redis-dev", component: "Redis", version: "7.4.1", port: 6379, status: "running", persist: "autostart", runtime: "docker", uptime: "6h 41m", logs: ["[05:27:09] Ready to accept connections tcp", "[05:27:08] container devbox-redis-dev started"] },
    { id: "mysql-legacy", name: "mysql-57-legacy", component: "MySQL", version: "5.7.44", port: 3307, status: "stopped", persist: "autostart", runtime: "native", uptime: "—", logs: [] },
  ],

  problems: [
    { level: "error", title: "nacos-dev 健康检查连续 3 次未通过", fix: "日志提示 class file 版本不支持：把 Nacos 关联的 JDK 切到 17 以上再重启。当前激活的是 JDK 21，检查是否被项目级覆盖成了 8。" },
    { level: "warn", title: "Redis 在 windows-x64 上没有可用制品", fix: "官方不提供 Windows 发行版。可以改用 Memurai，或在 Linux/macOS 上安装，或走容器。这里不会拿其他平台的包凑数。" },
    { level: "info", title: "3307 端口被非 dev-box 进程占用", fix: "mysql-57-legacy 配置的端口当前被 PID 8124 占用。启动前请改端口，或确认该进程是否还需要保留。" },
  ],
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

// ------------------------------------------------------------------ 渲染

function runningCount() {
  return state.instances.filter((i) => i.status === "running" || i.status === "starting").length;
}
function activeJobs() {
  return [...state.jobs.values()].filter((j) => !["ready", "failed", "canceled"].includes(j.phase));
}

function render() {
  document.querySelectorAll(".nav-item").forEach((b) =>
    b.setAttribute("aria-current", String(b.dataset.view === state.view))
  );
  const titles = { overview: "概览", instances: "服务实例", catalog: "组件目录", doctor: "诊断" };
  $("view-title").textContent = titles[state.view];
  for (const v of Object.keys(titles)) $(`view-${v}`).hidden = v !== state.view;
  $("search-wrap").hidden = state.view !== "catalog";
  $("btn-new-instance").hidden = state.view !== "instances";

  const running = runningCount();
  $("nav-running").textContent = String(running);
  $("nav-running").dataset.live = running > 0 ? "1" : "0";
  $("nav-problems").hidden = state.problems.length === 0;
  $("nav-problems").textContent = String(state.problems.length);
  $("live-summary").innerHTML = `<b>${running}</b> 个服务运行中`;
  $("platform-badge").textContent = state.platform;
  $("runtime-mode").textContent = state.runtime;
  $("docker-status").textContent = state.docker.available ? state.docker.version : "未检测到";
  $("docker-status").style.color = state.docker.available ? "" : "var(--warn)";
  $("disk-usage").textContent = `${state.disk.usedGb} GB`;

  if (state.view === "overview") renderOverview();
  if (state.view === "instances") renderInstances();
  if (state.view === "catalog") renderCatalog();
  if (state.view === "doctor") renderDoctor();
  renderDrawer();
}

function renderOverview() {
  const installedCount = state.components.reduce((n, c) => n + c.installed.length, 0);
  const chains = state.components.filter((c) => c.kind === "toolchain" && c.installed.length > 0);

  $("view-overview").innerHTML = `
    <div class="kpi-grid">
      ${kpi("运行中的服务", runningCount(), `共 ${state.instances.length} 个实例`, "ok", `/ ${state.instances.length}`)}
      ${kpi("已安装版本", installedCount, "只读制品，删版本不动数据", "", " 个")}
      ${kpi("磁盘占用", state.disk.usedGb, `配额 ${state.disk.totalGb} GB，下载前预检空间`, "", " GB")}
      ${kpi("进行中的任务", activeJobs().length, activeJobs().length ? "关掉面板也不会中断" : "没有排队的工作", activeJobs().length ? "warn" : "", " 个")}
    </div>

    <section class="section">
      <div class="section-head">
        <h2 class="section-title">一键环境</h2>
        <span class="section-note">按依赖拓扑安装，缺少的 JDK 会自动先装</span>
      </div>
      <div class="packs">
        ${state.packs.map((p) => {
          const missing = p.items.filter((i) => !i.installed).length;
          return `
          <div class="pack">
            <h3 class="pack-name">${esc(p.name)}</h3>
            <p class="pack-desc">${esc(p.desc)}</p>
            <div class="pack-items">
              ${p.items.map((i) => `<span class="chip ${i.installed ? "chip-ok" : ""}">${esc(i.label)}</span>`).join("")}
            </div>
            <div class="pack-foot">
              <span class="pack-size">${esc(p.size)} · 待装 ${missing}</span>
              <button class="btn btn-sm ${missing ? "btn-primary" : ""}" data-action="run-pack" data-id="${p.id}">
                ${missing ? "拉起环境" : "已就绪"}
              </button>
            </div>
          </div>`;
        }).join("")}
      </div>
    </section>

    <section class="section">
      <div class="section-head">
        <h2 class="section-title">工具链</h2>
        <span class="section-note">切换只改激活指针，已装版本无需重新下载</span>
      </div>
      <div class="chains">
        ${chains.map((c) => `
          <div class="chain">
            <div>
              <div class="chain-name">${esc(c.label)}</div>
              <div class="chain-meta">${c.installed.length} 个版本 · PATH 生效</div>
            </div>
            <div class="chain-right">
              <select class="select" data-change="activate" data-id="${c.id}" aria-label="${esc(c.label)} 当前版本">
                ${c.versions.map((v) => `<option value="${v}" ${v === c.active ? "selected" : ""}>${v}</option>`).join("")}
              </select>
            </div>
          </div>`).join("")}
      </div>
    </section>

    <section class="section">
      <div class="section-head">
        <h2 class="section-title">服务</h2>
        <span class="section-note">端口与健康状态一眼可见</span>
        <button class="btn-link section-more" data-action="goto" data-id="instances">管理全部实例 →</button>
      </div>
      <div class="panel">${serviceTable(state.instances.slice(0, 4), false)}</div>
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
  const pre = state.newPrefill;
  const initialId = pre?.component && services.some((s) => s.id === pre.component) ? pre.component : services[0].id;
  const initial = state.components.find((c) => c.id === initialId);

  $("view-instances").innerHTML = `
    ${state.showNewInstance ? `
      <div class="panel">
        <form class="form" id="new-instance-form">
          <div class="field">
            <label>组件</label>
            <select class="select" name="component" id="ni-component">
              ${services.map((c) => `<option value="${c.id}" ${c.id === initialId ? "selected" : ""}>${esc(c.label)}</option>`).join("")}
            </select>
          </div>
          <div class="field">
            <label>版本</label>
            <select class="select" name="version" id="ni-version"></select>
          </div>
          <div class="field">
            <label>运行方式</label>
            <select class="select" name="runtime" id="ni-runtime">
              <option value="native">本机进程</option>
              <option value="docker">容器</option>
            </select>
          </div>
          <div class="field">
            <label>实例名</label>
            <input class="input" name="name" placeholder="${initialId}-dev" />
          </div>
          <div class="field">
            <label>端口</label>
            <input class="input mono" name="port" id="ni-port" type="number" value="${DEFAULT_PORT[initialId] ?? 8080}" />
          </div>
          <div class="field">
            <label>常驻方式</label>
            <select class="select" name="persist">
              <option value="session">随 DBX 退出停止</option>
              <option value="detached">常驻后台</option>
              <option value="autostart">开机自启</option>
            </select>
          </div>
          <div class="field" id="ni-volume-field">
            <label>数据位置</label>
            <select class="select" name="volume" id="ni-volume">
              <option value="bind">挂载到实例目录</option>
              <option value="volume">Docker 卷（更快）</option>
            </select>
          </div>
          <p class="form-note" id="ni-hint"></p>
          <p class="form-note">
            数据、配置、日志都放在独立实例目录里；删除组件版本不会碰这里的数据。
            端口冲突会在创建时直接报出来，不会偷偷换端口。
            运行方式创建后不可更改，要换只能新建实例再导数据。
          </p>
          <div class="form-actions">
            <button class="btn btn-ghost" type="button" data-action="new-cancel">取消</button>
            <button class="btn btn-primary" type="submit">创建实例</button>
          </div>
        </form>
      </div>` : ""}
    <section class="section">
      <div class="section-head">
        <h2 class="section-title">全部实例</h2>
        <span class="section-note">${state.instances.length} 个 · 本机与容器实例共用同一张表</span>
      </div>
      <div class="panel">${serviceTable(state.instances, true)}</div>
    </section>`;

  if (state.showNewInstance) {
    syncVersionOptions(pre?.version);
    $("ni-component").addEventListener("change", () => {
      const c = state.components.find((x) => x.id === $("ni-component").value);
      $("ni-port").value = DEFAULT_PORT[c.id] ?? 8080;
      syncVersionOptions();
    });
    $("ni-runtime").addEventListener("change", () => updateRuntimeHint());
    $("ni-volume").addEventListener("change", () => updateRuntimeHint());
    $("ni-port").addEventListener("input", () => updateRuntimeHint());
    $("new-instance-form").addEventListener("submit", (e) => {
      e.preventDefault();
      createInstance(e.currentTarget);
    });
  }
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
      (!q || `${c.id} ${c.label} ${c.desc}`.toLowerCase().includes(q))
  );

  $("view-catalog").innerHTML = `
    <p class="section-note">
      组件目录随插件版本发布（rev 2026.10.27-m0），新增组件版本需等商店审核；
      本机与容器两种后端共用同一份配方。
    </p>
    <div class="filters">
      ${categories.map((f) => `<button class="filter" data-action="filter" data-id="${f.id}" aria-pressed="${state.filter === f.id}">${f.label}</button>`).join("")}
    </div>
    ${items.length === 0 ? `<div class="empty">没有匹配的组件</div>` : `
    <div class="cards">
      ${items.map((c) => `
        <div class="card">
          <div class="card-top">
            <h3 class="card-name">${esc(c.label)}</h3>
            <span class="chip">${c.kind === "service" ? "服务" : "工具链"}</span>
            ${c.installed.length ? `<span class="chip chip-ok">已装 ${c.installed.length}</span>` : ""}
          </div>
          <p class="card-desc">${esc(c.desc)}</p>
          <div class="card-meta">
            <span>约 ${c.sizeMb} MB</span>
            <span>${c.versions.length} 个版本</span>
            ${c.docker ? `<span>支持容器</span>` : `<span>仅本机</span>`}
            ${c.requires?.length ? `<span>依赖 ${c.requires.map((r) => `${r.component.toUpperCase()} ${r.range}`).join("、")}</span>` : ""}
            ${c.initSql ? `<span>自动建库</span>` : ""}
            ${c.active ? `<span>当前 ${c.active}</span>` : ""}
          </div>
          ${c.nativeUnsupported
            ? `<span class="unsupported">${esc(c.nativeUnsupported)}，创建实例时会走容器</span>`
            : ""}
          <div class="card-foot">
            <select class="select" data-role="version" data-id="${c.id}" aria-label="${esc(c.label)} 版本">
              ${c.versions.map((v) => `<option value="${v}">${v}${c.installed.includes(v) ? " · 已装" : ""}</option>`).join("")}
            </select>
            ${c.kind === "service"
              ? `<button class="btn btn-sm btn-primary" data-action="new-for" data-id="${c.id}">新建实例</button>`
              : `<button class="btn btn-sm btn-primary" data-action="install" data-id="${c.id}">安装</button>`}
          </div>
        </div>`).join("")}
    </div>`}`;
}

function renderDoctor() {
  $("view-doctor").innerHTML = `
    <section class="section">
      <div class="section-head">
        <h2 class="section-title">发现 ${state.problems.length} 个需要处理的问题</h2>
        <span class="section-note">只报告，不自动修复</span>
      </div>
      <div class="panel">
        ${state.problems.map((p) => `
          <div class="problem">
            <span class="problem-tag" data-level="${p.level}">${p.level.toUpperCase()}</span>
            <div class="problem-body">
              <div class="problem-title">${esc(p.title)}</div>
              <div class="problem-fix">${esc(p.fix)}</div>
            </div>
          </div>`).join("")}
      </div>
    </section>
    <section class="section">
      <div class="section-head"><h2 class="section-title">自检范围</h2></div>
      <div class="panel">
        <div class="problem"><div class="problem-body"><div class="problem-fix">
          端口占用与系统保留段 · PATH 实际生效项与顺序 · 制品哈希抽检 · 孤儿进程 ·
          自启注册项与实例表一致性 · 磁盘配额 · 残留 staging 目录。
          这些问题只列出来并给修复建议，不会替你自动改。
        </div></div></div>
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

boot();
