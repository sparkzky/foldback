# rawref Phase 2 实现计划 — 专用 Reducer 框架

> **定位**：在 Phase 1（commit `b1b72dc`，92/92 测试）之上，交付**可测试的 display/reducer 框架**、**argv 规范化**、**pytest reducer**、**cargo test reducer**、**fixture/E2E** 与 **`RAWREF_REDUCERS=0` opt-out**。
> **研究依据**：RTK 的 registry / parse fallback / never-worse / lossiness / exit 保护；ANOLISA Tokenless 的 arbitration / recoverability 思想，**不**照搬 StashLedger 与 middleware。
> **权威基线**：`docs/plan.md`（Phase 1 规格）、`docs/design.md`（架构）、`src/` + `tests/`（实现与验收）。
> **实现注记路径**：`docs/impl-notes/2026-08-31-specialized-reducers.md`（Phase 2 偏离与决策记录）。

---

## 1. 目标与非目标

### 1.1 目标（Phase 2）

| # | 目标 | 验证方式 |
|---|------|----------|
| G1 | **Display pipeline 框架**：candidate → generic → raw 三级回退；纯函数 reducer；registry 按命令 basename 匹配 | 单元测试 `display/*`；Wave 1 行为等价 |
| G2 | **argv 规范化**：`python -m pytest`、`cargo test` 等可识别 basename；未匹配命令走 Phase 1 generic | 单元测试 `argv.rs`；集成 t02 等价 |
| G3 | **pytest reducer**：保留失败块、ERROR、short summary、warnings 摘要/最终计数；成功路径压成 summary 但不伪造 | fixture 单元 + E2E `tests/fixtures/pytest/*` |
| G4 | **cargo test reducer**：stdout 保留测试结果/failed names/failure blocks/final summary；stderr 编译错误保守 generic/raw | fixture 单元 + E2E `tests/fixtures/cargo-test/*` |
| G5 | **Never-worse（按字节）**：含 marker 字节计入；专用视图 ≥ raw 则回退 generic；generic ≥ raw 则 passthrough | 单元 + 集成 never-worse 矩阵 |
| G6 | **Raw-first 不变**：Stash save 完成后才调用 reducer；fail-open 跳过 reducer | 现有 t07 + 新增 stash-fail-open-reducer 测 |
| G7 | **Marker 契约**：generic 保持 Phase 1 格式（含精确 `omitted=`）；专用 marker 含 `view=`/`mode=`/`recoverability=retrievable`、**不含** `omitted=`；`output get` byte-exact 不变；**inline semantic lossy, end-to-end retrievable** | t03 + marker 解析单测 |
| G8 | **Opt-out**：`RAWREF_REDUCERS=0` 回退 Phase 1 generic condenser 行为 | 集成 opt-out 测 |
| G9 | **Phase 1 全量回归**：现有 92 项测试全部保留且通过 | `cargo test` 基线 |

### 1.2 非目标（Phase 2 明确不做）

| 类别 | 说明 | 归属 |
|------|------|------|
| **git diff / git status reducer** | 不在本阶段范围 | Phase 3+ |
| **Streaming spill-to-disk** | capture 仍全量 `Vec<u8>` | Phase 3 |
| **SQLite schema migration** | 不在 DB 存 reduction/recoverability；仅 marker 与 impl-notes | Phase 3+ |
| **Stats / quota / metrics** | 无 token 估算、无 retrieve 命中率 | Phase 3 |
| **透明 hook / middleware** | agent 仍显式 `rawref` 前缀 | 长期非目标 |
| **`--message-format=json` / nextest / 自定义 cargo 格式** | 检测后 passthrough generic | Phase 3+ |
| **真实临时 git 项目 E2E** | 本阶段用 pytest/cargo fixture 小 crate | Phase 3+ |
| **Regex grep / JSON path retrieve** | plan §12 #3 | Phase 3+ |
| **Windows / CI workflow** | 仍仅 Unix/macOS 本地验证 | 后续 |

### 1.3 Phase 1 不可破坏不变量

实现者**不得违反**；Phase 2 测试须直接断言它们（与 `docs/plan.md` §2 一致）：

| # | 不变量 | Phase 2 约束 |
|---|--------|--------------|
| I1 | **单次执行** — 底层命令恰好启动一次；`output get` 不重跑 | reducer 不得 spawn 子进程 |
| I2 | **Raw-first** — 先 save raw → 再 display；stash 失败 fail-open 原样输出 | reducer 仅在 stash 成功后调用 |
| I3 | **通道分离** — stdout/stderr 独立 blob 与 condense | reducer 按通道独立调用 |
| I4 | **Exit code 透传** — passthrough exit = 子进程 exit；reducer 不得修改 | `ReduceOutcome` 无 exit 字段 |
| I5 | **检索 bypass** — `rawref output *` 不经过 reducer/condenser | commands/* 不 import display |
| I6 | **Ref 不可预测** — 128-bit 随机 hex，非内容寻址 | 不变 |
| I7 | **过期即失效** — expired ref → exit 1 | 不变 |
| I8 | **Never-worse** — display 字节数（含 marker）严格小于 raw 才替换 | Phase 2 扩展至专用+generic 两级 |
| I9 | **Marker 可提取** — 现有 `extract_ref_id()` 对 Phase 1 与 Phase 2 marker 均有效 | 前缀 `[rawref ref=<32hex>` 不变 |

---

## 2. 已批准的高风险决策与备选

以下决策**已由用户批准**；实现须遵循，偏离须在 impl-notes 记录并获 re-review。

### 2.1 只抽纯 display pipeline，不提前抽 ByteView / streaming

| 决策 | 只新增 `display/` 模块族（pipeline、registry、marker、reducers）；`runner`/`stash` 仍用 `Vec<u8>` |
|------|--------------------------------------------------------------------------------------------------|
| **Why** | Phase 3 spill 需不同抽象；过早 ByteView 会绑定错误边界 |
| **备选 A（拒绝）** | 现在引入 `ByteView` trait + mmap — 与 spill 设计耦合，YAGNI |
| **备选 B（拒绝）** | 在 `condenser.rs` 内硬编码 pytest — 不可测试、不可扩展 |
| **验收** | `display` 模块零 IO；`runner`/`stash` diff 仅限 `handle_run` 调用点 |

### 2.2 三级 pipeline：专用 candidate → generic → raw

```
display(raw_bytes, ctx) ──►
  1. registry.match(ctx) → specialized.reduce() → candidate
  2. if candidate.skip or !beneficial(candidate, raw) → generic.condense()
  3. if !beneficial(generic, raw) → raw passthrough (condensed=false)
```

| 决策 | 专用 reducer 输出为 **candidate**；generic 为 Phase 1 head/tail；raw 为原样 |
|------|-------------------------------------------------------------------------------|
| **Why** | RTK 风格 parse fallback；专用失败不丢信息 |
| **备选 A（拒绝）** | 专用失败直接 raw — 丢失 generic 对「长但不可 parse」输出的压缩 |
| **备选 B（拒绝）** | 专用+generic 取较短者不做级联 — 可能选到语义更差但更短的视图 |
| **验收** | 每级单测 + 集成「parse 失败 → generic marker」 |

### 2.3 Never-worse 按字节严格，且 marker 字节计入

| 决策 | `beneficial(display, raw) := display.len() < raw.len()`（**严格小于**） |
|------|------------------------------------------------------------------------|
| **Why** | Phase 1 `condenser.rs` 已用严格小于；marker 是 display 的一部分 |
| **备选 A（拒绝）** | 不计 marker — 可能 `len(display) >= len(raw)` 仍替换，违反 I8 |
| **备选 B（拒绝）** | 按「行数/token 估算」— 不可测试、主观 |
| **验收** | 宽行 fixture（Phase 1 `test_byte_threshold_with_few_lines_*`）仍 passthrough |

### 2.4 Raw-first 后才调用 reducer

| 决策 | `handle_run` 顺序不变：`capture → stash.save → display::render_channel ×2` |
|------|-------------------------------------------------------------------------------|
| **Why** | plan §2 #2；Tokenless recoverability 前提 |
| **备选 A（拒绝）** | reducer 边 capture 边精简 — 违反 raw-first |
| **验收** | t03 byte-exact；stash fail-open 无 marker（t07） |

### 2.5 压缩性质与恢复能力分离；marker 契约，但不迁移 DB

| 决策 | **两轴建模**：`ReductionKind`（inline 如何压缩）与 `Recoverability`（端到端能否恢复 raw）分离；Phase 2 所有 applied view 均为 `Recoverability::Retrievable`（raw 已 stash），**不**意味着 inline view 无损 |
|------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **Why** | 语义摘要（pytest/cargo summary）inline **有损**；recoverability 来自 raw-first + `output get`，非 marker 声称可逆 |
| **Generic marker** | Phase 1 原格式不变，含可精确计算的 `omitted=` |
| **Specialized marker** | `[rawref ref=<32hex> raw=<bytes>b lines=<n> view=pytest|cargo-test mode=summary recoverability=retrievable expires=...]`；**不含** `omitted=`（重组 failure/warning 段时无法精确证明行数，避免虚假元数据） |
| **备选 A（拒绝）** | 专用 marker 保留 `omitted=` — 重组视图时元数据不可证 |
| **备选 B（Phase 3）** | `refs` 表持久化 `reduction_kind` / `recoverability` — 本阶段不做 |
| **验收** | marker 单测；info 仍只含 Phase 1 字段；专用 marker 断言无 `omitted=` |

### 2.6 `RAWREF_REDUCERS=0` 回退 Phase 1 generic

| 决策 | env 非 `"0"` 时启用 registry；`=0` 时 **`display` 仅调用 legacy generic**（等价现 `condenser::condense`） |
|------|----------------------------------------------------------------------------------------------------------|
| **Why** | 生产回滚开关；A/B 与 debug |
| **备选 A（拒绝）** | 完全 passthrough 不 condense — 与 Phase 1 行为不一致 |
| **验收** | opt-out 集成测：长输出仍 generic marker，无 `view=` |

### 2.7 Reducer 纯函数：无 IO / 无 spawn / 不改 exit

| 决策 | `fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome` |
|------|-------------------------------------------------------------------------|
| **Why** | 可单元测试；I1/I4 |
| **验收** | reducer 模块无 `std::fs`/`Command`/网络 import |

---

## 3. 仍需人判断的问题（已尽量收敛）

| # | 问题 | 建议默认 | 若偏离则记录 |
|---|------|----------|--------------|
| Q1 | pytest warnings 摘要最大行数 | **5 行** + 末行 `… N warnings` 计数 | impl-notes |
| Q2 | cargo test failure block 上下文行数（`---- foo stdout ----` 前） | **0 行**（仅块内） | impl-notes |
| Q3 | `python3` vs `python` basename | 仅匹配 argv0 **`python*`** 且 args 含 `-m pytest` | impl-notes |
| Q4 | `cargo test` 子命令形态 | 匹配 basename `cargo` + 首子参数 `test`（`cargo test`、`cargo test --lib`） | impl-notes |
| Q5 | generic vs specialized marker 字段 | **已关闭**：generic 保留 Phase 1 全字段（含精确 `omitted=`）；specialized 含 `raw=`/`lines=`/`view=`/`mode=`/`recoverability=retrievable`/`expires=`，**不含** `omitted=` | 非开放 |

> **说明**：Q1–Q4 有合理默认，实现可按默认推进；Q5 已按 marker 契约关闭。仅在实测 fixture 证明不足时调整 Q1–Q4 并写入 impl-notes。
>
> **核心语义**：**inline semantic lossy, end-to-end retrievable** — 专用 summary 在终端可见层丢弃进度行等；完整 raw 通过同一 ref 的 `output get` 恢复。

---

## 4. Rust API / 类型草案

### 4.1 模块布局

```
src/
├── lib.rs                    # + pub mod display; pub mod argv;
├── main.rs                   # handle_run → display::render_passthrough
├── condenser.rs              # 保留；generic 实现迁入 display/generic.rs 后 re-export
└── display/
    ├── mod.rs                # render_channel, render_passthrough, beneficial
    ├── context.rs            # CommandContext, ChannelContext
    ├── outcome.rs            # ReduceOutcome, SkipReason, ViewKind, ReductionKind, Recoverability
    ├── marker.rs             # build_marker, parse_marker_prefix
    ├── registry.rs           # Registry, Reducer trait, BenefitDecision
    ├── generic.rs            # Phase 1 head/tail（自 condenser 迁入）
    └── reducers/
        ├── mod.rs
        ├── pytest.rs
        └── cargo_test.rs
```

### 4.2 核心类型（草案）

```rust
/// Passthrough 命令上下文（stash save 后、display 前构造）
pub struct CommandContext {
    pub command: String,           // argv[0] 原样
    pub args: Vec<String>,
    pub normalized: NormalizedCommand, // argv 规范化结果
    pub exit_code: i32,
    pub cwd: String,
}

/// 单通道 display 上下文
pub struct ChannelContext<'a> {
    pub command: &'a CommandContext,
    pub channel: Channel,          // 复用 stash::Channel 或 display 本地 enum
    pub ref_id: &'a str,
    pub expires_at: &'a DateTime<Utc>,
}

/// 规范化命令 — registry 匹配唯一依据
pub enum NormalizedCommand {
    Pytest { module_invocation: bool },  // python -m pytest / pytest
    CargoTest,
    Generic,
}

pub enum ViewKind {
    Generic,
    PytestSummary,
    CargoTestSummary,
    Raw,
}

/// inline 压缩/摘要性质（终端可见层）
pub enum ReductionKind {
    GenericTruncation,  // head/tail 截断；omitted= 可精确计算
    SemanticSummary,    // pytest/cargo 语义重组；inline 有损
}

/// 端到端恢复能力（与 ReductionKind 正交）
pub enum Recoverability {
    Retrievable,  // raw 已 stash；output get 可 byte-exact 恢复
}

pub enum SkipReason {
    Disabled,           // RAWREF_REDUCERS=0
    NoMatch,            // Generic command
    ParseFailed,
    MachineReadable,    // --collect-only, json format, etc.
    NoBenefit,
    NonUtf8,
    Empty,
}

pub struct ReduceOutcome {
    pub display: Vec<u8>,
    pub applied: bool,              // true = 替换了 raw（含 generic）
    pub view: ViewKind,
    pub reduction: ReductionKind,   // GenericTruncation | SemanticSummary | （Raw 时 N/A）
    pub recoverability: Recoverability, // Phase 2 applied view 均为 Retrievable
    pub skip_reason: Option<SkipReason>,
}

pub enum BenefitDecision {
    Accept,
    Reject { reason: SkipReason },
}

pub trait Reducer: Send + Sync {
    fn name(&self) -> &'static str;
    fn matches(&self, norm: &NormalizedCommand) -> bool;
    fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome;
}

pub struct Registry {
    reducers: Vec<Box<dyn Reducer>>,
}

impl Registry {
    pub fn default_registry() -> Self { /* pytest, cargo_test */ }
    pub fn reduce_or_skip(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome;
}
```

### 4.3 入口 API

```rust
// display/mod.rs
pub fn render_channel(
    raw: &[u8],
    ctx: &ChannelContext,
    registry: &Registry,
    reducers_enabled: bool,
) -> ReduceOutcome;

pub fn render_passthrough(
    stdout: &[u8],
    stderr: &[u8],
    cmd_ctx: &CommandContext,
    ref_id: &str,
    expires_at: &DateTime<Utc>,
    registry: &Registry,
    reducers_enabled: bool,
) -> (ReduceOutcome, ReduceOutcome);
```

### 4.4 argv 规范化（`src/argv.rs`）

```rust
pub fn normalize(command: &str, args: &[String]) -> NormalizedCommand {
    let basename = command_basename(command); // 末段路径，不含 .exe
    // cargo test
    if basename == "cargo" && args.first().map(|s| s.as_str()) == Some("test") {
        return NormalizedCommand::CargoTest;
    }
    // pytest / python -m pytest
    if basename == "pytest" {
        return NormalizedCommand::Pytest { module_invocation: false };
    }
    if basename.starts_with("python") {
        if let Some(idx) = args.iter().position(|a| a == "-m") {
            if args.get(idx + 1).map(|s| s.as_str()) == Some("pytest") {
                return NormalizedCommand::Pytest { module_invocation: true };
            }
        }
    }
    NormalizedCommand::Generic
}

fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}
```

**不匹配** → `NormalizedCommand::Generic` → 仅 generic head/tail（与 Phase 1 字节等价）。

### 4.5 环境变量

| 变量 | 默认 | 用途 |
|------|------|------|
| `RAWREF_DATA_DIR` | （Phase 1 不变） | Stash 根 |
| `RAWREF_REDUCERS` | 启用（未设或非 `0`） | `0` = 仅 generic，不调用专用 reducer |

---

## 5. Display pipeline 详细语义

### 5.1 总流程

```
handle_run (stash Ok)
│
├─ cmd_ctx = CommandContext { normalized: argv::normalize(...) }
├─ reducers_enabled = env("RAWREF_REDUCERS") != "0"
│
├─ stdout_out = render_channel(stdout_raw, ctx_stdout, registry, reducers_enabled)
└─ stderr_out = render_channel(stderr_raw, ctx_stderr, registry, reducers_enabled)
```

### 5.2 `render_channel` 内部

```
if !exceeds_threshold(raw) → return raw passthrough (applied=false, view=Raw)

if reducers_enabled:
    for r in registry.match(normalized):
        if r.passthrough_gate(cmd_ctx, args) → SkipReason::MachineReadable
        candidate = r.reduce(raw, ctx)
        if candidate.skip_reason == ParseFailed → continue 下一 reducer 或 fall through
        if beneficial(candidate.display, raw) → return candidate with specialized marker
        else → fall through

generic = generic::condense(raw, ctx)   // Phase 1 算法
if beneficial(generic.display, raw) → return generic (view=Generic)
else → return raw passthrough
```

### 5.3 Marker 格式

**Generic（Phase 1 不变，`ReductionKind::GenericTruncation`）**：

```text
[rawref ref=<32-hex> raw=<bytes>b lines=<n> omitted=<m> expires=<ISO8601Z>]
```

- `raw=` / `lines=` / `omitted=` 相对 **原始 raw** 精确计算（head/tail 算法可证）。
- `omitted=` 表示 head+tail 之间省略的**原始行数**。

**专用（`ReductionKind::SemanticSummary`，向后兼容前缀）**：

```text
[rawref ref=<32-hex> raw=<bytes>b lines=<n> view=pytest mode=summary recoverability=retrievable expires=<ISO8601Z>]
[rawref ref=<32-hex> raw=<bytes>b lines=<n> view=cargo-test mode=summary recoverability=retrievable expires=<ISO8601Z>]
```

- **含**：`raw=`（原始字节数）、`lines=`（原始行数）、`view=`、`mode=summary`、`recoverability=retrievable`、`expires=`。
- **不含**：`omitted=` — 语义重组后无法精确映射「省略了多少原始行」，输出该字段会构成虚假元数据。
- **`recoverability=retrievable`**：声明端到端可通过 `output get` 恢复 full raw；**不**声称 inline display 无损。

**共通规则**：

- **提取规则**：自 `[rawref ref=` 起 32 位 hex；后续空格分隔键值对为可选。
- **`output get`**：仍返回 stash blob 原始字节，**不含** marker。
- **语义总结**：**inline semantic lossy, end-to-end retrievable**。

### 5.4 `extract_ref_id` 兼容

现有测试 helper（`tests/integration_tests.rs`）：

```rust
// 对 Phase 1 与 Phase 2 marker 均有效 — 前缀 ref= 后 32 hex
for chunk in s.split("ref=") { ... }
```

Phase 2 新增 marker 单测：`view=pytest` 不影响 ref 提取。

---

## 6. pytest reducer 精确保留策略

### 6.1 适用范围

- **匹配**：`NormalizedCommand::Pytest`
- **通道**：仅 **stdout** 尝试专用 reducer；stderr 走 generic/raw（pytest 失败详情通常在 stdout）
- **阈值**：仍须先过 `exceeds_threshold`（>100 行或 >10KB）

### 6.2 Passthrough gate（专用 reducer 不运行，直接 generic/raw）

| 条件 | SkipReason |
|------|------------|
| `--collect-only` / `--co` | MachineReadable |
| `-q` / `--quiet` 且同时有 `--tb=no` 等已极简 | MachineReadable（可选，默认 **不** skip `-q`） |
| `-v` / `--verbose` 计数 > 0 | MachineReadable |
| `--json-report` / `--junitxml` 等机器可读输出 flag | MachineReadable |
| 输出非 UTF-8 | NonUtf8 → generic/raw |

> **默认**：仅 **明确机器可读** flag 走 gate；普通 `-q` 仍允许 summary reducer。

### 6.3 解析模型

- 按行解析 UTF-8；失败 → `SkipReason::ParseFailed` → generic
- 识别段落：
  - **失败块**：`FAILED ...` / `FAILURES` / `= FAILURES =` 后 traceback 至下一段落边界
  - **ERROR 块**：`ERROR ...` / `ERRORS` 段
  - **Short test summary**：`= short test summary info =` 至下一 `=` 行或 EOF
  - **Warnings**： `warnings summary` / `PytestWarning` 段；保留至多 **5 行** + 一行 `… (<total> warnings omitted)` 若可计数
  - **最终 summary 行**：末行 `=N passed/ failed/ error/ skipped/ warnings in` 模式

### 6.4 输出结构（成功路径 — 压成 summary 但不伪造）

```
<保留的所有失败/ERROR 块 — 原文逐字节>
<short test summary info 段 — 原文>
<warnings 摘要 — 仅缩略多余行，不捏造 warning 文本>
<最终 summary 行 — 必须来自 raw，禁止合成 passed 数>
[rawref ref=... view=pytest mode=summary recoverability=retrievable ...]
```

- **成功无失败**：保留最终 summary 行 + warnings 摘要（若有）+ marker；**不得**注入虚假 `FAILED`
- **无最终 summary 且 parse 不完整** → ParseFailed → generic

### 6.5 异常 exit / 边界

| 场景 | 行为 |
|------|------|
| exit code 5（no tests collected） | 保留 pytest 原始关键行（如 `no tests ran`）+ marker 若 beneficial；parse 失败 → generic |
| 空 stdout | Empty → generic/raw |
| 混合 BOM / 非 UTF-8 | NonUtf8 → generic/raw |
| 极短输出未超阈值 | 不 condense（Phase 1 规则） |

### 6.6 Fixture 类型

| Fixture 文件 | 用途 |
|--------------|------|
| `tests/fixtures/pytest/passing_many.txt` | 大量 `.` 进度行 + 最终 passed summary |
| `tests/fixtures/pytest/failing_one.txt` | 单测失败 + traceback + short summary |
| `tests/fixtures/pytest/error_setup.txt` | ERROR 非 FAILURE |
| `tests/fixtures/pytest/warnings_heavy.txt` | 多 warnings 缩略 |
| `tests/fixtures/pytest/no_tests_exit5.txt` | exit 5 语义 |
| `tests/fixtures/pytest/collect_only.txt` | gate → generic |
| `tests/fixtures/pytest/non_utf8.bin` | NonUtf8 路径 |

E2E：临时 venv + 最小 `test_*.py` 或通过 `python3 -m pytest` 对 fixture 目录（若环境无 pytest，用 fixture 单测为主；E2E 在 pytest 可用环境运行）。

---

## 7. cargo test reducer 精确保留策略

### 7.1 适用范围

- **匹配**：`NormalizedCommand::CargoTest`
- **通道**：**stdout** 专用；**stderr** 编译错误保守 **generic/raw**（不尝试结构化 compile diagnostic）

### 7.2 Passthrough gate

| 条件 | SkipReason |
|------|------------|
| `--message-format=json` 或 `json-diagnostic` | MachineReadable |
| 检测到 nextest 输出特征（如 `NEXTEST_*` / `nextest` banner） | MachineReadable |
| 非 UTF-8 stdout | NonUtf8 |
| 自定义 harness / 非 libtest 格式 | ParseFailed |

### 7.3 stdout 解析保留

| 段落 | 策略 |
|------|------|
| 测试结果行 | `test <name> ... ok` / `... FAILED` 行 — 失败名 **全保留** |
| Failed tests 列表 | `failures:` 段内测试名全保留 |
| Failure blocks | `---- <name> stdout ----` / `stderr` 块 **原文保留** |
| 最终 summary | `test result: FAILED/passed` 行 **必须保留** |
| 成功路径 | 仅保留 summary 行 + 失败相关（若无失败则 summary + marker） |

### 7.4 stderr 策略（保守）

- **编译错误**（`error[E` / `error:` / `--> file.rs:line`）：**不** parse 为专用视图 → **generic head/tail 或 raw**
- **理由**：rustc diagnostic 结构复杂；误判风险高
- **never 把 failure 写成 success**：若专用 reducer 无法确认失败语义 → ParseFailed → generic，**禁止**仅输出 `test result: ok`

### 7.5 Fixture 类型

| Fixture 文件 | 用途 |
|--------------|------|
| `tests/fixtures/cargo-test/passing_many.txt` | 大量 ok + 最终 summary |
| `tests/fixtures/cargo-test/failing_one.txt` | 单测 FAILED + block |
| `tests/fixtures/cargo-test/compile_error.stderr` | stderr 走 generic |
| `tests/fixtures/cargo-test/json_format.txt` | gate |
| `tests/fixtures/cargo-test/non_utf8.bin` | NonUtf8 |

E2E：`tests/fixtures/cargo-test/minimal-crate/` — 最小 Rust lib + 若干单元测试；`rawref cargo test` 真实执行。

---

## 8. 文件改动地图

| 文件 | 改动类型 | 说明 |
|------|----------|------|
| `src/lib.rs` | 修改 | `pub mod display; pub mod argv;` |
| `src/main.rs` | 修改 | `handle_run` 调用 `display::render_passthrough`；读 `RAWREF_REDUCERS` |
| `src/condenser.rs` | 修改 | 算法迁至 `display/generic.rs`；保留 re-export 与 Phase 1 单测路径 |
| `src/display/mod.rs` | **新增** | pipeline 入口 |
| `src/display/context.rs` | **新增** | CommandContext / ChannelContext |
| `src/display/outcome.rs` | **新增** | ReduceOutcome / enums |
| `src/display/marker.rs` | **新增** | marker 构建与解析 |
| `src/display/registry.rs` | **新增** | Registry trait 对象 |
| `src/display/generic.rs` | **新增** | 自 condenser 迁入 |
| `src/display/reducers/mod.rs` | **新增** | |
| `src/display/reducers/pytest.rs` | **新增** | |
| `src/display/reducers/cargo_test.rs` | **新增** | |
| `src/argv.rs` | **新增** | normalize + 单测 |
| `tests/display_generic.rs` 或 lib 单测 | **新增** | display 单元测试 |
| `tests/reducers_pytest.rs` | **新增** | pytest reducer 单测（读 fixture） |
| `tests/reducers_cargo_test.rs` | **新增** | cargo reducer 单测 |
| `tests/integration_phase2.rs` | **新增** | E2E + opt-out + never-worse |
| `tests/fixtures/pytest/*` | **新增** | 文本 fixture |
| `tests/fixtures/cargo-test/*` | **新增** | 文本 fixture + minimal crate |
| `docs/impl-notes/2026-08-31-specialized-reducers.md` | **新增** | Phase 2 偏离记录 |
| `docs/phase2-plan.md` | 新增 | 本文档 |
| `docs/plan.md` / `docs/design.md` | **不修改** | Phase 2 不强制同步 |

**不改动**：`stash.rs` schema、`commands/*` 逻辑（除必要时 import 路径）、`runner.rs`。

---

## 9. TDD 实现波次

严格 **RED → GREEN → REFACTOR**。每波先写失败测试，再最小实现。

### Wave 0 — 脚手架与 impl-notes（0.5 天）

1. 创建 `docs/impl-notes/2026-08-31-specialized-reducers.md` 空模板
2. 创建 `src/display/`、`src/argv.rs` 空模块；`cargo test` 仍绿（92/92）

### Wave 1 — Foundation：行为等价 RED/GREEN（1–1.5 天）

**RED**

- 复制 Phase 1 condenser 断言到 `display/generic.rs` 单测
- 新增：`render_channel` 对 Generic 命令 + 长输出 ≡ 现 `condense()` 字节级一致
- 新增：`RAWREF_REDUCERS=0` ≡ Phase 1

**GREEN**

- 迁入 `generic.rs`；`condenser::condense` → `pub use display::generic::condense`
- `main.rs` 改调 `display::render_passthrough`（registry 空、仅 generic）
- **验证**：92/92 仍绿；零行为变化

**REFACTOR**

- `beneficial()` 单函数；marker 构建集中 `marker.rs`

### Wave 2 — argv 规范化（0.5 天）

**RED**：`argv.rs` 单测矩阵

| 输入 | 期望 |
|------|------|
| `pytest` | Pytest |
| `/usr/bin/pytest` | Pytest |
| `python3 -m pytest` | Pytest { module_invocation: true } |
| `cargo test` | CargoTest |
| `cargo test --lib` | CargoTest |
| `git diff` | Generic |
| `python3 -m unittest` | Generic |

**GREEN**：`normalize()` 实现；registry 用 `NormalizedCommand` 匹配

### Wave 3 — pytest reducer 独立（1.5–2 天）

**RED**（纯函数，读 fixture）

- passing_many → display 含 summary + marker + `view=pytest` + `recoverability=retrievable`；marker **不含** `omitted=`；不含全部 `.` 行
- failing_one → 含 traceback + short summary；**不含** middle passing dots
- error_setup → ERROR 段保留
- warnings_heavy → ≤5 行 warnings + 计数行
- no_tests_exit5 → 关键信息保留
- collect_only fixture → skip 专用（generic）
- never-worse：display.len() < raw.len()
- **禁止伪造**：成功 fixture 不含 `FAILED`

**GREEN**：`reducers/pytest.rs` 最小实现

**REFACTOR**：提取段落扫描 helper（仍保持纯函数）

### Wave 4 — cargo test reducer 独立（1.5–2 天）

**RED**

- passing_many / failing_one / json_format / non_utf8
- stderr compile_error → 专用 **不** 应用于 stderr
- 失败时 display 必含 `FAILED` 或 `failures:`
- never-worse + 禁止 failure→success

**GREEN**：`reducers/cargo_test.rs`

### Wave 5 — 集成与 E2E（1 天）

**RED**

- `integration_phase2.rs`：
  - pytest E2E（minimal tests）
  - cargo minimal-crate E2E
  - opt-out：`RAWREF_REDUCERS=0` 无 `view=`
  - unmatched `rawref seq 1 200` 与 Phase 1 字节等价
  - stash fail-open：无 marker、无 panic
  - t03 类 byte-exact get 对专用 marker 仍成立

**GREEN**：接好 registry；`default_registry()` 注册 pytest + cargo_test

### Wave 6 — 文档与质量门（0.5 天）

- 完成 impl-notes
- `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`

---

## 10. 验收矩阵

### 10.1 Phase 1 回归（必须 100% 保留）✅

| 套件 | 数量 | 要求 | 状态 |
|------|------|------|------|
| `rawref_lib` 单元 | 52 | 全过 | ✅ |
| `rawref` binary 单测 | 4 | 全过 | ✅ |
| `tests/cli_errors.rs` | 17 | 全过 | ✅ |
| `tests/integration_tests.rs` t01–t16 | 19 | 全过 | ✅ |
| **合计** | **92** | **92/92** | **✅** |

### 10.2 Phase 2 完整验收 ✅

| ID | 场景 | 断言 | 状态 |
|----|------|------|------|
| p01 | Generic 命令长输出 | 与 Phase 1 字节等价（`seq 1 200`） | ✅ |
| p02 | pytest 专用 | stdout 含 `view=pytest`、`recoverability=retrievable`；**不含** `omitted=`；get byte-exact | ✅ |
| p03 | cargo test 专用 | stdout 含 `view=cargo-test`、`recoverability=retrievable`；**不含** `omitted=`；get byte-exact | ✅ |
| p04 | never-worse | 各 fixture `display.len() < raw.len()` 才 applied | ✅ |
| p05 | parse 失败 | 降级 generic marker，无 `view=` | ✅ |
| p06 | 非 UTF-8 | generic/raw；get 仍 byte-exact | ✅ |
| p07 | 无收益 | passthrough 无 marker | ✅ |
| p08 | Machine-readable gates | `--collect-only` / `-v` / `--message-format` → generic 行为 | ✅ |
| p09 | `RAWREF_REDUCERS=0` | 无专用 view；generic 等价 Phase 1 | ✅ |
| p10 | unmatched 命令 | 字节等价 Phase 1 | ✅ |
| p11 | stash fail-open | 无 reducer；raw 输出；exit=子进程 | ✅ |
| p12 | exit code | pytest/cargo 失败仍透传非零 exit | ✅ |
| p13 | marker 契约 | `extract_ref_id` 成功；generic 含精确 `omitted=`；specialized 含 `recoverability=retrievable` 且不含 `omitted=` | ✅ |
| p14 | stderr 编译错误 | 不被 cargo 专用误判为 success summary | ✅ |
| p15 | 成功路径不伪造 | 无失败时不出现 `FAILED` 文本 | ✅ |
| **Focused tests** | pytest + cargo reducers | pytest：39 source unit + 68 focused；cargo：42 source unit | ✅ |
| **E2E black-box** | 18 integration scenarios | simulated pytest/cargo scripts, TempDir isolated | ✅ **303/303 绿** |
| **真实工具复验** | Cargo + pytest 8.4.2，pass/fail | specialized marker、exit、failure evidence、raw SHA/size | ✅ |

### 10.3 质量门 ✅

| 门 | 标准 | 状态 |
|----|------|------|
| `cargo fmt --check` | exit 0 | ✅ |
| `cargo clippy --all-targets -- -D warnings` | exit 0 | ✅ |
| `cargo test --locked` | **303/303** 全绿 | ✅ |
| `cargo build --release` | exit 0 | ✅ |

---

## 11. 风险、回滚与工作量

### 11.1 风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| pytest/cargo 输出格式版本差异 | parse 失败频繁 | parse fallback → generic；fixture 锁常见格式 |
| 专用 reducer 误删失败证据 | agent 误判 | 失败块全保留；never-worse；get 恢复 |
| marker 变长导致 never-worse 失败 | 专用不生效 | 接受；generic 兜底 |
| 模拟脚本与真实工具版本存在格式差异 | 真实环境 parse fallback 频率偏高 | fixture 锁常见格式；E2E 用隔离模拟可执行文件验证完整 CLI 链路；未知格式保守降级 generic |
| 宽行/少行 tail 回归 | 违反 I8 | 复用 Phase 1 fixture |

### 11.2 回滚

1. **运行时**：`RAWREF_REDUCERS=0` → Phase 1 generic 行为
2. **代码**：revert Phase 2 commit；92 测试为安全网
3. **数据**：无 schema 变更；Stash blob 与 Phase 1 兼容

### 11.3 工作量估算（1 人）

| 波次 | 人天 |
|------|------|
| Wave 0–1 Foundation | 1.5 |
| Wave 2 argv | 0.5 |
| Wave 3 pytest | 2 |
| Wave 4 cargo | 2 |
| Wave 5 集成 E2E | 1 |
| Wave 6 文档 QA | 0.5 |
| **合计** | **~7.5 人天** |

---

## 12. Phase 3 预告（本阶段不实现）

- Streaming spill-to-disk（大输出 RAM 问题）
- Quota / 磁盘上限 / ref 数量限制
- Stats（token 节省估算、retrieve 命中率）
- git diff/status reducer
- `--message-format=json` / nextest 专用解析
- SQLite schema：`reduction_kind`、`recoverability` 持久化
- 真实 temp git repo E2E

---

## 13. Phase 2 实现与集成完成（2026-08-31）✅

Phase 2 完整实现（Waves 0–5）已完成：

- [x] **D1** 本文档 `docs/phase2-plan.md` 已作为实现权威
- [x] **D2** 现有 **92/92** Phase 1 测试全部通过，无删除、无弱化断言 ✅
- [x] **D3** `display/` + `argv` 框架就位；reducer 纯函数、无 IO/spawn/exit 副作用 ✅
- [x] **D4** pytest reducer（39 source unit + 68 focused）+ cargo test reducer（42 source unit）+ **18 E2E** 集成完成 ✅
- [x] **D5** 三级 pipeline candidate → generic → raw 实现且单测覆盖 ✅
- [x] **D6** never-worse 按字节严格（含 marker）；Phase 1 宽行 fixture 仍 passthrough ✅
- [x] **D7** marker 契约：`extract_ref_id` 有效；generic Phase 1 格式（含精确 `omitted=`）；specialized 含 `view=`/`mode=`/`recoverability=retrievable`、**不含** `omitted=` ✅
- [x] **D8** `RAWREF_REDUCERS=0` 回退 Phase 1 generic 行为已测 ✅
- [x] **D9** stash fail-open 跳过 reducer；`output get` byte-exact + SHA 与 Phase 1 一致 ✅
- [x] **D10** passthrough gate：`--collect-only`、`-v`、`--message-format` 等不走专用 ✅
- [x] **D11** 未匹配命令与 Phase 1 **字节等价** ✅
- [x] **D12** `docs/impl-notes/2026-08-31-specialized-reducers.md` 记录完整实现细节与所有 Q1–Q4 closed ✅
- [x] **D13** `cargo fmt` / `clippy -D warnings` / `cargo test --locked` **303/303** / `cargo build --release` 全绿 ✅
- [x] **D14** **未**引入 schema migration、spill、stats、quota、hook、git diff reducer ✅

**当前状态**：Phase 2 ✅ 完成；两个 reducer 均已实现；总测试
**303/303** 全绿；最终集成审查与独立真实工具复验均通过。

---

## 附录 A：与 RTK / Tokenless 的对照（设计参考，非复制）

| 概念 | RTK | Tokenless | rawref Phase 2 |
|------|-----|-----------|----------------|
| Registry | 命令 → 过滤器 | middleware 链 | `Registry` + `NormalizedCommand` |
| Parse fallback | 失败 → 原样/通用 | arbitration | candidate → generic → raw |
| Never-worse | 输出长度 | recoverability | 字节严格 `<` |
| Reduction vs recoverability | 多种 lossiness 混用 | REVERSIBLE 为主 | `ReductionKind`（inline）+ `Recoverability::Retrievable`（端到端）；语义摘要 inline 有损 |
| Exit 保护 | 不改 exit | N/A | I4 强制 |
| Stash | tee / 外部 | StashLedger | 自有 SQLite（Phase 1，不变） |

## 附录 B：测试命令速查

```bash
# Phase 1 回归（92 tests）
cargo test --lib runner
cargo test --lib stash
cargo test --lib condenser
cargo test --test integration_tests

# Phase 2 专用 reducer focused tests
cargo test --locked --lib display::reducers::pytest      # 39 tests
cargo test --locked --test reducers_pytest               # 68 tests
cargo test --locked --lib display::reducers::cargo_test  # 42 tests

# Phase 2 集成测试（18 E2E black-box）
cargo test --locked --test integration_phase2

# 所有测试（303 total）
cargo test --locked

# Opt-out 手动验证
RAWREF_REDUCERS=0 cargo run --quiet -- seq 1 200

# 质量门
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build --release
```
