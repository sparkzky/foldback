# foldback Phase 1 实现计划

> **定位**：独立 Rust CLI，参考 [RTK](https://github.com/rtk-ai/rtk) 的命令输出精简思路与 [ANOLISA Tokenless](https://github.com/alibaba/anolisa/tree/main/src/tokenless) 的可逆 Stash 协议，**不 fork RTK、不复制源码、不做透明 hook**。
> **承诺**：底层命令只执行一次；被隐藏内容 byte-exact 可恢复；命令 exit code 透传。
> **不承诺**：压缩后 agent 决策完全等价；stdout/stderr 跨通道时序重放。

---

## 1. 目标与非目标

### 1.1 目标（MVP）

| # | 目标 | 验证方式 |
|---|------|----------|
| G1 | RTK 风格 CLI 前缀：`foldback <cmd> [args...]` 执行一次并捕获 stdout/stderr/exit code | 集成测试 t01、t05 |
| G2 | **Raw-first**：任何有损精简前，完整 raw 已写入本地 Stash | 集成测试 t03、t06 |
| G3 | 超阈值输出返回精简视图 + `[foldback ref=…]` marker | 集成测试 t02 |
| G4 | `foldback output get/tail/grep/info/purge` 按需恢复，检索结果**不再精简** | 集成测试 t03–t12 |
| G5 | Stash 写入失败 **fail-open**：仍返回原始输出，不崩溃 | 集成测试 t07 |
| G6 | 短输出、二进制/非法 UTF-8 原样 passthrough 或 byte-exact 恢复 | 集成测试 t01、t06 |
| G7 | 并发 capture 不串 ref、不碰撞 blob | 集成测试 t13 |

### 1.2 非目标（MVP 明确不做）

| 类别 | 说明 |
|------|------|
| **透明 hook** | 不做 Cursor/Claude Code 命令重写；agent 显式使用 `foldback` 前缀 |
| **Windows** | 仅 Unix/macOS；blob 权限、signal exit 按 Unix 语义 |
| **交互式 TTY** | 不分配 PTY；适合有限生命周期、非交互命令 |
| **watch / server** | 不支持长驻进程、流式实时压缩、文件监听 |
| **RTK 级命令过滤器** | MVP 仅通用 head/tail 截断；git/pytest 等专用 reducer 属后续波次 |
| **通用 agent middleware** | 不做 before_model / after_tool_call SDK |
| **跨通道时序重放** | 各通道 byte-exact；不重建 stdout/stderr 交错顺序 |
| **LOSSY 语义摘要模式** | 仅 `REVERSIBLE`；有损过滤必须在 raw 入库之后 |

---

## 2. 核心不变量

实现者**不得违反**以下不变量；测试应直接断言它们。

1. **单次执行** — 底层命令通过 `std::process::Command`（或等价）启动恰好一次；`output get` 等恢复路径**绝不**重跑原命令。
2. **Raw-first** — 精简逻辑读取的是内存中已捕获的 raw；Stash 写入与精简显示顺序为：**先 save raw → 再 condense → 再写终端**。Stash 失败则跳过精简 marker，原样输出 raw（fail-open）。
3. **通道分离** — stdout、stderr 分别存储为独立 blob；metadata 记录各自 size 与 SHA-256。
4. **Exit code 透传** — passthrough 模式 exit code = 子进程 exit code；Unix signal 映射为 `128 + signal`（与常见 shell 约定一致）。
5. **检索 bypass** — `foldback output *` 子命令 stdout 直接写 raw 字节，**不经过 condenser**。
6. **Ref 不可预测** — ref_id 为 128-bit 随机 hex（32 字符）；禁止内容寻址 ref（防跨 session 探测）。
7. **过期即失效** — `expires_at < now` 的 ref 对所有读操作返回 `Expired`（exit 1），不得静默返回部分数据。

---

## 3. CLI 契约

### 3.1 命令面

```
foldback <command> [args...]              # RTK 风格隐式 passthrough（主路径）
foldback run -- <command> [args...]       # 显式逃生：cmd 名为 output/run 时使用
foldback output get    <ref> [flags]      # byte-exact 恢复
foldback output tail   <ref> [flags]      # 末 N 行
foldback output grep   <ref> <pattern>    # 子串匹配行
foldback output info   <ref>              # 元数据
foldback output purge  --expired          # 清理过期 ref
```

**保留命名空间**：首参数为 `output` 或 `run` 时进入 foldback 自身语义；要执行名为 `output`/`run` 的外部命令，必须使用 `foldback run -- …`。

### 3.2 Passthrough 行为

```
foldback git diff
foldback pytest -q tests/
foldback cargo test --lib
```

1. 启动子进程，捕获 stdout/stderr（pipe，非 TTY）。
2. 尝试 Stash save（含 command、args、cwd、exit_code、raw blobs、TTL）。
3. 对 stdout/stderr 分别 condense；写入终端。
4. 以子进程 exit code 退出（0–127 或 128+signal）。

### 3.3 管理命令行为

| 子命令 | 语法 | 说明 |
|--------|------|------|
| `get` | `get <ref> [--channel stdout\|stderr\|both] [--offset N] [--limit N]` | 默认 channel=stdout；both 为 stdout 拼接 stderr |
| `tail` | `tail <ref> [--channel stdout\|stderr] [--lines N]` | 默认 lines=10 |
| `grep` | `grep <ref> <pattern> [--channel stdout\|stderr\|both]` | 逐行子串匹配（非 regex MVP） |
| `info` | `info <ref>` | 人类可读 metadata |
| `purge` | `purge --expired` | 删除过期 metadata + blob；打印删除计数 |

### 3.4 精简 marker 格式

当 condense 生效时，display 输出中插入一行：

```text
[foldback ref=<32-hex> raw=<bytes>b lines=<n> omitted=<m> expires=<ISO8601Z>]
```

- 短输出（未超阈值）或 condense 不省空间：**无 marker**，display = raw。
- marker 是唯一 ref 来源；agent 通过第二次 CLI 调用恢复隐藏内容。

### 3.5 环境变量

| 变量 | 默认 | 用途 |
|------|------|------|
| `FOLDBACK_DATA_DIR` | `$XDG_DATA_HOME/foldback` 或 `~/.local/share/foldback` | Stash 根目录 |
| `XDG_DATA_HOME` | `~/.local/share` | 标准 XDG 路径 |

---

## 4. 数据模型

### 4.1 目录布局

```
$FOLDBACK_DATA_DIR/
├── meta.db          # SQLite WAL
└── blobs/
    ├── <ref_id>.stdout
    └── <ref_id>.stderr
```

### 4.2 SQLite `refs` 表

| 列 | 类型 | 说明 |
|----|------|------|
| `ref_id` | TEXT PK | 32 位 hex |
| `command` | TEXT | argv[0] |
| `args_json` | TEXT | JSON 数组，argv[1..] |
| `cwd` | TEXT | 捕获时工作目录 |
| `created_at` | INTEGER | Unix ms |
| `expires_at` | INTEGER | Unix ms |
| `exit_code` | INTEGER | 子进程 exit code |
| `stdout_size` | INTEGER | 字节数 |
| `stderr_size` | INTEGER | 字节数 |
| `stdout_sha256` | TEXT | hex digest |
| `stderr_sha256` | TEXT | hex digest |

### 4.3 内存结构（实现参考）

```rust
// 概念类型 — 实现可调整命名，语义须一致
CaptureResult { stdout, stderr, exit_code, cwd, command, args, started_at_ms, duration_ms }
EntryMeta     { ref_id, command, args, cwd, created_at_ms, expires_at_ms, exit_code,
                stdout_size, stderr_size, stdout_sha256, stderr_sha256 }
Channel       { Stdout | Stderr | Both }
CondenseResult { display: Vec<u8>, condensed: bool }
```

### 4.4 ref_id 规则

- 生成：`rand` 16 字节 → `hex` 32 字符。
- 校验：长度 32 且全 `[0-9a-f]`（大小写不敏感或统一小写）；否则 `InvalidRef`（exit 2）。

---

## 5. Raw-first 数据流

```
Agent 调用: foldback <cmd> [args]
        │
        ▼
┌───────────────────┐
│  Command::output  │  ← 仅执行一次
└─────────┬─────────┘
          │ CaptureResult (stdout, stderr, exit_code, cwd, …)
          ▼
┌───────────────────┐     失败 ──► fail-open: 原样写 raw 到终端，仍透传 exit code
│  Stash::save      │
│  (blobs + meta)   │
└─────────┬─────────┘
          │ (ref_id, expires_at)
          ▼
┌───────────────────┐
│ condense(stdout)  │──► 终端 stdout
│ condense(stderr)  │──► 终端 stderr
└───────────────────┘

Agent 按需: foldback output get <ref> [--offset N] [--limit N]
        │
        ▼
┌───────────────────┐
│ Stash::read_*     │  ← 不 condense，不写 stash
└─────────┬─────────┘
          ▼
      stdout (raw bytes)
```

**与 RTK 的关键差异**：RTK 默认仅在失败路径 tee，成功过滤通常不可逆；foldback **所有**进入 condense 路径的输出都必须先入库。

**与 Tokenless 的关键差异**：Tokenless 在 agent 框架 `after_tool_call` 钩子里替换 model-visible 副本；foldback 是独立 CLI，agent 通过前缀调用，retrieve 通过 `output get` 而非动态发布的 tool。

---

## 6. 精简规则（MVP 通用 reducer）

### 6.1 触发条件

满足**任一**即尝试 condense：

- `line_count > 100`（`CONDENSE_LINE_THRESHOLD`）
- `byte_len > 10_240`（`CONDENSE_BYTE_THRESHOLD`）

### 6.2 算法

1. 若不超阈值 → 返回原始 bytes，`condensed = false`。
2. 否则取 head 20 行 + tail 20 行（不足则全保留），中间插入 marker 行。
3. 若 `len(condensed) >= len(raw)` → 回退为原始 bytes（condense 无收益）。
4. 二进制/非法 UTF-8：存储与 `get` 保持 byte-exact；display 侧 condense 仅在省空间时替换（marker 为 UTF-8）。

### 6.3 后续专用 reducer（Phase 2+，不在 MVP）

参考 RTK 思路，按命令增加结构化提取（如 `git diff` hunk 摘要、`pytest` 失败段）。**必须**遵守 raw-first：先 stash 完整 raw，再生成专用精简视图。

---

## 7. 错误与 Exit Code 语义

### 7.1 Passthrough 模式（`foldback <cmd>` / `foldback run -- …`）

| 情况 | Exit code |
|------|-----------|
| 子进程正常退出 | 子进程 code（0–255，通常 0–127） |
| 子进程 signal 终止 | `128 + signal`（Unix） |
| 命令不存在 / 无法 exec | 127 |
| Stash 失败 | **仍用子进程 exit code**（fail-open，错误打 stderr） |

### 7.2 管理命令（`foldback output …`）

| Code | 含义 | 典型场景 |
|------|------|----------|
| 0 | 成功 | get/tail/grep/info/purge 正常 |
| 1 | ref 不可用 | `NotFound`、`Expired` |
| 2 | 坏输入 | 缺参数、非法 flag、ref 格式非法 |
| 3 | 内部错误 | SQLite/IO/存储故障 |

**约束**：管理命令 exit code 与子进程 code **命名空间分离**（管理用 0–3；passthrough 用 0–127+）。

### 7.3 错误类型（实现枚举）

```
NotFound { ref_id }
Expired  { ref_id }
InvalidRef { input }
BadInput(String)
Storage(String)
Io(Error)
```

---

## 8. 存储、权限与 TTL

| 项 | MVP 值 | 说明 |
|----|--------|------|
| 默认 TTL | 7 天 | `DEFAULT_TTL_SECS = 7 * 24 * 3600` |
| Blob 权限 | `0600` | owner read/write only（`OpenOptionsExt::mode`） |
| DB | SQLite WAL, synchronous=NORMAL | 单进程 CLI 足够；并发 capture 依赖 OS + SQLite 锁 |
| 清理 | `purge --expired` 手动；无后台 daemon | 删除过期行 + 对应 blob 文件 |
| 隔离 | `FOLDBACK_DATA_DIR` per project/test | 集成测试必须使用临时目录 |
| 容量上限 | MVP 无硬上限 | **风险**：超大输出占满磁盘；文档声明适用 bounded 命令 |

---

## 9. TDD 实现波次

严格 **RED → GREEN → REFACTOR**。每波次先写失败测试，再最小实现。

### Wave 0 — 脚手架 ✅

- [x] `cargo init`、依赖（以 `Cargo.toml` 为准）：`rusqlite`（bundled）、`sha2`、`hex`、`chrono`、`serde`、`serde_json`、`rand`、`thiserror`
- [x] dev-deps：`tempfile`、`assert_cmd`、`predicates`
- [x] 模块骨架：`runner`、`stash`、`condenser`、`error`、`commands/*`
- **验证**：`cargo test` 编译通过 ✅
- **实现注记**：CLI 解析为手写 `env::args`（未引入 `clap`/`anyhow`；见 `docs/impl-notes/`）

### Wave 1 — 捕获与 exit code（RED: t01, t05）✅

- [x] `runner::capture` — pipe stdout/stderr，记录 cwd
- [x] `main` 隐式 passthrough，无 stash 时原样输出
- **验证**：`foldback echo hello`、`foldback true/false/exit 42` ✅

### Wave 2 — Stash 核心（RED: stash 单元测试）✅

- [x] `Stash::open/save/meta/read_channel`
- [x] ref 生成、SHA-256、blob 0600、TTL 字段
- [x] `validate_ref_id`、`Expired`/`NotFound`/`InvalidRef`
- **验证**：单元测试 roundtrip、offset/limit、并发 save 无碰撞 ✅

### Wave 3 — Condenser（RED: condenser 单元测试 + t02）✅

- [x] 阈值、head/tail、marker 格式、无收益回退
- [x] `handle_run` 集成 save + condense
- **验证**：`seq 1 200` 产生 marker；短输出无 marker ✅

### Wave 4 — 恢复命令（RED: t03, t04, t08–t10, t12）✅

- [x] `output get/tail/grep/info/purge`
- [x] CLI 解析与 exit code 映射
- **验证**：byte-exact 恢复、channel 分离、offset/limit、tail/grep ✅

### Wave 5 — 韧性（RED: t06, t07, t11, t13–t16）✅

- [x] 二进制 stdout、stash fail-open、过期 purge
- [x] 并发 ref 隔离、`foldback run --` 逃生
- [x] invalid/not-found ref exit code
- **验证**：完整 `cargo test`；`cargo clippy -D warnings` ✅

### Wave 6 — 文档与发布准备 ✅

- [x] README、`docs/design.md`、`docs/impl-notes/`、`LICENSE`、`.gitignore`
- [x] release build smoke demo（见 README「Quick start (smoke test)」）
- **未实现**：CI 流水线（仓库无 `.github/workflows/`；本地质量门已绿，见 §10）

---

## 10. 验收映射

| 测试 ID | 断言 | 计划条目 |
|---------|------|----------|
| t01 | 短 stdout passthrough | §6.1 未超阈值 |
| t01b | 短 stderr passthrough | §3.2 通道分离 |
| t02 | 长输出含 `[foldback ref=` | §3.4 marker |
| t03 | get 恢复 byte-exact | §2 raw-first + §3.3 get |
| t04 | stdout/stderr channel 隔离 | §2 #3 |
| t05 | exit code 0/42/1 透传 | §7.1 |
| t06 | 非法 UTF-8 恢复 | §2 byte-exact |
| t07 | 只读 data dir fail-open | §2 #2 fail-open |
| t08 | offset/limit | §3.3 get |
| t09 | tail | §3.3 tail |
| t10 | grep | §3.3 grep |
| t11 | 过期 → exit 1；purge | §7.2 + §8 |
| t12 | info 字段完整 | §4.2 |
| t13 | 并发 6 ref 不串 | §8 隔离 |
| t14 | 非法 ref → exit 2 | §7.2 |
| t15 | 不存在 ref → exit 1 | §7.2 |
| t16 | `run --` 逃生 | §3.1 |

**完成标准**（Phase 1 MVP，2026-08-31 验收）：

| 质量门 | 状态 |
|--------|------|
| `cargo fmt --check` | ✅ |
| `cargo clippy --all-targets -- -D warnings` | ✅ |
| `cargo test` | ✅ **92/92**（lib 单元 52 + bin 单元 4 + `tests/cli_errors.rs` 17 + 集成 t01–t16 共 19） |
| `cargo build --release` | ✅ |
| Smoke（临时 `FOLDBACK_DATA_DIR`）：`seq 1 200` → 提取 ref → `output get` 字节与 `output info` SHA-256 一致 | ✅（README「Quick start (smoke test)」；t03 断言 byte-exact 内容） |

**已知缺口（不阻塞 MVP 交付）**：signal exit `128+sig` 无端到端集成测（`runner` 单元已覆盖）；并发 SQLite 锁冲突 → CLI exit 3 无集成测；**无 CI 配置**（须本地或后续补 workflow）。

---

## 11. 风险与缓解

| 风险 | 影响 | MVP 缓解 |
|------|------|----------|
| 大输出内存占用 | capture 全量读入 RAM | 文档声明 bounded 命令；后续 streaming spill-to-disk |
| 磁盘耗尽 | save 失败 | fail-open；stderr 告警 |
| 精简遗漏关键证据 | agent 误判 | marker 提示 `output tail/grep`；后续专用 reducer |
| retrieve 再次灌爆 context | 二次 token 爆炸 | 推广 `--offset/--limit`、tail、grep；文档示例 |
| 无 hook  adoption 摩擦 | agent 需改调用习惯 | README 示例；后续可选 wrapper 脚本 |
| grep 子串语义 | 误匹配 | 文档标明非 regex；Phase 2 可选 `-E` |
| 并发 SQLite |  rare 锁冲突 | WAL + 短事务；冲突返回 exit 3 |
| 敏感数据落盘 | 本地泄露 | 0600 blob、TTL、purge；文档威胁模型 |

---

## 12. 后续阶段（Phase 2+）

1. **命令专用 reducer** — git diff/status、pytest、cargo test、npm/tsc/eslint（参考 RTK 语义，非复制代码）。
2. **Streaming spill** — 超大输出写临时文件，capture 不全量驻内存。
3. **Regex grep / JSON path retrieve** — 降低恢复 payload。
4. **Shell 补全与安装脚本** — `cargo install`、Homebrew formula。
5. **可选轻量 hook** — 仅文档化 opt-in 别名，非 MVP。
6. **Windows 移植** — 权限模型、进程 API、exit code 语义。
7. **Framework adapter** — MCP tool / Python SDK 包装 `foldback output get`。
8. **Metrics** — token 节省估算、retrieve 命中率、悬空 ref 率。

---

## 附录 A：与 Phase 0 原 plan 的差异说明

对话中 Phase 1 曾使用 `rev retrieve` 命名；**现以 `foldback output get` 为准**（避免与 `run` 子命令冲突、对齐 `output` 管理命名空间）。语义等价：retrieve = get，inspect 能力拆为 tail/grep/info。

Fork RTK 方案已明确**放弃**；改为独立实现 + 参考设计。工作量预估（1 人）：核心可逆 CLI 2–3 周；含 6–10 类专用 reducer 的可用 MVP 4–6 周。

---

## 附录 B：实现状态快照（2026-08-31 验收）

> 供新维护者区分 **Phase 1 MVP 已完成** 与 **后续工作**；**以代码与测试为准**。若本 plan 与代码不一致，**以本 plan 为规范**，在 `docs/impl-notes/` 记录偏离。

### Phase 1 MVP — 已完成 ✅

| 范围 | 状态 | 证据 |
|------|------|------|
| Wave 0–6 实现波次 | ✅ | §9 全部勾选；92 项测试全绿 |
| 核心模块 | ✅ | `src/main.rs`、`runner.rs`、`stash.rs`、`condenser.rs`、`error.rs`、`commands/*` |
| 集成测试 t01–t16 | ✅ | `tests/integration_tests.rs` |
| CLI 错误路径 | ✅ | `tests/cli_errors.rs`（17 项） |
| 文档 | ✅ | `README.md`、`docs/design.md`、`docs/impl-notes/` |
| 发布产物 | ✅ | `cargo build --release`；`LICENSE`、`.gitignore` |
| 本地质量门 | ✅ | fmt / clippy / test / release（§10） |
| Smoke 演示 | ✅ | README「Quick start (smoke test)」 |

| 模块 | 文件 |
|------|------|
| CLI 入口 | `src/main.rs` |
| 捕获 | `src/runner.rs` |
| 存储 | `src/stash.rs` |
| 精简 | `src/condenser.rs` |
| 错误 | `src/error.rs` |
| 管理命令 | `src/commands/{get,tail,grep,info,purge}.rs` |
| 集成测试 | `tests/integration_tests.rs` |

### 后续工作 — 未实现（Phase 2+，见 §12）

| 项 | 说明 |
|----|------|
| **CI 流水线** | 无 `.github/workflows/`；质量门仅本地验证 |
| **命令专用 reducer** | git diff、pytest、cargo test 等结构化精简（§6.3、§12 #1） |
| **Streaming spill** | 超大输出 spill-to-disk，不全量驻内存（§12 #2） |
| **Regex grep / JSON path** | §12 #3 |
| **Shell 补全 / 安装脚本** | §12 #4 |
| **透明 hook / Windows / Framework adapter** | §1.2 非目标、§12 #5–7 |
| **Metrics** | §12 #8 |

### 依赖快照（`Cargo.toml`，Wave 0 对齐）

**dependencies**：`rusqlite`、`sha2`、`hex`、`chrono`、`serde`、`serde_json`、`rand`、`thiserror`

**dev-dependencies**：`tempfile`、`assert_cmd`、`predicates`

> 未使用：`clap`、`anyhow`（早期 scaffold 曾列，已移除；CLI 手写解析，见 impl-notes）。
