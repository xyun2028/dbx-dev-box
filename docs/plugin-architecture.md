# dev-box 插件架构设计

> 目标：在 DBX 里一键装齐 JDK / Maven / MySQL / Redis / Nginx / MQ / Nacos / xxl-job-admin，支持多版本共存、秒级切换、可回滚、可复现。
> 本文只讲底层架构与关键取舍，不含具体实现代码。

---

## 0. 一句话概括

**把"装环境"这件事拆成三个正交的层：不可变的软件仓库（artifact）、可复制的运行实例（instance）、唯一的激活指针（activation）。**
所有扩展性、安全性、稳定性都建立在这三层的分离之上。

---

## 1. 先认边界：DBX 给的硬约束

架构不是自由发挥，是被宿主能力框死的。以下每一条都直接改变了设计：

| DBX 约束 | 对架构的直接影响 |
|---|---|
| UI 跑在 `sandbox="allow-scripts"` 的 iframe，opaque origin，默认无网络 | 前端**不能**下载、不能写盘、不能起进程。所有重活必须在 sidecar 里 |
| 唯一通道 `window.dbxPlugin.invoke / notify` | 前端只能是"显示器 + 遥控器"，要按这个前提设计状态同步 |
| 原生代码只能放 backend sidecar（Rust / Go，JSON-RPC over stdio） | 引擎必须用 Rust 或 Go 写，不能用 Node/Java |
| 安装包 ≤ 512 MiB，且有 `universal`（纯前端）/ 分平台两种发布形态 | 二进制不能进包，必须**运行时下载**；含 sidecar 就必须按 OS+CPU 分别发包 |
| JSONL 单条 ≤ 8 MiB、framed 帧 ≤ 64 MiB、桥接二进制 ≤ 8 MiB | 大文件走 sidecar 直连网络，不经过 UI 桥；日志必须流式分片 |
| `permissions` 按项申请，网络需 `host.network:<origin>` 白名单（仅 https、无路径、≤8 个） | 镜像站名单必须**长期稳定**，不能动态拼 origin；这直接否掉了"任意 URL 下载" |
| `host.storage` 单值 256 KiB / 整库 1 MiB | 别拿它存状态、存密码、存日志。状态用 sidecar 自己的 SQLite |
| 插件上下文是纯 JSON 快照，≤ 2 MiB | 别想着传对象引用，跨边界只有值 |

**结论**：这个插件本质上是"一个自带包管理器的进程管理器"，DBX 只提供 UI 容器、权限闸门和入口位置。

---

## 2. 地基：三个必须分开的概念

大多数同类工具烂掉，都是因为把这三件事混成一个"已安装版本"。

| 概念 | 是什么 | 生命周期 | 例子 |
|---|---|---|---|
| **Artifact（制品）** | 某个版本解压后的**只读**目录，全局只有一份 | 装一次，永不变更 | `jdk@21.0.3`、`mysql@8.4.0`、`redis@7.2.4` |
| **Instance（实例）** | 用某个制品跑起来的一份**可变**数据与配置 | 可创建/启停/销毁，同制品可有多个 | `mysql-8.4.0-dev`(3306)、`mysql-5.7-legacy`(3307) |
| **Activation（激活）** | 指向"当前用哪个制品"的**指针** | 随时切换，原子替换 | `current/jdk -> store/jdk/21.0.3` |

这个分离带来的直接好处：

- **多版本共存不需要"卸载再装"**：两个版本就是两棵只读目录，切换只是改指针。
- **同一个版本可以跑多个实例**：开发库、测试库、脏数据库三份数据互不污染。
- **卸载一个版本不会毁掉数据**：数据在 instance 里，制品删掉不影响 `data/`。
- **回滚是免费的**：指针指回旧版本即可，秒级。

按这个模型，用户看到的意图到机制的映射是：

| 用户想干的 | 机制 | 耗时 |
|---|---|---|
| "装 JDK 21" | 拉制品进 store | 分钟级（首次） |
| "切到 JDK 17" | 改 activation 指针 | < 100 ms（已装过时） |
| "再起一个 MySQL 5.7" | 建实例 + 分配端口 | 秒级（制品已存在） |
| "把 MySQL 升到 8.4" | 建新实例 + 迁数据（可选 dump/restore） | 分钟级，旧实例保留可回退 |
| "清掉所有 Java" | 删制品 + 删激活指针，保留实例数据 | 秒级 |

---

## 3. 分层架构

```
┌──────────────────────────────────────────────────────────────┐
│ L5  UI（沙箱 iframe，纯 TS，无网络无盘）                       │
│     目录 / 已安装 / 实例 / 任务 / 日志 / 诊断 六个视图          │
├──────────────────────────────────────────────────────────────┤
│ L4  Bridge：window.dbxPlugin.invoke ⇄ JSON-RPC over stdio     │
│     请求/响应 + 事件流（job.progress / instance.state / log）  │
├──────────────────────────────────────────────────────────────┤
│ L3  编排层  JobEngine · Planner · Resolver · Reconciler        │
│     任务队列、依赖拓扑、断点续做、崩溃恢复                     │
├──────────────────────────────────────────────────────────────┤
│ L2  能力层（可独立测试，无 UI 依赖）                           │
│     Downloader · Verifier · Extractor · Store · Activator      │
│     Supervisor · PortAllocator · ConfigRenderer · StateDB      │
├──────────────────────────────────────────────────────────────┤
│ L1  平台适配层                                                │
│     Windows(junction/PID复用检测) · macOS(launchd/xattr)       │
│     Linux(systemd --user/权限位) · 路径与 shell 语义差异        │
├──────────────────────────────────────────────────────────────┤
│ L0  Catalog（签名清单，独立于插件版本演进）                    │
└──────────────────────────────────────────────────────────────┘
```

设计要点：

- **L2 全部是无状态函数 + 一个 StateDB**，可以脱离 DBX 单独跑 CLI 测试。这是能不能长期维护的分水岭。
- **L3 是唯一有"事务"概念的地方**，L2 的每个动作都必须可重入。
- **L0 随插件版本发布**（决策：走商店审核）。catalog 打在 `.dbxp` 包内，目录因此被安装包的 Ed25519 签名一并覆盖——**不需要另建一套目录签名体系**，这是选商店审核换来的简化。代价是新增一个组件版本要等一次审核，所以首发时要把常用版本一次性列全。

---

## 4. 磁盘布局

所有数据落在 DBX 给的 `DBX_PLUGIN_DATA_DIR` 下，不污染用户主目录以外的路径：

```
$DATA/
├── catalog/
│   ├── current.json            # 当前生效的目录版本（rev 号）
│   ├── <rev>.json              # 组件与版本的完整清单
│   └── <rev>.sig               # Ed25519 签名
├── store/                      # 只读，写完打标记，永不修改
│   ├── jdk/21.0.3/windows-x64/{...,.devbox-ok}
│   ├── jdk/17.0.11/windows-x64/
│   └── mysql/8.4.0/windows-x64/
├── current/                    # 激活指针（junction / symlink）
│   ├── jdk   -> ../store/jdk/21.0.3/windows-x64
│   └── maven -> ../store/maven/3.9.9/windows-x64
├── instances/
│   └── mysql-8.4.0-a1b2/
│       ├── instance.json       # 制品引用、端口、创建时间、状态
│       ├── config/my.cnf       # 模板渲染产物
│       ├── data/               # 用户数据，卸载默认不动
│       ├── logs/               # 轮转日志
│       └── run/pid
├── bin/                        # 可选：生成的启动脚本 / shim
├── tmp/                        # 下载与解压暂存，可随时整体清理
├── journal/                    # 未完成事务记录
├── logs/                       # 引擎自身日志
└── state.db                    # SQLite：artifacts / instances / jobs / events
```

**三条不可违反的纪律**：

1. `store/` 里的东西一旦写下 `.devbox-ok` 标记就只读。任何修改都要产生新目录。这是"可回滚"和"可校验"的前提。
2. `tmp/` 里的一切都可以被杀掉，恢复逻辑不得依赖它。
3. `instances/*/data` 属于用户，任何删除动作必须显式确认 + 默认备份。

---

## 5. 组件模型：扩展性的核心

### 5.1 声明式配方（Recipe）

新中间件 = 一份 YAML，不是一段代码。引擎只认配方，不认具体组件。

```yaml
schema: 1
id: mysql
kind: service                  # toolchain | service
label: MySQL
versions:
  "8.4.0":
    artifacts:
      windows-x64:
        url: https://mirrors.example.com/mysql-8.4.0-winx64.zip
        sha256: 3f2a...        # 强绑定，镜像站换不了字节
        size: 234567890
        format: zip
        strip_components: 1
      darwin-arm64: { url: ..., sha256: ..., size: ... }
    provides:
      binaries: [mysqld, mysql]
      default_port: 3306
    install:
      - render: { from: assets/my.cnf.tmpl, to: "{{instance_root}}/my.cnf", vars: [port, datadir] }
      - driver: { name: mysql-initialize, args: { datadir: "{{datadir}}" } }
    start:
      argv: ["{{install_root}}/bin/mysqld", "--defaults-file={{instance_root}}/my.cnf"]
    health:
      tcp: { host: 127.0.0.1, port: "{{port}}", timeout: 60s }
```

带依赖的组件只要声明：

```yaml
id: nacos
requires: [{ component: jdk, range: ">=17 <22" }]
---
id: xxl-job-admin
requires:
  - { component: jdk, range: ">=8 <22" }
  - { component: mysql, range: ">=8" }
```

### 5.2 步骤白名单（这是安全的关键）

配方里的 step 只允许这些动词，每个动词只接受固定参数，**没有 shell，没有字符串拼接执行**：

`download` `extract` `render` `copy` `write-file` `run`(argv 数组) `chmod` `hardlink` `wait-port` `http-probe` `sql` `driver`

`run` 的 argv 是数组，变量替换后仍然不经过 shell 解析 —— 这样 `; rm -rf` 永远没有机会被当成命令。

### 5.3 两级扩展

| 层级 | 谁写 | 能力 | 审批 |
|---|---|---|---|
| Tier 1 配方 | 任何人 | 覆盖 90% 的中间件（下载/解压/配置渲染/启停/探活） | 进包 → 随插件发版走商店审核 |
| Tier 2 驱动 | 项目内部 | 配方描述不了的特殊逻辑（Windows 服务安装、协议级初始化、授权校验） | 编进 sidecar，随插件发版 |

Tier 2 用 Rust trait 实现，配方里通过 `driver: { name: ... }` 调用：

```rust
pub trait Driver: Send + Sync {
    fn id(&self) -> &'static str;
    fn preflight(&self, ctx: &Ctx) -> Result<()>;          // 环境检查（如 VC++ 运行库）
    fn plan(&self, ctx: &Ctx) -> Result<Vec<Step>>;        // 动态生成步骤
    fn health(&self, ctx: &Ctx) -> Result<Health>;         // 自定义探活（非 TCP 场景）
}
```

**不要**给配方开放"执行脚本"的口子。那是把沙箱拱手让出去，后期无法审计。

---

## 6. 安装事务

### 6.1 状态机

```
QUEUED → PLAN → DOWNLOAD → VERIFY → EXTRACT → COMMIT → READY
                    │         │         │
                    └─────────┴─────────┴──→ FAILED / CANCELED / PAUSED
```

每个阶段写一条 journal（含已完成的分片、已解压的条目数、目标路径）。sidecar 或 DBX 意外退出后，启动时按 journal 续做：

- `DOWNLOAD`：服务端支持 Range 就续传，否则重下（成本可控，因为有镜像测速）。
- `VERIFY`：重算即可，幂等。
- `EXTRACT`：直接丢弃 staging 重来（不试图续解压，省得留半成品）。
- `COMMIT`：只要还没打 `.devbox-ok`，就视为未安装。

### 6.2 原子提交

```
1. 下载到 tmp/<jobId>/pkg
2. 校验 sha256 与大小（不匹配立即删除，不重试超过 2 次换镜像）
3. 解压到 store/<id>/<ver>/<platform>.staging/
4. 规范化权限、校验关键二进制存在
5. 写 .devbox-ok ← 唯一"成功"标志
6. rename staging → final（同卷 rename 是原子的）
```

只要 `.devbox-ok` 不在，任何残留都算垃圾，启动时由 GC 清掉。**顺序不能变**：先标记、后改名会让崩溃窗口产生"看起来装好了实际缺文件"的假目录。

### 6.3 幂等与取消

- 每个 step 有 `done` 标记，重跑跳过。
- 取消 = 置标志 → 当前 step 在检查点退出 → 清理 tmp/staging → 状态置 `CANCELED`（不删已完成的 artifact）。
- 安装和卸载共用一把组件级锁，避免两个窗口同时装同一个版本。

---

## 7. 多版本共存与切换

按 `kind` 分成两套机制，别混：

### 7.1 toolchain（JDK / Maven / Node 类）

只有一个"当前版本"，通过激活指针切换：

```
current/jdk ──(junction)──> store/jdk/21.0.3/windows-x64
```

- 切换 = 写临时链接 → 原子替换 → 更新 `state.db`。失败自动回滚到上一个指针。
- PATH 只加**一个**目录：`$DATA/current/jdk/bin`。用户环境里永远只有一条路径，不随版本数量膨胀。
- 项目级覆盖：`devbox env --project` 生成 `.devbox/env.{cmd,ps1,sh}`，由 shell 激活时注入；或提供 `devbox shell` 起子会话。
  > 取舍：v1 用"指针 + 激活脚本"，零运行时开销；v2 若确实需要"进目录自动切换"，再引入轻量 shim 进程（asdf 模式），代价是每次调用多一次进程创建（Windows 约 5–10 ms）。

### 7.2 service（MySQL / Redis / Nginx / MQ / Nacos / xxl-job）

服务天生要多开，所以是实例模型：

- 每个实例独立 `config` + `data` + `port`，互不感知。
- 端口策略：主实例优先抢默认端口（3306），抢不到或新建次实例时从 `35000–39999` 里挑一个当前未被占用的。
- **启动时不做自动改端口**：配置里写死 3306 却启动失败，宁可报错让人改，也不要偷偷换端口——那会让外部配置全部失效。
- 升级 = 用新版本建实例 + 按需迁移数据，旧实例原地保留，随时切回。

---

## 8. 编排：依赖与环境包

这是"快速搭好环境"真正的价值点。

- **依赖图求解**：`xxl-job-admin → jdk, mysql`；`nacos → jdk`。求解器选满足范围的最小集合，冲突则报出冲突链（"你要 JDK 21，但 xxl-job-admin 只支持 <22"）。
- **拓扑安装**：按依赖顺序装制品 → 建实例 → 启动 → 探活通过才继续下一个。任一步失败，整个包回滚（保留已装制品，销毁本次创建的实例）。
- **环境包（Env Pack）**：一个命名的组合，比如 `backend-java` = jdk17 + maven + mysql8 + redis7 + nacos2。
- **锁文件**：`devbox.lock` 记录每个组件的精确版本 + 端口 + 关键配置项。团队共享这个文件，就能得到完全一致的本地环境。

```yaml
# devbox.lock（由 devbox env plan 生成，进版本库）
packs:
  backend-java:
    jdk:    { version: 17.0.11, activate: true }
    maven:  { version: 3.9.9 }
    mysql:  { version: 8.4.0, instance: mysql-dev, port: 3306 }
    redis:  { version: 7.2.4, instance: redis-dev, port: 6379 }
    nacos:  { version: 2.3.2, instance: nacos-dev, port: 8848 }
```

---

## 9. 进程监管与常驻

服务是"用户级进程"，不注册系统服务（这是默认，理由见 §10）。

| 关注点 | 做法 |
|---|---|
| 启动 | `argv` 数组直接 spawn，`cwd` 锁在 instance 目录，不继承父进程的敏感环境变量 |
| 存活判断 | **不能只看 PID**。记录 `(pid, 进程启动时间戳)`，避免 PID 被复用后误判存活 |
| 探活 | 端口可连 ≠ 服务可用。MySQL 要做握手，Redis 发 `PING`，Nacos 打 `/nacos/v1/console/health/liveness`；超时进入 `DEGRADED` 而不是 `RUNNING` |
| 停止 | 先 SIGTERM / `taskkill`（不带 /F），宽限 10 s，再强杀；MySQL 必须先 `mysqladmin shutdown` 走正常关闭，避免脏页 |
| 日志 | 内存环形缓冲（2000 行/实例）+ 文件轮转（10 MB × 3），结构化 JSON 行，敏感值脱敏 |
| 孤儿回收 | 启动时扫描：DBX 不在了但子进程还活着 → 标记并提示，不擅自 kill 用户可能在用的库 |
| 崩溃重启 | 可选策略 `never / on-failure(N 次) / always`，默认 `never`（数据库自动重启最容易掩盖问题） |
| 资源 | 每个实例可选 CPU/内存上限（Windows Job Object，Unix rlimit/cgroup v2），默认不限但可观测占用 |

**恢复原则**：宁可"启动时报错并给出修复建议"，也不要"自动猜着修"。自动修复在环境管理工具里通常会把用户的数据搞坏。

### 9.1 常驻三态：一个勾选背后是三件事

"是否常驻"看似一个布尔值，实际由两个正交问题组成：**DBX 关掉以后服务还跑不跑**、**开机要不要自己起来**。所以做成三态，UI 上是一个勾选 + 一个二级开关：

| 模式 | DBX 退出后 | 开机后 | 适用 |
|---|---|---|---|
| `session`（默认） | 停止 | 不启 | 临时试一下、跑一次就删 |
| `detached` | **继续运行** | 不启 | 长期开着本地库，但不想它开机自启 |
| `autostart` | 继续运行 | **自动启动** | 日常开发常驻的中间件 |

崩溃重启（`never / on-failure / always`）是**独立策略**，不要和常驻混成一个开关——MySQL 自动重启最容易掩盖真实问题，默认必须是 `never`。

### 9.2 代价：sidecar 必须变成双进程形态

这是整个决策里最容易被低估的部分。如果 sidecar 只是 DBX 的子进程，DBX 一退它就死，`detached` 根本无从实现。所以：

```
DBX 启动
 └─ sidecar-stdio（DBX 的子进程，只做协议翻译）
      ├─ 探测 $DATA/run/daemon.sock（Windows: \\.\pipe\devbox-<sid>）
      ├─ 没有 → spawn devbox-daemon（脱离父进程，独立生命周期）→ 等 socket ready
      └─ 已有 → 直接连上
      ↓
    所有 RPC 转发给 daemon；stdio 侧不碰 store、不持锁、被杀也不影响服务
```

三个直接收益，正好覆盖三处老问题：

1. `detached` 有了宿主：daemon 活着，服务就活着。
2. **单写者问题被结构性解决**：daemon 独占 `$DATA/.lock`，开三个 DBX 窗口也只有一份状态在动。
3. UI 断线可恢复：窗口刷新后重新 attach，`job.get` 接上原任务，安装不会中断。

连接鉴权（本地也不可省：同机其他程序能连上来就是漏洞）：

- Unix socket 放 `$DATA/run/`，权限 `0600`，属主本人。
- Windows 命名管道用当前用户 SID 写 DACL，拒绝其他账户。
- 另加一次性 token：daemon 启动时写入 `$DATA/run/token`（0600），stdio 侧读取后带在首帧。防同用户下的误连与恶连。

### 9.3 三平台各自的实现（全都不需要管理员权限）

| OS | `detached` | `autostart` | 坑 |
|---|---|---|---|
| Windows | `DETACHED_PROCESS` + 脱离 Job Object | 计划任务，`/SC ONLOGON /RL LIMITED`，用户上下文 | 标准用户可创建；失败常静默，必须自己写日志校验；卸载时务必删任务 |
| macOS | 双 fork + `setsid` | `~/Library/LaunchAgents/<id>.plist` + `launchctl bootstrap gui/$UID` | LaunchAgent 只在登录后运行；`KeepAlive` 交给 launchd，别自己写守护循环 |
| Linux | `setsid` | `~/.config/systemd/user/<id>.service` + `systemctl --user enable --now` | 未开 linger 时注销即停（`loginctl enable-linger` 需策略支持，不作默认要求） |

**一致性纪律**：取消勾选必须**同时**清理注册项（任务 / plist / unit）和进程，否则会留下"看不见但还在跑"的幽灵实例。Doctor 每次启动都要比对"已注册的自启项"与"实例表"，不一致就报出来。

### 9.4 三平台差异矩阵（并进的真实成本）

| 维度 | Windows | macOS | Linux | 架构应对 |
|---|---|---|---|---|
| 包格式 | zip | tar.gz / zip | tar.gz | recipe 每平台分别声明 artifact |
| 可执行位 | 无此概念 | 需保留 | 需保留 | 解压后统一对 `bin/*` 补 755 |
| 目录链接 | junction（免管理员、免开发者模式） | symlink | symlink | `Platform::link_dir()` 抽象，激活逻辑不感知 |
| 长路径 | > 260 需 `\\?\` 前缀 | 无 | 无 | 路径统一过一次 `native_path()` |
| 大小写 | 不敏感 | 默认不敏感 | 敏感 | 文件名比较统一降小写 |
| 停止信号 | `taskkill`（先不带 `/F`） | SIGTERM | SIGTERM | `Supervisor::graceful_stop()` |
| PID 复用 | 有 | 有 | 有 | 一律用 `(pid, 进程启动时间)` 二元组判活 |
| 端口被系统占用 | Hyper-V / WSL 保留段会吃掉端口 | 较少 | 较少 | 分配前查 `netsh int ipv4 show excludedportrange tcp` |
| 运行库依赖 | VC++ 运行库 | 架构分 arm64/x64 | **glibc 版本** | 见下方风险说明 |
| 安全软件 | Defender 误杀新下载的二进制 | 隔离属性 | — | 只做校验与提示，**不引导用户关闭防护** |

**Linux 是唯一真正的风险点**：Windows 和 macOS 的中间件基本是"自带运行时的整包"，Linux 不是——官方 Nginx 二进制绑发行版，MySQL 官方包依赖 `libaio`/`numactl`，Redis 官方只给源码。三条路线，建议 M2 前必须选定：

1. 优先选自带依赖的分发（JDK 与多数 Java 中间件都满足）；
2. C 系组件提供"按 libc 分类"的预编译包（自己的 CI 出包，成本是维护多条构建链）；
3. 明确降级：Linux 只保证 Debian/Ubuntu 与 RHEL 系，其余发行版提示"用容器"。

建议 1 + 3：把 Linux 的支持边界写进 UI，比假装全平台可用诚实得多。

---


## 10. 安全设计

### 10.1 威胁模型

| 威胁 | 后果 | 对策 |
|---|---|---|
| Catalog 被篡改 | 全网用户装到恶意二进制 | 目录 Ed25519 签名，公钥内嵌在引擎里 |
| 镜像站被劫持 / DNS 污染 | 下发同名但内容不同的包 | catalog 里钉死 sha256；镜像**只能换域名不能换字节** |
| 恶意压缩包（zip-slip / 符号链接逃逸 / 压缩炸弹） | 任意路径写文件、覆盖系统文件 | 解压前逐条目校验 + 流式体积监控 |
| 配方被塞入任意命令 | RCE | 步骤白名单 + argv 数组，永不经过 shell |
| PATH 被劫持 | 用户执行 `java` 实际跑了别的程序 | 激活目录 ACL 仅本人可写；全局 PATH 修改需显式确认 |
| 服务暴露到公网 | 数据库被扫 | 默认绑 `127.0.0.1`；改 `0.0.0.0` 必须二次确认 |
| 密码进日志 | 凭据泄露 | 创建时生成随机密码，落在 OS 凭据库；日志统一脱敏 |
| 提权被滥用 | 任意管理员操作 | 提权走独立最小接口，白名单动作，不复用主进程权限 |

### 10.2 六道闸门

1. **信任链**：`catalog 签名 → artifact sha256 → 解压校验`。任何一环缺失就拒绝，除非用户显式打开"专家模式"（且 AI / MCP 触发路径永久禁止专家模式）。
2. **解压闸门**（必须逐条实现，这是最高频的攻击面）：
   - 拒绝绝对路径、盘符、`..`、空名、控制字符
   - Windows 额外拒绝：`:`（备用数据流）、`CON/PRN/AUX/NUL/COM1..9/LPT1..9`、结尾的点和空格
   - 拒绝硬链接；符号链接只允许相对且解析后仍在 staging 内，否则丢弃
   - 去掉所有 setuid / setgid / sticky 位
   - 条目数 ≤ 200k；解压总量 ≤ 声明大小 × 1.2 且 ≤ 配额；实时监控压缩比，超限中止
   - 落盘后 `canonicalize` 再比对前缀，兜底 zip-slip
3. **执行闸门**：argv 数组 + 环境变量白名单（剥掉 `LD_PRELOAD` / `DYLD_*` / `NODE_OPTIONS` 这类注入点）+ cwd 限定。
4. **权限闸门**：默认全程用户态，零管理员权限。只有"安装为系统服务 / 开机自启"这一条路需要提权，且：
   - 独立小二进制，只暴露 5 个固定动作（注册服务、注销、启停、查状态、写开机项）
   - 每次调用弹系统授权框，不做"一次授权长期复用"
   - 动作与参数全部进审计日志
5. **网络闸门**：只走 https；域名白名单来自 manifest 的 `host.network`，且必须是**长期稳定的镜像域**（DBX 限制 ≤ 8 个），所以要做"镜像前缀替换"而不是"每加一个源就加一个域"。
6. **数据闸门**：删除版本不删数据；删除实例数据要二次确认 + 可选自动 dump；任何 `purge` 都不允许作用于 `$DATA` 之外。

### 10.3 一个容易被忽略的问题：PATH

给用户全局 PATH 里塞一条可写目录，等于给了所有能写这个目录的程序一个"劫持 `java` 的机会"。所以：

- 目录 ACL 收紧为"仅当前用户可写，禁用继承"。
- 默认**不改全局 PATH**，只在 `devbox shell` / 激活脚本里生效。
- 用户坚持要全局时，明确提示风险并把该目录放在 PATH **尾部**（宁可被系统版本抢，也不劫持系统命令）。

---

## 11. 稳定性设计

| 机制 | 解决什么 |
|---|---|
| 数据目录独占锁 | 多窗口 / 多进程同时写 store 的竞态 |
| journal + 启动 reconcile | 半装状态、幽灵实例、残留 staging |
| `.devbox-ok` 标记 | "看起来装好了但缺文件"的假成功 |
| 状态机而非布尔值 | UI 能区分"下载中/校验中/已装/损坏" |
| 预检磁盘空间（需求 × 2.5） | 装到一半空间不足导致 store 被写坏 |
| 单组件锁 + 全局并发上限（默认 2 个下载） | 一次点十个组件把带宽和磁盘打满 |
| Doctor 自检 | 端口占用、PATH 实际生效项、制品哈希抽检、孤儿进程、磁盘占用 |
| 版本化 store 布局 + 迁移脚本 | 插件自身升级不毁老数据；留 `format_version` 并支持降级读 |
| 引擎级日志 + 事件时间线 | 出问题能回答"谁在什么时候改了什么" |

**恢复原则**：宁可"启动时报错并给出修复建议"，也不要"自动猜着修"。自动修复在环境管理工具里通常会把用户的数据搞坏。

---

## 12. 与 DBX 的集成面

这是这个插件区别于"独立安装器"的地方。

| 贡献点 | 用法 | 价值 |
|---|---|---|
| `workbench`（tab） | 主控制台：目录 / 已安装 / 实例 / 任务 / 日志 / 诊断 | 主入口 |
| 同一 workbench，`surface=window` | 浮窗形态渲染成"服务托盘"：只显示运行中实例 + 启停按钮 | 开发时随手可见，不占标签页 |
| `mcp` | 暴露 `devbox.install / start / stop / status / doctor` 给 AI | 让 AI 自己把环境备齐 |
| `context-menu`（连接上） | "用本地 dev-box 复刻这个连接"：读出 host/port/库名，在本机拉一个同版本实例 | 从"管理环境"变成"复制环境" |
| `connection-provider` | 把已创建的 MySQL/Redis 实例直接变成一个可点的连接入口 | 装完即连，少一次手填 |

> **待验证**：DBX 是否允许插件在运行期直接创建/写入连接条目（`host.connections:read` 是只读的）。如果不能，降级方案是生成连接 URL / 由 `executeCommand` 触发宿主原生"新建连接"并预填参数。这一点要在 M0 就用真实宿主验证，不要等做到一半才发现走不通。

> **已澄清（原先的担心不成立）**：`host.network` 的域名白名单只约束**沙箱 iframe** 发出的请求。dev-box 的所有下载都发生在原生 sidecar 里，不走沙箱网络栈，因此不受"≤8 个域、仅 https"的限制，镜像源可以自由扩展。代价是：**网络出口的安全责任全在自己身上**——sidecar 必须自己做 https 强校验、sha256 钉死、重定向只允许同域跳转。这反而简化了架构：UI 一个网络权限都不用申请。

---

## 13. RPC 协议草案

前端 → sidecar，全部走 `dbxPlugin.invoke(method, params)`：

```
devbox/catalog            → { revision, components:[{id, kind, versions[], installed[]}] }
devbox/plan               { requirement }            → { steps, downloads, totalBytes, conflicts[] }
devbox/install            { component, version }     → { jobId }        # 异步，立即返回
devbox/uninstall          { component, version, purgeData? } → { jobId }
devbox/activate           { component, version }     → { ok }
devbox/job.get            { jobId }                  → { phase, percent, bytes, speed, error? }
devbox/job.cancel         { jobId }
devbox/instance.create    { component, version, name?, port? } → { instanceId }
devbox/instance.start     { instanceId } | .stop | .restart
devbox/instance.list      → [{ id, component, version, port, state, health, uptime }]
devbox/logs.query         { instanceId, since, limit } → { lines[] }
devbox/instance.persist   { instanceId, mode: session|detached|autostart } → { registered: bool }
devbox/runtime.info       → { mode: stdio|proxy, daemonPid, platform, formatVersion }
devbox/doctor             → { problems:[{ level, code, message, fix }] }
```

sidecar → 前端（事件，前端用 `onEvent` 订阅，断线后可凭 `jobId` 重放）：

```
evt/job.progress     { jobId, phase, percent, bytesPerSec, eta }
evt/instance.state   { instanceId, state, detail }
evt/log              { instanceId, stream, line }        # 批量发送，避免刷爆
evt/catalog.updated  { revision }
```

**异步是必须的**：安装一个 MySQL 要几分钟，如果用同步请求，用户切一下标签页、iframe 重载，整个进度和结果就全丢了。Job 活在 sidecar 里，UI 只是观察者，重连后 `job.get` 就能接上。

---

## 14. 工程结构

```
dev-box/
├── manifest.json                 # 贡献点：workbench / mcp / context-menu / connection-provider
├── dbx-plugin.toml               # include / dev 命令
├── ui/                           # 沙箱 iframe，原生 ESM（M0 零构建，M1 再上 Vite）
│   ├── index.html
│   └── src/{views,components,rpc,theme}
├── backend/
│   ├── core/                     # 纯逻辑，无 SDK 依赖，可离线单测 ← 先写这里
│   │   └── src/{lib,platform,recipe,job,safe_path,runtime,volume,mirror,dep}.rs
│   ├── engine/                   # 真实 IO：下载、换源、校验、安全解压、原子提交
│   │   └── src/{lib,fetch,checksum,extract,store}.rs + bin/install.rs
│   └── sidecar/                  # 薄壳：SDK 接入 + stdio 转发到 daemon
│       └── src/main.rs
├── catalog/                      # 配方 YAML（随插件发首版，之后独立更新）
└── dist/                         # 构建产物，不进 Git
```

**为什么 core 和 sidecar 拆开**：core 不引用 `dbx-plugin-sdk`，就能用 `cargo test` 离线跑全部逻辑（事务状态机、路径安全、平台选择、配方解析）。sidecar 只有几十行胶水，出问题时也容易定位是"引擎错"还是"协议错"。这是三平台并进时唯一能控制测试成本的组织方式。

技术选型建议（全部纯 Rust，无运行时依赖，单文件分发）：

`tokio`（异步 IO）· `reqwest + rustls`（下载，支持 Range）· `sha2`（校验）· `zip` / `tar` + `flate2`（解压）· `rusqlite`（状态库，开 WAL）· `fs2`（文件锁）· `keyring`（OS 凭据库）· `serde`（协议与配方）· `ed25519-dalek`（签名校验）

包体积预期：UI < 1 MB，sidecar 每平台 8–15 MB。因为必须有 sidecar，**只能按平台发包**，纯 `universal` 包不可用。

---

## 15. 落地路线（三平台并进）

并进不等于三份代码，而是**一条流水线、三个平台适配器**。CI 从 M0 就按 `windows-x64 / darwin-arm64 / darwin-x64 / linux-x64` 四目标出包，任何一天主分支不可构建就是红灯——这是并进唯一可行的纪律。

| 阶段 | 范围 | 完成标志 |
|---|---|---|
| **M0 骨架** | manifest + UI 壳 + sidecar 握手 + Job 模型 + 平台抽象 + 路径安全；只支持 JDK | 三平台都能装上 JDK；重连后进度不丢 |
| **M1 引擎** | 下载/校验/解压/store/激活全链路；JDK、Maven、Redis | 切版本 < 100 ms；断网重试可用；三平台 `current/` 链接正确 |
| **M2 服务与常驻** | 实例模型、端口、监管、探活、日志、崩溃恢复、**daemon 双进程 + 三态常驻**；MySQL、Nginx | 两个 MySQL 实例并行；关掉 DBX 服务仍在；取消常驻后注册项被清干净 |
| **M3 编排** | 依赖求解、环境包、`devbox.lock`、数据库初始化驱动；catalog 随包发布 | 一条命令拉起 java 后端全家桶，Nacos 自动带 JDK、xxl-job 自动建库 |
| **M4 生态** | MCP、连接托管、Doctor、团队共享 | AI 能自己备环境；三平台行为一致 |

两个前置验证（放在 M0，不通就改架构）：

1. §12 的连接创建到底允不允许（`host.connections:read` 只读）。
2. ~~`host.network` 白名单会不会卡住镜像~~ → 已澄清：下载在原生 sidecar 里，不走沙箱网络栈，白名单不适用（见 §12）。但 sidecar 需要自建出站安全策略（https 强校验、重定向同域、sha256 钉死）。

M2 之前必须补一个决策：**Linux 的支持边界**（见 §9.4），这直接决定 catalog 里哪些组件要在 Linux 上显示"不可用"。

---

## 16. 决策记录

已定：

| 决策 | 结论 | 架构后果 |
|---|---|---|
| 平台范围 | **三平台并进** | CI 四目标出包；`platform/` 从 M0 就有；Linux 支持边界必须显式声明 |
| 服务常驻 | **用户可勾选** | sidecar 拆成 stdio + daemon 双进程；引入 attach 协议与本地鉴权；实例增加三态 |
| 运行后端 | **本机 + 容器双后端，按实例选择** | `RuntimeBackend` 两实现；`Pull` 阶段；镜像 digest 钉死；后端创建后不可切换 |
| 镜像加速器 | **用户自配优先，预设兜底** | 只换 origin 不换 path；404 不降级；不写用户 daemon 配置 |
| 容器数据位置 | **默认 bind mount，可选命名卷** | 宿主路径限制在 `$DATA/instances` 内；容器内路径黑名单；选卷时删除必须先导出 |
| docker compose | **不用** | 组合环境由自己的依赖图 + Job 编排；环境包内共用 `devbox-<pack>` 网络 |
| Java 系依赖 | **我们管** | 依赖求解进 core；实例创建时钉死依赖版本；数据库初始化走驱动 |
| catalog 通道 | **随插件发版，走商店审核** | 包签名即目录签名，砍掉独立签名体系；新增版本需等审核，首发一次列全 |

仍待定（不阻塞 M0/M1）：

1. **企业内网分发**：catalog 随包发放后，私服/内网镜像场景怎么走？可能需要"企业自签 catalog"作为后续能力。
2. **Linux 支持边界**（§9.4）：决定 catalog 里哪些组件在 Linux 上显示"不可用"。

---

## 17. M0 交付状态

已落地并通过验证：

| 项 | 结果 |
|---|---|
| `devbox-core` 单元测试 | **59 passed**（状态机 9、平台 7、路径安全 11、运行后端 7、卷规划 6、镜像源 6、依赖求解 8、激活规划 5） |
| `devbox-engine` 单元测试 | **29 passed**（校验 3、解压 6、换源 3、目录 8、续传 2、激活 4、入口 3） |
| 真实下载链路 | Maven 3.9.9 / 9.2MB / 90 文件，官方 sha512 校验通过 |
| 目录驱动 | 只给 `--catalog` + 组件名 + 版本，URL / 校验 / 解压方式全部来自 YAML |
| 占位符拦截 | `jdk 21.0.5` 在解析期即拒绝，报"校验值仍是占位符"，**一次网络请求都没发** |
| 断点续传 | 预置 4MB 分片后续传成功（从 3.81MB 接上），最终全量 sha512 通过 |
| 产物可用性 | `mvn -v` 输出 `Apache Maven 3.9.9`，`Maven home` 指向 store 目录 |
| 换源降级 | 用户加速器 DNS 失败 → 自动落到原始地址并成功 |
| 404 不降级 | 镜像返回 404 → 立即失败，不做无谓重试 |
| 校验失败 | 篡改校验值 → 拒绝落盘，`store` 与 `tmp` 均无残留 |
| RPC 端到端 | JSONL 触发安装，事件按百分比节流，`job.get` 最终 `ready`；占位符组件直接 `failed` |
| 第二个真实组件 | Node.js 22.11.0（31MB / 2521 文件），只改 YAML 未改代码，`node -v` 输出 `v22.11.0` |
| 版本切换 | Maven 3.8.8 ↔ 3.9.9 三次切换，`current/maven/bin/mvn -v` 报告对应版本 |
| 命令行入口 | `$DATA/bin` 下生成 mvn/node/npm/npx 四个入口；切版本后**未重新生成**，`mvn -v` 直接跟着变 |

还没做（不要当成已完成）：

- **只有 Maven 一条配方被真正验过**。其余组件在 catalog 里仍是 `<REPLACE_ME>`，会被拒绝安装——这是设计意图，不是遗漏。
- `ContainerBackend` 只有规划逻辑（卷、端口、镜像源），没有真正调用 `docker`。
- 没有 daemon 模式，`detached` / `autostart` 三态在 UI 上还不可用（M2）。
- UI 的组件列表仍是 mock，还没从 `devbox/catalog` 读真实目录（所以界面上看起来"什么都能装"）。
- 执行位的**判断逻辑**有单测，但真正 `chmod` 的那一步没在 Linux/macOS 上跑过——三平台里只有 Windows 被实测。
- 未在真实 DBX 宿主里加载过：`window.dbxPlugin` 的事件形状、`dbx-plugin.toml` 字段、
  sidecar 首帧格式三处仍需对真实宿主校准（core 与 UI 不因此改动）。

两个集成阶段真实抓到的 bug（都值得记下来，因为类型系统抓不住）：

1. **`Commit` 被当成可取消阶段**。提交正在做"写标记 + 原子改名"，此刻接受取消只会留下残骸。已从 `cancellable()` 里排除。
2. **下载阶段没把状态机推进到 `Download`**。于是结尾的 `Plan → Verify` 是非法跳转，被静默忽略，`job.get` 永远停在 `plan`——而 UI 上看到的是"进度 100% 却还在解析"。现在 Plan 之后显式进 Download，并且 `set_phase` 遇到非法跳转仍然静默（这是对的），靠端到端日志才发现。

第三个不算 bug 但值得记：解压时把 `/etc/passwd` 静默转成相对路径虽然安全，但属于"放过可疑输入"。改成直接拒绝并记录。

---

## 18. 前端信息架构

设计依据是"开发者打开它时想干什么"，而不是"插件有哪些功能"。打开这个插件的高频意图是看服务跑没跑、端口多少，所以首页是环境而不是目录。

| 视图 | 回答的问题 | 关键元素 |
|---|---|---|
| 概览 | 我的环境现在什么样 | KPI（运行中/已装版本/磁盘/任务）、一键环境包、工具链激活下拉、服务状态表 |
| 服务 | 实例怎么管 | 新建实例表单、启停/重启、端口、常驻三态、行内日志、连接串复制 |
| 组件 | 还能装什么 | 分类过滤 + 搜索、版本下拉、已装标记、平台不可用的明确提示 |
| 诊断 | 哪里出了问题 | 问题分级 + 修复建议，只报告不自动修 |

六条具体决定：

1. **组件目录降级为次要入口**。上一版把它当首页，等于让"装新东西"这个低频动作占据了最高频的屏幕。
2. **任务区改成抽屉**。默认只有一条状态栏，有任务才展开；不再永久吃掉 168 px 高度。安装是后台行为，不该长期占据视觉中心。
3. **常驻三态直接放在实例行的下拉里**，而不是藏进设置页。它是实例的属性，改的时候应该能看见自己在改哪一个。
4. **平台不可用要说人话**。例如 Windows 上的 Redis 直接显示"官方无 Windows 发行版，不提供安装"，而不是给一个点了会失败的按钮。
5. **异常 ≠ 停止**。健康检查失败的实例进程还在跑，操作必须是"重启/停止"而不是"启动"。
6. **所有颜色走 CSS 变量 + `[data-theme="light"]` 一套亮色**。宿主下发 `theme.tokens` 时整体覆盖，组件里不写死颜色。窄窗口（浮窗形态）下左侧退成 56 px 图标栏、表格隐藏次要列。

工程上保持零构建：原生 ESM + 事件委托，重渲染不需要重新绑定监听。支持 `?view=instances` 深链，宿主或外部链接可以直接落到指定视图。

---

## 19. 容器后端（Docker 模式）

### 19.1 定位：只容器化"服务"，不容器化"工具链"

这是最容易做错的一步。把 JDK 装进容器，宿主机上的 `mvn` 和 IDE 根本用不了——容器的隔离性在这里是负担，不是优点。所以边界划死：

| 类型 | 可用后端 | 理由 |
|---|---|---|
| `toolchain`（JDK / Maven / Node） | 仅本机 | 容器里的工具链对宿主 CLI 无效，做了也是个假功能 |
| `service`（MySQL / Redis / Nginx / MQ / Nacos / xxl-job） | 本机 **或** 容器 | 服务只需要一个能连的端口，隔离反而更干净 |

如果用户想要"整套环境都在容器里"，那是 devcontainer 的形态，不是这个插件要解决的。**我们要解决的是"本机开发，依赖用容器"，不是"把开发机搬进容器"。**

顺带解决一个遗留问题：Redis 在 Windows 上没有官方发行版，容器模式正好补上——所以 Redis 的"平台不可用"提示应该改成"本机不可用，可用容器运行"，而不是死路。

### 19.2 三层模型怎么映射

原有模型不变，只是每层的实现换了一套：

| 概念 | 本机后端 | 容器后端 |
|---|---|---|
| Artifact | `store/<id>/<ver>/<platform>/` 解压目录 | 镜像引用 `repo@sha256:...` |
| Instance | 独立 `instances/<id>/`（config + data + logs） | 一个容器 + 命名卷（或 bind mount） |
| Activation | junction / symlink 指针 | 不适用（容器服务没有"激活着"的概念） |
| 启动 | spawn 子进程 + 探活 | `docker start` + `inspect` 健康态 + 同一套探针 |

引擎侧的扩展点是一个 trait，两个实现，**Instance 的数据结构与 UI 完全共用**：

```rust
pub trait RuntimeBackend {
    fn status(&self) -> BackendStatus;                       // 可用性 + 版本 + 不可用原因
    fn ensure_artifact(&self, spec: &ArtifactSpec) -> Result<ArtifactRef>;
    fn create(&self, plan: &InstancePlan) -> Result<InstanceRef>;
    fn start(&self, id: &str) -> Result<()>;
    fn stop(&self, id: &str) -> Result<()>;
    fn remove(&self, id: &str, purge_data: bool) -> Result<()>;
    fn probe(&self, id: &str) -> Health;
    fn logs(&self, id: &str, since: u64) -> Vec<LogLine>;
}
```

新增的是实现，不是分支。任何 `if runtime == docker` 出现在业务层，都是设计没做干净。

### 19.3 供应链：镜像比 tarball 更需要钉死

tar 包的 sha256 是"这份字节"，镜像的 tag 却是"一个可能被重新指向的指针"——`mysql:8.4` 今天和明天可以是不同镜像。所以：

- catalog 里写 **digest**（`mysql@sha256:...`），不写 tag；tag 只做展示用。
- registry 白名单：只允许 catalog 声明的来源，用户不能临时填一个新 registry。
- 拉取后 `docker image inspect` 复核 digest，与 catalog 不一致直接终止。

**镜像加速器（决策：用户自己配，预设兜底）**

加速器只改"从哪个域名取"，不改"取到什么"。所以规则是一句话：**只换 origin，不换 path，不换校验**。

```
原始: https://registry-1.docker.io/v2/library/mysql/blobs/sha256:8f2c...
换源: https://<用户配置或预设>/v2/library/mysql/blobs/sha256:8f2c...
                              ↑ 后面这一段一个字节都不能变
```

降级判定必须分清"源坏了"和"东西不存在"，否则会把同一个错误重复三遍还拖慢失败反馈：

| 结果 | 处理 |
|---|---|
| 超时 / 连不上 / TLS 失败 | 换下一个源 |
| 403 / 408 / 429 / 5xx | 换下一个源 |
| **404 / 401 / 400** | 直接失败——版本或路径本来就错了，换源只会掩盖问题 |

候选顺序：用户配置的源 → 内置预设（去重、丢掉非 https 的）。预设的作用不是"更好"，而是用户填的加速器过期时安装不至于直接失败。

加速器本身**不写进用户的 Docker daemon 配置**，那是引擎级设置，我们只读、只提示。

### 19.4 数据放哪：默认挂载出来，可选放进卷里

决策（2026-10-27）：**默认 bind mount 到实例目录**。宁可慢一点，也不让用户找不到自己的数据——"看不见的卷"是同类工具最容易让人丢库的地方。

| 方案 | 优点 | 代价 |
|---|---|---|
| **bind mount**（默认） | 数据可见、可直接备份、两个后端共用一套目录 | Docker Desktop 的文件共享层慢，数据库尤其明显 |
| Docker 命名卷（可选） | 快一个数量级 | 数据在 Docker 目录里，删容器即消失，必须靠导出 |

既然选了默认慢的那条，就必须把代价说在装之前，而不是让人装完觉得卡：

1. 新建实例时的提示原话里写明"**Docker Desktop 上数据库会明显变慢，介意就换成卷**"——把权衡交给用户，别替他们隐瞒。
2. 选了卷的，删除实例**不允许静默删卷**：要么先 `devbox export` 导出成 tar 落到 `$DATA/backups/`，要么显式勾选"同时删除数据"。
3. 卷名固定为 `devbox-<instanceId>`，实例日志里直接打印数据实际位置，不用去猜。

**宿主路径必须落在 `$DATA/instances` 之内**，这是一条硬约束：否则配方里写个 `/etc` 就能把系统目录挂进容器，等于把沙箱交给 catalog 的作者。容器内路径同时做黑名单——`/proc`、`/sys`、`/dev`、`/etc`、`docker.sock`、`/var/lib/docker` 一律拒绝。

### 19.5 常驻三态在容器里变简单了

容器有原生的重启策略，不需要我们再写守护：

| 模式 | `--restart` | 附加要求 |
|---|---|---|
| `session` | `no` | DBX 退出时我们主动 stop |
| `detached` | `unless-stopped` | 无 |
| `autostart` | `always` | **Docker Desktop 本身要开机自启**，这一项我们改不了，检测到没开就明确提示 |

注意一个容易被忽略的事实：Windows / macOS 上"Docker 引擎开机自启"是 Docker Desktop 的设置项，不在我们的控制范围。Linux 上则是 `systemctl enable docker`。所以勾了"开机自启"但容器起不来，八成是引擎没起来——诊断页要把这条列进固定检查项。

### 19.6 安全基线（比本机模式更严格）

拉一个镜像等于执行别人的构建产物，所以默认参数就必须是收着的：

- 端口一律 `-p 127.0.0.1:<host>:<container>`。**绝不** `-p 3306:3306`——那等于把数据库发布到 `0.0.0.0`，同网段任何人都能连。
- 禁止：`--privileged`、`--network host`、挂载 `/var/run/docker.sock`、挂载 `$HOME`、`--pid host`。
- 默认 `--cap-drop ALL`，只按配方白名单加回必要能力；能非 root 就用镜像自带的非 root 用户。
- 不代用户 `sudo`。Linux 上如果当前用户不在 `docker` 组，只提示"把用户加入 docker 组后重新登录"，不弹密码框。
- 日志走 `docker logs --since`，不落盘到宿主任意路径。

### 19.7 混合模式的两个真实冲突

1. **端口**：一个本机实例占了 3306，一个容器实例也想用 3306 —— 端口分配器必须同时看宿主进程监听和 `docker ps` 的发布端口，不能各算各的。
2. **后端不可中途切换**：一个实例创建时定了本机还是容器，就不允许原地改。要换后端 = 新建实例 + 导数据（`dump` → 导入）。假装能无缝切换，最后一定丢数据。

### 19.8 UI 上怎么表达

- 新建实例的表单里加一个二选一：**本机 / 容器**，默认跟随全局偏好。容器不可用时该选项禁用并给出原因，不给一个点了会失败的按钮。
- 实例表加一列"运行方式"，用 `本机` / `容器` 小标签，一眼能分辨。
- 组件卡片上加"支持容器"标记；本机不可用但有镜像的组件（Redis on Windows），文案改成"可用容器运行"而不是"不可用"。
- 侧栏底部显示 Docker 引擎状态（可用 / 未检测到 / 版本），因为它是容器模式的硬前提。

### 19.9 配方要多一段

```yaml
id: mysql
runtime:
  native:
    artifacts: { windows-x64: {...}, darwin-arm64: {...}, linux-x64: {...} }
    install: [ ... ]
  container:
    image: mysql
    digest: sha256:8f2c...            # 钉死，不接受只有 tag
    registries: [docker.io, registry.example.com]
    ports: [{ container: 3306, host: "{{port}}", bind: 127.0.0.1 }]
    env: { MYSQL_ROOT_PASSWORD: "{{secret.root}}", MYSQL_DATABASE: "{{database}}" }
    volumes:
      # 默认：挂到实例目录，用户可备份；选"用卷"时由引擎改写成 volume 形式
      - { mode: bind, host: "{{data_root}}/instances/{{instance_id}}/data", path: /var/lib/mysql }
    options: { cap_drop: [ALL], user: "999:999" }
    health: { exec: ["mysqladmin", "ping", "-h", "127.0.0.1"] }
```

一份配方同时描述两种后端，UI 只展示当前平台/后端能用的那一种。缺 `container` 段的组件，容器模式里就不出现。

### 19.10 明确不用 docker compose

决策（2026-10-27）：**不引入 compose**。组合环境（Nacos + MySQL + xxl-job）由我们自己的编排层按依赖拓扑逐个创建容器，理由有两条：

1. compose 会引入一个外部依赖，用户的 compose 版本差异会变成我们的兼容问题；而我们已经有了 Job + 依赖图这套编排，没必要再学一套文件格式。
2. 编排权在我们手里，才能做"装到一半失败就回滚到干净状态"——compose 的失败态往往留下一堆命名容器和网络，清理反而更麻烦。

代价是要自己处理容器间网络（一个环境包内共用一个 `devbox-<pack>` 网络，服务名互相可达），这块工作量不小，但可控。

---

## 20. 依赖与数据库初始化

决策（2026-10-27）：**Java 系组件的依赖由我们管**。Nacos 要 JDK 17+、xxl-job-admin 还要一个建好表结构的 MySQL——"你先自己装个 JDK"就不叫一键了。

### 20.1 依赖图随后端变化

这是最容易被写成固定图的地方。规则是：

| 依赖类型 | 本机后端 | 容器后端 |
|---|---|---|
| 运行时依赖（`jdk`） | **需要**：宿主上必须有对应版本 | **不需要**：镜像自带 JRE |
| 服务依赖（`mysql`） | 需要 | 需要，且可以是另一个容器的实例 |

所以 `nacos@docker` 不拉 JDK，`xxl-job@docker` 仍然要准备 MySQL。依赖求解拿"后端"当输入，不是常量。

### 20.2 实例创建时把依赖钉死

全局激活指针是给 CLI 用的，**不能成为运行中实例的依赖**。否则用户为了跑老项目把 JDK 切回 8，正在跑的 Nacos 会跟着挂掉。

做法：创建实例时求解一次，把结果冻结进 `instance.json`：

```json
{
  "requires_resolved": { "jdk": "17.0.13", "mysql": "8.4.3" },
  "requires_host": { "jdk": { "home": ".../store/jdk/17.0.13/windows-x64" } }
}
```

之后启动只用这份快照，全局怎么切都不影响它。想换依赖版本 = 新建实例（和"后端不可中途切换"是同一套原则）。

### 20.3 冲突要给人能读的链路

```
nacos 需要 jdk >=17 <22，legacy-app 需要 jdk >=8 <17，两者无交集；可选版本：8.0.432, 17.0.13, 21.0.5
```

而不是一句 `version conflict`。这条信息直接展示在安装失败的提示里，用户才知道该放弃哪个。

### 20.4 数据库初始化：五步，可重入

以 xxl-job-admin 为例：

1. 确认目标 MySQL 实例健康（不是"端口通"，是握手成功）。
2. 生成随机密码，写进 OS 凭据库，**不进日志**。
3. 建库：库名带实例 id（`devbox_xxl_job_<instance>`），先查存在性再建，可重入。
4. 执行组件自带的初始化 SQL，配一张 `devbox_schema_version` 表记录已执行的迁移，重复执行不会炸。
5. 渲染应用配置（连接串、库名、密码）→ 启动 → 探活。

**失败处理**：初始化失败时删掉**本次刚创建的** schema，保留日志。理由很简单——这个库里此刻只有我们刚灌进去的半截数据，留着比删掉更危险；但如果 schema 是先前就存在的（用户已有数据），一律不动，只报错。

**SQL 权限边界**：catalog 里的 SQL 是签名的，可以执行，但只允许作用于实例自己的 schema（`CREATE DATABASE` / `USE` / `DDL` / `INSERT`）。禁止跨库操作、禁止 `GRANT`、禁止 `DROP DATABASE` 其他库。这条限制写在 SQL 执行驱动里，不靠配方自觉。

### 20.5 环境包的网络

同一个环境包内的容器共用一个 `devbox-<pack>` 网络，容器之间用服务名互相解析（`mysql:3306`），宿主机只看到回环端口。跨环境包的实例不共享网络，避免"某个包里能连到另一个包的库"这种隐蔽耦合。

一个附带的判断：**初始化 SQL 不写在配方里，放在组件的制品里**（xxl-job 的 jar 包内自带 schema）。配方只声明"要执行 init"，具体 SQL 跟着上游版本走，升级时不会因为配方里的 SQL 过期而建出错的表。

---

## 21. 下载的边界情况

### 21.1 断点续传：三种返回，三种处理

`dest.part` 已存在就带 `Range` 头去要剩下的部分。服务端可能给三种回答，每种都要有明确动作：

| 返回 | 含义 | 处理 |
|---|---|---|
| `206` + `Content-Range: bytes N-M/T` | 正常续传 | 校验 N 等于本地长度后追加写 |
| `200` | 服务端忽略了 Range | **从头覆写**，绝不能把两段拼起来 |
| `416` | 本地分片已失效 | 删掉分片重来 |

`Content-Range` 起点与本地长度不一致时同样丢弃分片——拼接错误比重新下载代价大得多。

**分片的去留要分清**：内容校验失败 → 删除（多半是源上就不对）；网络中断 → **保留**（否则下次还得从头下）。一刀切删掉等于白续。

**最终校验仍然是全量的。** 上游没有发布分片哈希，所以"边下边校验"做不到；能保证的是：拼完的整文件必须匹配 catalog 里的 sha512。断点续传提高的是可用性，不降低校验强度。

### 21.2 执行位：判断要和执行分开

zip 和 tar 对权限的处理不一样，tar 有 mode，zip 大多数条目没有。规则：

1. 归档里本来就带执行位 → 保留为 `0755`
2. 没有 → 看路径：落在 `bin/`、`sbin/` 下，或以 `.sh` 结尾 → `0755`
3. 其余保持 `0644`

一律清掉 setuid/setgid/sticky——这类位在开发机上只有害处。

判断逻辑抽成纯函数 `exec_mode_for(path, archive_mode)`，这样在 Windows 上也能跑单测；真正 `chmod` 的那一步才 `#[cfg(unix)]`。**只有纯函数被测过，实际 chmod 未经运行验证**——这一点必须说清楚，不能因为"测试通过"就当成三平台都验过。

### 21.3 目录解析的三条拒绝规则

| 情况 | 行为 |
|---|---|
| 组件或版本不存在 | 直接失败，列出可用版本 |
| 当前平台没有制品 | 直接失败，列出可用平台（**不拿别的平台凑数**） |
| 校验值是 `<REPLACE_ME>` | 直接失败，报"这份配方还没被真正验过" |

第三条是故意的：没验过的配方宁可装不上，也不能装上去之后才发现内容不对。

---

## 22. 激活：切换版本

### 22.1 只改一个链接

激活不复制、不移动、不重新解压任何东西，只把 `current/<component>` 这个入口重新指到
`store/<component>/<version>/<platform>`。所以切换是文件系统操作里最便宜的一种。

两级事实，各有各的用处：

| 载体 | 谁用 | 失效后果 |
|---|---|---|
| `current/<component>`（junction / symlink） | 进程执行、PATH 解析 | 程序找不到命令 |
| `current/<component>.txt`（指针文件，人能读） | 界面展示、排障、回滚 | 只是显示不准 |

指针文件不是冗余：链接可能因为权限、外部操作、跨版本迁移而失效，而指针是我们自己写的文本，
出问题时打开就能看到"上次激活到哪"。

### 22.2 Windows 上踩到的三个坑

都是实测出来的，不是文档里能读到的：

1. **不能拿 `std::fs::remove_dir` 删 junction**，会返回 `拒绝访问 (os error 5)`。
   必须走 reparse point 的删除路径（`junction::delete`）。
2. **删完之后还会残留一个悬空条目**。不清掉的话，紧接着创建新链接会直接失败——表现成
   "切换失败：拒绝访问"，实际是"目标已存在"。
3. **junction 在 `symlink_metadata` 里被识别成 symlink（`is_dir == false`）**。
   所以清理时必须按 `is_file/is_dir` 分开判断，不能想当然当成目录删。

### 22.3 失败要回滚

激活的三步是"删旧链接 → 建新链接 → 写指针"。中间任何一步失败，都要把旧链接接回去，
不能让用户停在"没有当前版本"的状态。

实测场景：激活一个**目录存在但没有 `.devbox-ok`** 的版本 → 直接拒绝，且当前版本保持不变。

### 22.4 实测结果

| 动作 | 结果 |
|---|---|
| 装 Maven 3.8.8 / 3.9.9 两个版本 | 各自独立目录，互不影响 |
| 3.8.8 → 3.9.9 → 3.8.8 连续切换 | 每次 0.0s（进程启动占绝大部分） |
| 通过 `current/maven/bin/mvn -v` 执行 | `Apache Maven 3.8.8`，`Maven home` 指向 `current/maven` |
| 激活未完成的制品 | 拒绝，并保留原当前版本 |

### 22.5 还没做的

- **PATH 里只有一个组件能生效** → 已由 §23 的 shim 目录解决。
- 没有"项目级覆盖"（进某个目录自动切版本），那是 v2 的方案。
- 没有并发保护：两个进程同时激活同一组件，后写的赢。当前依赖"只有一个 sidecar 在写"这条前提。

---

## 23. 命令行入口（shim）

### 23.1 问题

jdk、maven、node 的程序分别在 `current/<component>/bin` 下。用户不可能把三条路径都加进
PATH，更不可能每次切版本都改一遍。

### 23.2 方案：一层固定的转发目录

`$DATA/bin` 永远不变，里面每个文件都通过 `current/<component>/` **间接**指向真实程序：

```
$DATA/bin/mvn.cmd  →  ..\current\maven\bin\mvn.cmd  →  store/maven/3.9.9/windows-x64/bin/mvn.cmd
                                  ↑ 只有这一段指针会变
```

所以：**PATH 只需配置一次，切换版本后入口自动跟着走。** 实测同一个 `mvn.cmd`：

```
（切到 3.9.9）mvn -v → Apache Maven 3.9.9
（切到 3.8.8，未重新生成入口）mvn -v → Apache Maven 3.8.8
```

### 23.3 三个实现细节

1. **Windows 用 `.cmd` 转发，Unix 用相对符号链接**。Windows 上 `call "%~dp0..\current\..." %*`
   再接 `exit /b %errorlevel%`，既能转发 `.exe` 也能转发 `.cmd`，退出码不丢。
2. **要查两处目录**。多数组件把程序放在 `bin/`，但 Node 与 Nginx 的 Windows 包直接放在
   包根目录。只查 `bin/` 会让 node"装好了却没有入口"——这个坑踩过一次。
3. **整目录重建，不做增量**。这个目录完全归我们所有，重建是幂等的，还能顺手清掉
   已卸载组件留下的死入口。

### 23.4 不动用户的 PATH

这是"不做"清单里的第一条：不自动修改 shell 配置、注册表、环境变量。

所以只做两件事：把目录准备好，把这行命令原样打印出来：

```
# bash / zsh
export PATH="$DATA/bin:$PATH"

# Windows 当前会话
$env:PATH = "$DATA\bin" + [IO.Path]::PathSeparator + $env:PATH
```

副作用是我们**无法判断用户到底有没有加**。界面上的措辞因此是"把这一层加进 PATH 后…"，
而不是"已配置"——只能说事实，不能假设。

### 23.5 限制

- `.cmd` 转发用 `%*` 原样透传，不做参数重新引用。带复杂引号的参数可能被 cmd 的解析规则
  改变，这种情况建议直接调用 `current/` 下的原始路径。
- 没有提供 `.ps1` 转发；壳层靠 PATHEXT 找到 `.cmd` 就够了。
- shim 只覆盖"全局当前版本"，还没有按项目切换的能力。

---

## 24. 插件包的硬约定

这一节是从宿主安装器的行为反推出来的——不满足就装不上，而且报错不会解释为什么。

### 24.1 三条硬约定

| 约定 | 不满足的表现 |
|---|---|
| `manifest.entrypoints.backend.executable` 必须指向包内**真实存在**的文件，官方布局是 `bin/<target>/<binary>[.exe]` | `Plugin backend executable does not exist`，整个包被判为 incompatible |
| 包根必须有 `checksums.json`，且 `files` **精确覆盖**除自身与 `signature.json` 之外的所有文件 | `Plugin package is missing checksums.json` / `Plugin checksums do not cover the package exactly; missing=...` |
| zip 条目要带 Unix 权限位（bin 下 `0o755`，其余 `0o644`，`create_system=3`） | Unix 上侧车解出来是 `0o644`，**没有执行位，起不来**，而且不报错 |

第二条是最容易踩的：宿主不做任何路径替换，我们之前写 `bin/devbox-sidecar.exe`、实际文件也放那儿，
但官方约定是套一层 target 目录。**名字对不上就是 incompatible，不会帮你补 `.exe`，也不会补 target 段。**

### 24.2 checksums.json 的形状

```json
{
  "algorithm": "sha256",
  "files": {
    "manifest.json": "<hex>",
    "bin/windows-x64/devbox-sidecar.exe": "<hex>",
    "ui/index.html": "<hex>",
    "assets/icon.svg": "<hex>"
  }
}
```

- `algorithm` 目前只认 `sha256`（其它值会报 `Unsupported plugin checksum algorithm`）。
- 覆盖范围是"包内除 `checksums.json` / `signature.json` 之外的全部文件"，**多一个少一个都失败**。
  所以我们打包时是先收集文件、算哈希、最后把 checksums.json 自己追加进去。

### 24.3 未签名包

`signature.json` 可以省略，但只能在插件中心打开「允许安装未签名开发包」之后安装。
签名算法是 Ed25519，签名对象是 checksums 清单。我们目前不签名——**上架前必须走官方流程**。

### 24.4 我们的实现

`scripts/package.py` 一次做完：重写 manifest 的 executable → 收集文件 → 生成 checksums.json →
按 Unix 模式压 zip → **自检覆盖是否精确** → 顺带产出 `<pkg>.artifact.json`（target / url / sha256 / size）。

自检不是走过场：`verify_dbxp()` 会重新打开 zip，比对"包内文件集合"与"checksums 声明的集合"
是否完全相等，不相等就返回非零退出码。这样这类错误在本地就被拦住，不用等宿主报错。

顺带修了一个联动问题：可执行文件进了 `bin/<target>/` 之后，侧车再按 `exe 同级/../catalog` 找目录就错了。
现在改成从 exe 所在目录**向上最多走四层**找 `catalog/`，包内布局和源码内布局都能命中。

---

## 附：几个明确的"不做"

- **不做**"自动修改用户全局环境变量、注册表、shell 配置文件"，除非用户单次明确点击。
- **不做**"自动升级已装组件"。环境管理的可预测性比新鲜度重要。
- **不做**"从任意 URL 安装"。宁可少支持一个私服场景，也不开这个口子（私服需求用"企业 catalog 签名"解决）。
- **不做**"自动修复损坏状态"。报错 + 给出可执行的修复命令，比自作聪明安全一个数量级。
