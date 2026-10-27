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
- **L0 与插件版本解耦**：新增一个中间件版本，只更新 catalog，不用重新过商店审核发版。这一条决定了这个插件能不能活下去。

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
| Tier 1 配方 | 任何人 | 覆盖 90% 的中间件（下载/解压/配置渲染/启停/探活） | 提交 catalog，签名即可 |
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
│   │   └── src/{lib,platform,recipe,job,safe_path}.rs
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
| **M3 编排** | 依赖图、环境包、`devbox.lock`、catalog 独立签名与热更新 | 一条命令拉起 java 后端全家桶 |
| **M4 生态** | MCP、连接托管、Doctor、团队共享 | AI 能自己备环境；三平台行为一致 |

两个前置验证（放在 M0，不通就改架构）：

1. §12 的连接创建到底允不允许（`host.connections:read` 只读）。
2. `host.network` 的 https 白名单会不会卡住你的镜像方案（含内网私服场景）。

M2 之前必须补一个决策：**Linux 的支持边界**（见 §9.4），这直接决定 catalog 里哪些组件要在 Linux 上显示"不可用"。

---

## 16. 决策记录

已定：

| 决策 | 结论 | 架构后果 |
|---|---|---|
| 平台范围 | **三平台并进** | CI 四目标出包；`platform/` 从 M0 就有；Linux 支持边界必须显式声明 |
| 服务常驻 | **用户可勾选** | sidecar 拆成 stdio + daemon 双进程；引入 attach 协议与本地鉴权；实例增加三态 |

仍待定（不阻塞 M0）：

1. **镜像源归属**：官方 URL + 国内镜像前缀替换，谁维护这份可用性清单？镜像挂了要能降级，不能卡死安装。
2. **Java 系组件的边界**：Nacos / xxl-job-admin 要不要管它们的 JDK 依赖和 MySQL 建库初始化？管，才是"一键"；不管，就只是"下载器"。
3. **catalog 的更新通道**：独立签名仓库热更新（推荐，组件版本追得快），还是每次跟插件发版走商店审核（安全可控但慢）。

---

## 附：几个明确的"不做"

- **不做**"自动修改用户全局环境变量、注册表、shell 配置文件"，除非用户单次明确点击。
- **不做**"自动升级已装组件"。环境管理的可预测性比新鲜度重要。
- **不做**"从任意 URL 安装"。宁可少支持一个私服场景，也不开这个口子（私服需求用"企业 catalog 签名"解决）。
- **不做**"自动修复损坏状态"。报错 + 给出可执行的修复命令，比自作聪明安全一个数量级。
