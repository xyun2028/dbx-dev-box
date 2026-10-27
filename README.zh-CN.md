# dev-box

一个 [DBX](https://dbxio.com) 插件：一键搭好本地开发环境——JDK、Maven、MySQL、Redis、Nginx、
RocketMQ、Nacos、xxl-job-admin，多版本共存，切换只要毫秒级。

[English](./README.md)

> **当前状态：早期。** 下载链路是真实的、端到端验证过的；但除了 Maven，其余组件还装不上——
> 因为目录里的校验值仍是占位符。这是刻意为之，原因见下文。

---

## 为什么再造一个环境管理工具

三个判断决定了整个设计：

1. **制品、实例、激活是三件事。** 一个版本是一份永不修改的只读目录；一个实例是可以随意
   创建销毁的数据与配置；激活只是一个指针。把三者混成一个"已安装版本"，就必然在"切换版本"
   这件事上出错。
2. **目录是数据，不是代码。** 新增一个组件应该只写 YAML，不碰引擎。引擎不认识 MySQL。
3. **装软件是供应链行为。** 一切按校验值钉死，压缩包当敌意输入处理，任何东西都不经过 shell。

架构文档：[`docs/plugin-architecture.md`](./docs/plugin-architecture.md)。

---

## 当前状态

| 能力 | 状态 |
|---|---|
| 下载 → 校验 → 解压 → 原子提交 | **可用**，用真实字节验证过 |
| 断点续传 | **可用**（206 追加 / 200 覆写 / 416 丢弃） |
| 校验值强制 | 支持 sha256 与 sha512，占位符在解析期拒绝 |
| 换源降级 | 用户配置优先，原始地址兜底；**404 不降级** |
| 目录（YAML） | 已解析；目前只有 **Maven 3.9.9** 带真实校验值 |
| 依赖求解 | 逻辑与测试完成，尚未接进安装流程 |
| 容器（Docker）后端 | 只有规划逻辑，还没真正调用 `docker` |
| 常驻（`detached` / `autostart`） | 未实现（依赖 daemon 模式） |
| 界面 | 仍读 mock 数据，接真实目录是下一步 |

现在去装 Maven 之外的组件，会得到一句如实的报错：

```
jdk 21.0.5 (windows-x64) 的校验值仍是占位符，说明这份配方还没被真正验过
```

这是故意的：没验过的配方不该装成功。

---

## 构建与测试

需要 Rust（2021 edition）；只在改界面时才需要 Node 18+。

```bash
# 单测：共 76 个
cargo test --manifest-path backend/core/Cargo.toml
cargo test --manifest-path backend/engine/Cargo.toml

# sidecar（DBX 插件的后端进程）
cargo build --manifest-path backend/sidecar/Cargo.toml
```

### 真装一次

命令行是验证整条链路最快的入口：

```bash
cargo build --manifest-path backend/engine/Cargo.toml --bin devbox-install

./backend/engine/target/debug/devbox-install \
  --catalog catalog/jdk.yaml \
  --component maven --version 3.9.9 \
  --data /tmp/devbox-data
```

预期输出：

```
目录 rev 2026.10.27-m0 · schema 1
→ maven 3.9.9 (windows-x64)  https://...
✓ 安装完成： 90 个文件，8.78 MB
  产物目录: /tmp/devbox-data/store/maven/3.9.9/windows-x64
```

### 用 JSONL 跑 sidecar

```bash
echo '{"id":1,"method":"devbox/install","params":{"component":"maven","version":"3.9.9"}}' \
  | DEVBOX_CATALOG=catalog/jdk.yaml DBX_PLUGIN_DATA_DIR=/tmp/devbox \
    ./backend/sidecar/target/debug/devbox-sidecar
```

进度以 `evt/job.progress` 事件推送；`devbox/job.get` 让重连后的界面能接上一个不是它自己发起的任务。

---

## 目录结构

```
manifest.json            DBX 插件清单（workbench 贡献点）
dbx-plugin.toml          构建配置
catalog/                 组件配方（YAML），随安装包发布
ui/                      沙箱控制台：原生 ESM，零构建
backend/core/            纯逻辑：状态机、路径、平台、卷、依赖
backend/engine/          真实 IO：下载、校验、解压、原子提交
backend/sidecar/         DBX 侧进程：JSONL 协议 + 引擎接线
docs/                    架构文档
```

`core` 单独拆出来的原因：它**零依赖**，所以 `cargo test` 在任何机器上都能跑；真正要紧的规则
（状态跳转、路径安全、版本范围）不需要网络、不需要沙箱就能测。

---

## 安全说明

- 只允许 HTTPS。`http://` 地址在发出请求之前就被拒绝。
- 每个制品用 sha256/sha512 钉死；镜像站可以换域名，但换不了字节。
- 压缩包按敌意输入处理：逐条目拒绝绝对路径、`..`、符号链接、硬链接、Windows 设备名与
  备用数据流；解压有条目数与总体积上限。
- 命令一律以 `argv` 数组执行。没有 shell，没有字符串拼接。
- 容器端口只绑 `127.0.0.1`。拒绝挂载 `/proc`、`/sys`、`/dev`、`/etc` 以及 Docker socket。
- 不执行 `sudo`，不修改你的 shell 配置、注册表或 Docker daemon 配置。

---

## 关于镜像源的一个实测结论

做这个的时候在中国大陆网络下实测了 Maven 3.9.9 这个制品：

| 源 | 结果 |
|---|---|
| `repo.huaweicloud.com` | 约 2.4 MB/s，可用 |
| `mirrors.huaweicloud.com` | 约 1.4 MB/s，可用 |
| `archive.apache.org` | 约 20 KB/s，基本不可用 |
| 阿里云 / 腾讯云 / 中科大 / Apache CDN | 该路径全部 404 |

镜像清单必须实测，不能凭印象填。这也解释了为什么 404 不触发降级：连着四个 404 会白花八秒，
还会把真正的错误藏起来。

---

## 许可证

待定——上架商店之前必须定下来。
