# foldback Phase 1 实现笔记

> 日期：2026-08-31
> 规范来源：`docs/plan.md`（冲突时 plan 为规范，本笔记只记录**实际偏离与决策**）
> 仓库状态：无 git commit（全部 untracked）；无可用 `git diff` 历史

---

## 计划偏离

### 放弃 fork RTK，改为独立实现

- **原因**：`docs/plan.md` §1 与附录 A 明确不 fork RTK、不复制源码；需自有 Stash 协议与 correctness contract。
- **实际处理**：全新 Rust crate（`foldback` / `foldback_lib`），参考 RTK 的「前缀 CLI + 输出精简」思路与 Tokenless 的「先存 raw 再展示精简视图」，代码路径为 `runner` → `stash` → `condenser` → `commands/*`。
- **备选/升级**：Phase 2+ 可按命令增加专用 reducer（plan §6.3、§12），仍须 raw-first，不引入 RTK 源码依赖。

### CLI 命名：`rev retrieve` → `foldback output get`

- **原因**：避免与 passthrough 子命令 `run` 冲突；管理面统一在 `output` 命名空间（plan §3.1、附录 A）。
- **实际处理**：
  - 主路径：`foldback <cmd> [args...]` 隐式 passthrough
  - 逃生：`foldback run -- <cmd> [args...]`
  - 恢复：`foldback output get|tail|grep|info|purge`
  - 保留命名空间：`output`、`run` 为 foldback 自身语义（`src/main.rs`）
- **备选/升级**：README 与 `print_usage()` 已对齐；后续可加 shell 补全，不改语义。

### MVP 仅通用 head/tail reducer，无专用命令过滤器

- **原因**：plan §1.2 非目标；专用 reducer（git/pytest 等）属 Phase 2+。
- **实际处理**：`condenser.rs` 实现阈值触发（>100 行或 >10 KB）→ head 20 + marker + tail 20；若 `len(condensed) >= len(raw)` 则回退为原样（`condensed = false`）。
- **备选/升级**：plan §12 列 git diff、pytest、cargo test 等专用提取；实现前须先 stash 完整 raw。

### CLI 解析为手写 `env::args`（plan Wave 0 曾列 `clap`）

- **原因**：MVP 命令面简单，手写分支即可满足 §3.1 契约；未引入 clap derive。
- **实际处理**：`src/main.rs` 与各 `commands/*` 均为手动解析 flag。`Cargo.toml` **已无** `clap`、`anyhow` 依赖（曾出现在早期 scaffold，已清理）。
- **备选/升级**：子命令增多时可引入 clap，属 refactor，不改变对外契约。

### 读路径 SHA-256 校验（相对 `docs/design.md` §5.3 偏离）

- **原因**：design 初稿将 SHA-256 标为「写入时计算、读时不校验」；代码复审要求 retrieve 路径在返回字节前校验 size + digest，tamper 须 exit 3（`FoldbackError::Corrupted`）。
- **实际处理**：`stash::read_and_verify_blob` 在 `get` / `tail` / `grep` 全路径读完整 blob 后比对 `stdout_size`/`stderr_size` 与 metadata SHA-256；`get --channel both` 两通道各自 verify 后再拼接。
- **备选/升级**：超大 blob 可改为流式 hash + 分段读；MVP 仍全量读入（见边界情况 RAM 条目）。

### `data_dir` 无 `/tmp` fallback（相对 design §5.1 初稿偏离）

- **原因**：静默 fallback `/tmp` 在多用户/容器环境有数据隔离与权限风险；复审要求显式失败。
- **实际处理**：`main.rs::data_dir()` 优先级 `FOLDBACK_DATA_DIR` → `XDG_DATA_HOME/foldback` → `$HOME/.local/share/foldback`；三者皆不可用则返回 `Err`，管理命令 exit 3、passthrough fail-open 仍透传子进程 exit code（`cli_errors` e15/e16）。
- **备选/升级**：文档已更新 README；不设隐式 fallback。

---

## 边界情况

### `Command::output()` 全量内存驻留

- **原因**：`runner::capture` 使用 `std::process::Command::output()`，stdout/stderr 各为一个 `Vec<u8>`（`src/runner.rs`）；plan §11 已列为风险。
- **实际处理**：子进程结束前 buffers 常驻 RAM；stash 再写一份到磁盘 blob。`--channel both` 读路径亦须先加载 stdout + stderr 全文再拼接/切片（`read_verified_full`）。适用于有界生命周期命令，**不**适用于 GB 级输出或长驻流。
- **备选/升级**：plan §12「Streaming spill」— 捕获阶段 spill-to-disk，condense/read 按需 mmap 或分段读。

### 每通道 byte-exact，但不重放 stdout/stderr 交错顺序

- **原因**：pipe 捕获按通道聚合；plan §1.2、§2 #3、design §2.4 明确非目标。
- **实际处理**：`<ref>.stdout` / `<ref>.stderr` 独立 blob；`get --channel both` 为 **stdout 字节串 + stderr 字节串** 拼接（`read_verified_full`），非运行时交错顺序。`offset`/`limit` 作用于拼接后**整体**字节流（`test_channel_both_offset_limit*`），非 per-channel 各切一段。
- **备选/升级**：若未来需要时序，须额外记录 `(channel, offset, ts)` 事件流；MVP 不承诺。

### condenser：少行宽 payload 的 tail 保留

- **原因**：行数 ≤40 但每行 >400 B 时可超 10 KB 阈值且 `omitted=0`；旧 `build_condensed` 在 `tail_count > 0` 分支外错误丢弃 tail（约第 21–25 行，fixture 25 条宽行）。
- **实际处理**：`build_condensed` 在 `tail_count > 0` 时显式 append `lines[tail_start..]`；`test_byte_threshold_with_few_lines_preserves_head_and_tail` 锁定 head 0–19 与 tail 20–24 均保留；因无压缩收益仍 `condensed = false` passthrough。
- **备选/升级**：无；属 correctness 修复。

### `FOLDBACK_DATA_DIR` 隔离，无 session attribution

- **原因**：Tokenless 在 agent 框架内做 session/tool 归属；foldback 为独立 CLI，plan §8 用目录隔离替代跨 session 探测防护。
- **实际处理**：默认 `~/.local/share/foldback`（或 `XDG_DATA_HOME`）；集成测试与并发测试均 `env("FOLDBACK_DATA_DIR", tmp.path())`。ref_id 为 128-bit 随机 hex（`gen_ref_id`），**非**内容寻址。
- **备选/升级**：多项目可设不同 `FOLDBACK_DATA_DIR`；Phase 2 可选 wrapper 脚本，仍非透明 hook。

### 二进制 / 非法 UTF-8

- **原因**：plan G6、t06 要求存储与 `get` byte-exact；display 侧 condense 可能插入 UTF-8 marker。
- **实际处理**：stash 与 `output get/tail/grep` 走原始字节；condense 仅在省空间时替换 display。`grep` 对非 UTF-8 行用 byte window 匹配（`stash::test_binary_grep` 已通过）。
- **备选/升级**：超大二进制且超阈值时，condense 可能因「无收益回退」而 passthrough display（仍已 stash raw）。

### Stash 失败 fail-open；save 普通错误 best-effort rollback

- **原因**：plan §2 #2、G5；子进程已成功时不得因存储失败改变 exit code。
- **实际处理**：`handle_run` 中 `Stash::open/save` 失败 → stderr 告警 `[foldback] stash ... (fail-open)` → 原样写 raw stdout/stderr → 仍返回 `captured.exit_code`（t07 只读目录验证）。`save` 在 stderr blob 写失败时删除已写 stdout blob；DB INSERT 失败时删除两 blob（`test_db_insert_failure_cleans_up_blobs`）。
- **残余**：进程在 blob 写完后、rollback 前 **crash/kill** 仍可能留下 orphan blob；无跨 FS+SQLite 两阶段提交（见开放问题）。
- **备选/升级**：可选 `--strict-stash` 类 flag（plan 未要求，见开放问题）。

### passthrough 终端 write 失败可观测

- **原因**：pipe 捕获后写回调用方 stdout/stderr 若 BrokenPipe 等，原先静默吞错。
- **实际处理**：`write_passthrough_output` 在 write 失败时向对侧通道打印 `[foldback] warning: stdout|stderr write failed: …`；不 panic、不改变 exit code（`main.rs` 单测 4 项）。
- **备选/升级**：无。

### `tail` 拒绝 `--channel both`

- **原因**：tail 为行级操作；`both` 拼接流上 tail 语义易与 `get` 混淆；MVP 仅 stdout/stderr。
- **实际处理**：`parse_tail` 遇 `both` → `BadInput` exit 2（`cli_errors` e17）；`get`/`grep` 仍支持 `both`。
- **备选/升级**：若需 both-tail，须定义跨通道行边界语义。

### Signal 退出码

- **原因**：plan §7.1 Unix 约定 `128 + signal`。
- **实际处理**：`runner::exit_code_from_status` 在 `status.code()` 为 None 时用 `ExitStatusExt::signal()`；未知 signal 返回 128。**无 signal 端到端集成测试**（见开放问题）。

### purge / read TOCTOU

- **原因**：`purge_expired` 先删 blob 再 DELETE metadata；并发 `get` 可能在 blob 已删、行仍在（或反之）的窗口遇到 `NotFound`/`Corrupted`/IO 错误。
- **实际处理**：MVP 接受；无 ref 级锁。`Stash::open` 对 `meta.db` chmod 0600 与 create 之间存在窄 TOCTOU（代码注释已注明）。
- **备选/升级**：ref 级 lease、purge 与 read 串行化，或 write-temp + rename 原子 blob 替换。

---

## 保守决策

### 不做透明 hook / TTY / watch / Windows

- **原因**：plan §1.2 非目标；降低 MVP 面与平台矩阵。
- **实际处理**：
  - **无 hook**：agent 须显式前缀 `foldback`（README、plan §1.2）
  - **无 TTY**：`Command::output()` pipe 捕获；`print_usage()` 与 README Limitations 已声明
  - **无 watch/server**：单次 CLI invocation，无 daemon、无文件监听
  - **无 Windows**：blob `mode(0o600)`、`cfg(unix)` signal 映射；非 Unix 编译路径未实现
- **备选/升级**：plan §12 列 opt-in 别名、Windows 移植、MCP/SDK adapter；均为后续波次。

### 存储目录与 blob 权限显式设置

- **原因**：依赖 umask 不可靠；复审要求 owner-only。
- **实际处理**：`Stash::open` → `data_dir`/`blobs_dir` **0700**，`meta.db` **0600**（`set_mode`，chmod 错误不忽略）；blob 写入 `OpenOptionsExt::mode(0o600)`（`test_data_dir_and_db_permissions`、`test_blob_permissions`）。
- **备选/升级**：NFS/容器 mount 可能削弱 chmod 效果；仍依赖 `0700` data_dir 边界。

### Raw-first 顺序

- **原因**：plan §2 #2 不变量；与 RTK「成功路径常不可逆」对比（plan §5）。
- **实际处理**：`capture` → `stash.save`（完整 raw）→ `condenser::condense` 写终端；`output *` 子命令不 condense、不写 stash（`get.rs` 等）。
- **备选/升级**：专用 reducer 接入时须保持同一顺序。

### 管理命令 exit code 与子进程分离

- **原因**：plan §7.2；避免与 passthrough 0–127+signal 混淆。
- **实际处理**：`FoldbackError::exit_code()` → NotFound/Expired=1，InvalidRef/BadInput=2，Storage/Io/Corrupted=3；passthrough 始终用子进程 code（`main.rs` 注释与 t14/t15、t05b exit 42、`tests/cli_errors.rs` 验证）。

### grep 为字面量子串，非 regex

- **原因**：plan §3.3、§11；MVP 简单可预测。
- **实际处理**：`grep_bytes` / `matches_pattern` 按行 `contains`；二进制行用 byte window 匹配。
- **备选/升级**：Phase 2 可选 `--regex`（plan §12）。

### 无磁盘容量硬上限

- **原因**：plan §8「MVP 无硬上限」。
- **实际处理**：save 失败则 fail-open；无 quota/TTL 以外的大小拒绝逻辑。
- **备选/升级**：文档 + 后续 streaming；可选 `--max-bytes` 拒绝捕获（未实现）。

---

## 开放问题

### 残余风险（已知未闭合）

| 项 | 状态 |
|----|------|
| save crash 后 orphan blob | **未闭合** — 普通错误路径 rollback 已测；kill/panic 窗口仍在 |
| `Command::output()` + `both` 读路径 RAM | **接受** — 全量 Vec；plan §11 / §12 spill 未做 |
| purge ↔ read TOCTOU | **接受** — 无 ref 锁；见边界情况 |
| Signal exit `128+sig` 端到端集成测试 | **未做** — 仅 `runner` 单元逻辑 |
| CI 流水线 | **无** — 仓库无 workflow 配置 |
| 磁盘 quota / `--max-bytes` | **无** — plan §8 MVP 刻意不设硬上限 |

### 产品 / adoption

- **无 hook 的 agent 改调用习惯**：plan §11 列为风险；README 有示例，**无** adoption 数据。
- **TTL 不可 CLI 配置**：`DEFAULT_TTL_SECS = 7 天` 硬编码（`stash.rs`）；plan §8 称 per-call 未暴露为 flag。
- **retrieve 二次 token 爆炸**：plan §11；`--offset/--limit`、tail、grep 已实现，**无**自动分页或默认 limit。

### 与 plan Wave checklist

- plan §9 Wave 0–5 代码路径已存在（`main`、`runner`、`stash`、`condenser`、`commands/*`、集成与 CLI 错误测试）。
- Wave 6 质量门（fmt / clippy / release / smoke）**本会话已本地验证**；CI 自动化仍缺。

---

## 验证证据

### 质量门（2026-08-31 终审）

| 命令 / 项 | 结果 |
|-----------|------|
| `cargo fmt --check` | exit 0 |
| `cargo clippy --all-targets -- -D warnings` | exit 0 |
| `cargo build --release` | exit 0 |
| `cargo test --locked` | exit 0，**92/92** 全绿 |
| `cargo check --locked` | exit 0 |

**测试分布（92 项）**：`foldback_lib` 单元 52（condenser 16 + stash 31 + runner 5）+ `foldback` binary 单测 4（`write_passthrough_*`）+ `tests/cli_errors.rs` 17 + 集成 t01–t16 共 19。全部通过 `FOLDBACK_DATA_DIR=<tempdir>` 隔离（集成与 CLI 错误测试）。

### Smoke / 端到端行为

| 场景 | 结果 | 证据 |
|------|------|------|
| `seq 1 200` → ref → `output get` byte-exact | 通过 | t03 |
| stdout / stderr 分通道恢复 byte-exact | 通过 | t04；读路径 SHA-256 verify |
| stderr 子进程 exit 42 透传 | 通过 | t05b |
| `output tail` / `output grep` / `output info` | 通过 | t09、t10、t12 |
| 代码复审 7 项 | 全部 closed | 见上文边界/保守决策条目 |

### 已覆盖行为（按 plan 映射）

| 范围 | 证据 | 对应 plan |
|------|------|-----------|
| 短输出 passthrough | t01、t01b | G1、§6.1 |
| 长输出 marker | t02 | G3、§3.4 |
| byte-exact `output get` + 读时 integrity | t03、`test_tampered_*` | G2、G4 |
| stdout/stderr 通道隔离 | t04 | §2 #3 |
| exit code 透传 0/1/42 | t05、t05b、t05c | G1、§7.1 |
| 非法 UTF-8 恢复 | t06 | G6 |
| stash fail-open | t07 | G5 |
| get offset/limit（含 both 整体 range） | t08、`test_channel_both_offset_limit*` | §3.3 |
| tail / grep | t09、t10 | §3.3 |
| 过期 + purge | t11 | §7.2、§8 |
| info 字段（含 SHA-256） | t12 | §4.2 |
| 并发 6 ref 不串 | t13 | G7 |
| invalid/not-found ref | t14、t15、`cli_errors` e07–e10 | §7.2 |
| `run --` 逃生 | t16 | §3.1 |
| CLI 错误路径 | `cli_errors` e01–e17 | §7.2 |
| save rollback / orphan 清理 | `test_db_insert_failure_cleans_up_blobs` | Wave 2 |
| 权限 0700/0600 | `test_data_dir_and_db_permissions`、`test_blob_permissions` | Wave 2 |
| 无 data_dir fail-open / exit 3 | e15、e16 | Wave 2 |
| tail 拒绝 both | e17 | Wave 2 |
| condenser 宽行 tail | `test_byte_threshold_with_few_lines_preserves_head_and_tail` | Wave 3 |
| 终端 write 可观测 | `write_passthrough_*` ×4 | Wave 1 |

### 依赖

- `Cargo.toml` 无 `clap`、`anyhow`

---

*本文件仅记录实现期事实与决策；功能变更请同步更新 `docs/plan.md` 或在本文件追加 dated 条目。*
