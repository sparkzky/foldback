# rawref — 架构与设计

> **读者**：需要修改或扩展 rawref 的维护者。
> **权威来源**：本仓库 `src/` 与 `tests/`；`docs/plan.md` 为 Phase 1 规格，**实现与规格冲突时以代码为准**，偏离在 §12 列出。
> **验收基线**（2026-08-31）：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release` exit 0；`cargo test --locked` **92/92**；stdout/stderr SHA-256 byte-exact smoke、stderr exit 42、info/tail/grep 集成路径均通过。

---

## 1. 系统概览

rawref 是一个独立 Rust CLI，在 agent 调用外部命令时：

1. **只执行一次**底层命令，捕获 stdout / stderr / exit code；
2. **先**把完整 raw 写入本地 Stash（SQLite 元数据 + 文件 blob）；
3. **再**按阈值决定是否把终端可见输出精简，并插入 `[rawref ref=…]` marker；
4. 通过 `rawref output get|tail|grep|info|purge` **按需恢复**，恢复路径不重跑原命令、不再精简。

非交互、无 TTY、无 hook、无后台进程。适用有界生命周期的命令（测试、构建、查询），不适用流式 watch 或交互式 REPL。

---

## 2. 模块边界

```
rawref (binary, src/main.rs)
├── CLI 分发、环境路径、passthrough 编排
└── rawref_lib (src/lib.rs)
    ├── runner      — 子进程 capture，exit code 映射
    ├── stash       — SQLite + blob 读写、ref 生命周期
    ├── condenser   — 终端 display 精简（纯函数，无 IO）
    ├── error       — RawrefError 与 exit code 映射
    └── commands/   — output 子命令薄封装
        ├── get     — byte-exact 读出
        ├── tail    — 末 N 行
        ├── grep    — 子串行匹配
        ├── info    — 元数据人类可读输出
        └── purge   — 过期清理
```

| 模块 | 职责 | 不应承担 |
|------|------|----------|
| `main.rs` | argv 解析、`handle_run` / `handle_output` 编排、`RAWREF_DATA_DIR` | 存储细节、精简算法 |
| `runner` | `Command::output()` 一次调用、Unix signal → `128+sig` | Stash、condense |
| `stash` | ref 生成、SHA-256 写入与读路径校验、`data_dir`/`blobs/` 0700、blob/`meta.db` 0600、TTL、expiry 校验 | 终端输出、精简 |
| `condenser` | 阈值判断、head/tail/marker、无收益回退 | 任何持久化 |
| `commands/*` | 把 CLI 参数映射到 `Stash` API，写 stdout | 业务逻辑重复 |

**依赖方向**：`main` → `commands` / `condenser` / `runner` / `stash` → `error`。`condenser` 与 `runner` 互不依赖。

**crate 布局**：二进制入口 `src/main.rs`，库 crate 名 `rawref_lib`（`src/lib.rs`），供单元测试与逻辑复用。

---

## 3. CLI 分发

入口 `main()` 手工解析 `std::env::args()`（`match` + 循环），**无 clap 等 CLI 框架依赖**（见 §12 与 plan Wave 0 的差异）。

```
argv[1] 分支:
├── "output"  → handle_output(args[2..])  → exit 0–3（管理命名空间）
├── "run"     → 需 "--" 分隔符；之后 args 作为 handle_run 输入
└── 其他      → 隐式 passthrough：argv[1..] 全部作为外部命令
```

**保留命名空间**：首参数为 `output` 或 `run` 时进入 rawref 语义。要执行名为 `output` / `run` 的外部命令，必须用 `rawref run -- output …`。

| 模式 | exit code 命名空间 |
|------|-------------------|
| Passthrough（隐式或 `run --`） | 子进程 code（0–127 或 128+signal）；exec 失败 → 127 |
| `rawref output …` | rawref 自有 0–3（§9） |

无参数时打印 usage 到 stderr，exit 2。

---

## 4. Passthrough 时序（单次执行 + raw-first）

```
handle_run(command, args)
│
├─1─ runner::capture()          ← 唯一 Command::output() 调用
│       └─ CaptureResult { stdout, stderr, exit_code, cwd, … }
│
├─2─ Stash::open(data_dir)
│       └─ 失败 → stash_result = None，stderr 告警，跳至 4（fail-open）
│
├─3─ stash.save(SaveArgs { … ttl_secs: DEFAULT_TTL_SECS })
│       └─ 失败 → stash_result = None，stderr 告警
│       └─ 成功 → (ref_id, expires_at)
│
├─4─ 写终端
│       ├─ stash 成功：
│       │     condense(stdout, ref_id, expires_at) → write stdout
│       │     condense(stderr, ref_id, expires_at) → write stderr
│       └─ stash 失败（fail-open）：
│             write 原始 stdout / stderr，**无 marker、无精简**
│
└─5─ return captured.exit_code   ← 始终透传子进程 code，与 stash 成败无关
```

### 4.1 不变量

| # | 不变量 | 实现位置 |
|---|--------|----------|
| I1 | 底层命令恰好启动一次 | `runner::capture` |
| I2 | 精简输入来自内存中已 capture 的 raw；Stash 在 condense 之前完成（或 fail-open 跳过 condense） | `handle_run` 顺序 |
| I3 | `output get` 等路径只读 Stash，不调用 `runner` | `commands/*` |
| I4 | stdout / stderr 独立 blob，metadata 分 channel 记录 size 与 SHA-256 | `stash::save` |
| I5 | ref_id 128-bit 随机 hex，非内容寻址 | `gen_ref_id` |
| I6 | `expires_at < now` 的 ref 读操作返回 `Expired`（exit 1） | `stash::meta` |

### 4.2 stdout / stderr / exit 语义

- **Capture**：pipe 模式，非 TTY；两通道全量读入 `Vec<u8>`（当前无 spill）。
- **Passthrough 写终端**：stdout 内容写进程 stdout，stderr 写进程 stderr；**不保证**跨通道交错时序（与子进程运行时序不同，capture 已分离）。
- **写失败**：`write_passthrough_output` 在 stdout/stderr 写入失败时向**对侧**流打印 `[rawref] warning: … write failed: …`（best-effort，写入 warning 本身也可能失败）；**始终**返回子进程 exit code，不因终端 IO 错误改变 exit。
- **Exit code**：`status.code()` 原样；Unix signal 终止 → `128 + signal`；无法解析 → 128。
- **Exec 失败**（命令不存在）：stderr 打印错误，exit 127；无 capture、无 stash。

---

## 5. 存储：SQLite + Blob

### 5.1 目录布局

```
$RAWREF_DATA_DIR/          # 默认 ~/.local/share/rawref
├── meta.db                 # SQLite WAL
└── blobs/
    ├── <ref_id>.stdout
    └── <ref_id>.stderr
```

`RAWREF_DATA_DIR` 优先；否则 `XDG_DATA_HOME/rawref`；再否则 `$HOME/.local/share/rawref`。**三者均不可用则报错**（passthrough fail-open；`rawref output …` exit 3）。**无 `/tmp` fallback**。

`Stash::open` 显式设置权限：`data_dir` 与 `blobs/` → **0700**；`meta.db` → **0600**（WAL sidecar 权限不保证，由 0700 父目录约束访问）。

### 5.2 `refs` 表

| 列 | 类型 | 说明 |
|----|------|------|
| `ref_id` | TEXT PK | 32 字符 hex |
| `command` | TEXT | argv[0] |
| `args_json` | TEXT | `serde_json` 序列化的 argv[1..] |
| `cwd` | TEXT | capture 时 `current_dir()` |
| `created_at` | INTEGER | Unix 毫秒 |
| `expires_at` | INTEGER | Unix 毫秒 |
| `exit_code` | INTEGER | 子进程 exit code |
| `stdout_size` | INTEGER | 字节数 |
| `stderr_size` | INTEGER | 字节数 |
| `stdout_sha256` | TEXT | hex digest |
| `stderr_sha256` | TEXT | hex digest |

打开时执行：`PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;` + `CREATE TABLE IF NOT EXISTS`。

### 5.3 ref 与 SHA-256

- **生成**：`rand::thread_rng()` 取 16 字节 → `hex::encode` → 32 字符小写 hex。
- **校验**：`validate_ref_id` 要求长度 32 且 `is_ascii_hexdigit()`（大小写均可）；非法 → `InvalidRef`（exit 2）。
- **SHA-256**：save 时对 blob 全文计算 digest 存入 metadata；**所有 retrieve 路径**（`get`/`tail`/`grep`，含 `both` 与 offset/limit）先**全量读入并校验 size + SHA-256**，校验通过后再切片/行操作。不匹配 → `RawrefError::Corrupted` → **exit 3**（区别于 NotFound/Expired 的 exit 1）。
- **一次 capture 一个 ref_id**：stdout / stderr 各一文件，共享同一 ref_id 与 metadata 行。

### 5.4 save 顺序与原子性

```
write_blob(.stdout) → write_blob(.stderr) → INSERT refs
```

- **非事务、无跨 FS+SQLite 两阶段提交**：blob 先落盘再 INSERT。
- **普通错误路径 best-effort rollback**：stderr blob 写入失败 → 删除已写 stdout blob；INSERT 失败 → 删除两 blob。约束违反、I/O 错误等**可恢复**失败不会故意留 orphan。
- **进程 crash / panic 窗口**：blob 已落盘但 INSERT 未完成（或 rollback 未执行）时，**仍可能**留下孤儿 blob。
- **无 ref 碰撞处理**：依赖随机 128-bit；碰撞时 PRIMARY KEY 冲突 → `Storage` 错误 → rollback。
- **Blob 文件权限**：Unix `OpenOptionsExt::mode(0o600)`，owner rw only。

### 5.5 TTL 与 purge

| 常量 | 值 |
|------|-----|
| `DEFAULT_TTL_SECS` | `7 * 24 * 3600`（7 天） |

- `save` 时 `expires_at = now + ttl_secs`（调用方当前固定 `DEFAULT_TTL_SECS`，无 CLI/env 覆盖）。
- **读路径**：`meta()` 在 SELECT 成功后比较 `expires_at_ms < now_ms` → `Expired`。
- **`purge --expired`**：
  1. `SELECT ref_id WHERE expires_at < now`
  2. 对每个 ref 删除 `.stdout` / `.stderr`（删除失败静默忽略）
  3. `DELETE FROM refs WHERE expires_at < now`
  4. stdout 打印 `purged N expired ref(s)`
- **无自动 purge**：需显式调用或外部 cron。
- **purge 与 expiry 检查**：purge 按 DB 时间戳删；已 purge 的 ref → `NotFound`（非 `Expired`）。

---

## 6. Condenser 决策

常量（`condenser.rs`）：

| 常量 | 值 |
|------|-----|
| `CONDENSE_LINE_THRESHOLD` | 100 行 |
| `CONDENSE_BYTE_THRESHOLD` | 10_240 字节 |
| `HEAD_LINES` / `TAIL_LINES` | 各 20 |

### 6.1 触发与回退

```
exceeds = len > 10KB OR line_count > 100
├─ false → display = raw, condensed = false
└─ true  → build head(20) + marker + tail(20)
           ├─ len(display) < len(raw) → condensed = true
           └─ else → display = raw, condensed = false  （无收益回退）
```

- **阈值 OR 关系**：任一触发即尝试精简。
- **line_count**：按 `\n` 切分；末尾无 `\n` 的片段计为一行。
- **宽行 fixture（21–40 行、每行 >400B、总超 10KB）**：head(20)+marker+tail 必须保留 tail 行；`omitted=0` 时不得丢弃 tail（历史 bug 已修，见 `test_byte_threshold_with_few_lines_preserves_head_and_tail`）。
- **二进制 / 非法 UTF-8**：Stash 与 `get` 保持 byte-exact；condense 若产生更短 display（含 UTF-8 marker），则终端可能看到 marker + 部分二进制 head/tail；若无收益则原样输出 raw bytes。

### 6.2 Marker 格式

```text
[rawref ref=<32-hex> raw=<bytes>b lines=<n> omitted=<m> expires=<YYYY-MM-DDTHH:MM:SSZ>]
```

- 插入在 head 与 tail 之间，自带 trailing `\n`。
- **每通道独立 marker**：stdout / stderr 分别 condense，同一 ref_id 但 `raw` / `lines` / `omitted` 按通道计算。
- **fail-open 路径无 marker**：stash 失败时不调用 condenser。

### 6.3 与 Stash 的边界

`condenser` 是纯函数，不读盘。Raw-first 由 `handle_run` 保证：仅 stash 成功后传入 `ref_id` / `expires_at`。

---

## 7. Retrieve bypass（output 子命令）

所有 `rawref output *` 路径：

1. `Stash::open` — 失败 exit 3；
2. 调用 `commands::*::run` → `stash.read_*` / `meta` / `purge_expired`；
3. 结果 **直接 `write_all` 到 stdout**，不经 `condenser`；
4. 返回 rawref 管理 exit code。

| 子命令 | 默认 | 实现要点 |
|--------|------|----------|
| `get` | `--channel stdout` | `read_channel` 支持 offset/limit；见下 §7.1 `both` 语义 |
| `tail` | `--lines 10`，channel stdout | **`--channel both` 在 CLI 层拒绝**（exit 2，`BadInput`）；仅 stdout/stderr |
| `grep` | channel **both** | 子串匹配（非 regex）；UTF-8 行用 `str::contains`，二进制行用 byte window |
| `info` | — | 人类可读 metadata，不写 blob |
| `purge` | 必须 `--expired` | 否则 exit 2 |

### 7.1 `Channel::Both` 与 range 语义

与 `docs/plan.md` §3.3 一致：**`both` = stdout 字节串后接 stderr 字节串**（无分隔符、不重排跨通道时序）。

| 操作 | 语义 |
|------|------|
| 无 offset/limit | 返回完整 `stdout ‖ stderr` |
| 有 offset 和/或 limit | 在**拼接后的单一逻辑字节流**上应用**一次** range（`--limit N` 最多返回 N 字节） |

**实现**（`stash::read_verified_full` + `apply_slice`，`Channel::Both` 分支）：

1. 分别**全量读入并校验** stdout、stderr blob（size + SHA-256）；
2. `combined.extend(stderr)` 形成单一逻辑流；
3. 对 `combined` 应用 `offset`（默认 0）与 `limit`（可选）：`start >= len` → 空；否则取 `[start..]` 再按 limit 截断。

单通道（`stdout` / `stderr`）同样先 `read_and_verify_blob` 全文校验，再切片。

`tail` / `grep` 经 `read_verified_full` 取得已校验全文，再做行级操作（grep 默认 channel 为 `both`）。

示例（已由单元测试 `test_channel_both_offset_limit`、`test_channel_both_offset_limit_crosses_boundary` 验证）：

- stdout=`0123456789`，stderr=`ABCDEFGHIJ` → 拼接流 20 字节
- `--channel both --offset 3 --limit 4` → `3456`（字节索引 3..7）
- stdout=`AAABBB`，stderr=`CCCDD` → `--offset 5 --limit 4` → `BCCC`（跨 stdout/stderr 边界切片）

**已知代价**：`both` 与单通道 retrieve 均需先全量读入并校验 blob 再切片；超大 ref 有 RAM 压力（§11）。

---

## 8. 并发、原子性与 fail-open

### 8.1 并发 capture

- 多进程/多线程共享同一 `RAWREF_DATA_DIR`：依赖 SQLite WAL + OS 文件锁。
- ref_id 随机独立；集成测试 t13、单元测试 `test_concurrent_saves_no_collision`（8 线程）验证无碰撞、数据不串。
- **已知**：高并发下可能 SQLite `BUSY` → passthrough 路径 stash 失败 → fail-open（仍输出 raw）。

### 8.2 fail-open（Stash 失败）

| 场景 | 行为 |
|------|------|
| `data_dir()` 不可用 | stderr `[rawref] stash unavailable (fail-open): …`；写 raw；子进程 exit code |
| `Stash::open` 失败 | stderr `[rawref] stash open failed (fail-open): …`；写 raw；子进程 exit code |
| `save` 失败 | stderr `[rawref] stash write failed (fail-open): …`；同上 |
| 只读 data dir（t07） | 仍 exit 0（子进程成功时），输出完整 |

**约束**：fail-open 时**不**精简、**不**插入 marker；agent 无法从输出中获得 ref。

### 8.3 残余缺口（已知，非 bug）

| 缺口 | 说明 |
|------|------|
| **crash 原子性** | save 在 blob 落盘与 INSERT 之间被 kill/panic → 孤儿 blob 可能残留（普通错误路径已 rollback） |
| **purge / read TOCTOU** | purge 删 blob 失败不阻止 DELETE metadata → 可能残留孤儿 blob；`chmod` 与 open 之间存在窄 TOCTOU 窗口（本地威胁模型可接受） |
| **终端写失败** | 已向对侧流打印 warning；子进程 exit 不变；warning 写入本身 best-effort |
| **retrieve 全量 RAM** | 所有读路径先全文读入再校验/切片（含 tampered range 亦校验全文后才拒） |

无 save 后读回 digest 二次校验（retrieve 路径已 gate）；无 `purge-orphans` 子命令。

---

## 9. 错误码

### 9.1 管理命令（`rawref output …`）

| Code | `RawrefError` | 场景 |
|------|---------------|------|
| 0 | — | 成功 |
| 1 | `NotFound`, `Expired` | ref 不存在或已过期 |
| 2 | `InvalidRef`, `BadInput` | 缺参数、未知 flag、ref 格式非法 |
| 3 | `Storage`, `Io`, **`Corrupted`** | SQLite/文件 IO（含 `Stash::open` 失败）；**blob size 或 SHA-256 不匹配** |

`RawrefError::exit_code()` 集中映射；错误消息打印到 stderr（`rawref: …`）。

### 9.2 Passthrough

| 场景 | Code |
|------|------|
| 子进程正常退出 | 子进程 code |
| Signal 终止（Unix） | `128 + signal` |
| 无法 exec | 127 |
| Stash 失败 | **仍用子进程 code** |

两套命名空间刻意分离：管理 0–3 vs passthrough 0–127+。

---

## 10. 威胁模型与信任边界

### 10.1 信任边界

```
┌─────────────────────────────────────────┐
│  Agent / 调用方 shell                    │  显式前缀 rawref；可见 condensed + marker
└─────────────────┬───────────────────────┘
                  │ spawn once
┌─────────────────▼───────────────────────┐
│  rawref 进程                             │  读写 RAWREF_DATA_DIR；不 sandbox 子命令
└─────────────────┬───────────────────────┘
                  │ exec
┌─────────────────▼───────────────────────┐
│  底层命令（不可信输出内容）               │
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│  本地 Stash（同 UID 可读）               │  data_dir/blobs 0700；meta.db + blob 0600
└─────────────────────────────────────────┘
```

### 10.2 假设与风险

| 风险 | 缓解（当前） | 残余 |
|------|-------------|------|
| 命令输出含密钥/PII 落盘 | blob 0600、目录 0700、TTL、手动 purge | 同 UID 进程可读；backup 同步可能泄露 |
| ref 被猜测/遍历 | 128-bit 随机 ref，非内容寻址 | 本地 attacker 仍可通过读 data dir 枚举文件 |
| 跨 session 引用旧 ref | TTL + Expired | 过期前任何持有 ref 者可 `get` 全文 |
| 精简丢失 agent 可见上下文 | marker 提示 + tail/grep/get | agent 可能不 follow up |
| 磁盘耗尽 | save 失败 → fail-open | 大输出全量 RAM + 磁盘，无配额 |
| 子命令恶意行为 | **不**隔离；rawref 与子命令同用户 | 常规 shell 风险 |
| SQLite / blob 损坏 | retrieve 全量 size+SHA 校验 → `Corrupted` exit 3 | 无自动 repair；tampered 字节不会部分返回 |

### 10.3 非目标（安全）

- 不加密 at-rest；不提供 ref  capability 撤销（除 TTL/purge）。
- 不验证子命令完整性；metadata 中的 command/args 仅供 audit。

---

## 11. 已知限制

1. **全量内存 capture**：`Command::output()` 将整个 stdout/stderr 读入 RAM；超大输出 OOM 风险。
2. **无磁盘配额 / 条目上限**：仅 TTL 与手动 purge。
3. **无 streaming / spill-to-disk**（§13.1）。
4. **无专用 reducer**：仅通用 head/tail（§13.2）。
5. **grep 子串语义**：`item 1` 匹配 `item 10`；非 regex。
6. **跨通道时序**：不重建 stdout/stderr 交错。
7. **Unix only**：0700/0600 权限、signal exit；**无 Windows** 支持。
8. **非交互**：**无 PTY / TTY** 模拟。
9. **无后台 daemon**：需显式 `purge --expired` 或外部 cron。
10. **`CaptureResult.started_at_ms` / `duration_ms`**：runner 填充但未写入 Stash。
11. **retrieve 全量读入 RAM**：所有读路径（含 `both` + offset/limit、tail、grep）先全文读入并校验再操作；超大 ref 有内存压力。
12. **save 无 crash 原子性**：进程 crash 仍可能 orphan blob（普通错误路径已 best-effort rollback）。
13. **purge / read TOCTOU**：purge 删 blob 失败不阻止 metadata DELETE；`Stash::open` chmod 存在窄 TOCTOU。
14. **TTL 不可配置**：无 CLI flag / env 覆盖 7 天默认。

---

## 12. 与 `docs/plan.md` 的偏离

| 条目 | plan | 实现 / 状态 |
|------|------|-------------|
| CLI 解析 | Wave 0 列 `clap` | 手工 `match` / 循环解析；**依赖中无 clap**（已从 `Cargo.toml` 移除） |
| `grep` 默认 channel | 未明确写默认值 | 默认 `both`（`main.rs` `parse_grep`） |
| `tail --channel both` | plan 未详述 | **CLI 拒绝** `both`（exit 2）；grep/get 仍支持 `both` |
| `CaptureResult` 时间字段 | 概念类型含 `started_at_ms` / `duration_ms` | runner 有，**未** persist 到 SQLite |
| plan 附录 B | 「以 plan 为规范」 | **本文档以代码为准**（plan 自身亦注明测试优先） |
| SHA 校验时机 | plan implied integrity | **retrieve 路径全量 size+SHA 校验** → `Corrupted` exit 3；save 时写入 digest |
| save 原子性 | plan 未详述 | blob-first、非事务；**普通错误 best-effort rollback**；**crash 仍可能 orphan** |
| 写终端 IO | plan 未详述 | 失败向对侧流打印 `[rawref] warning: …`；**不覆盖**子进程 exit |
| data dir 解析 | plan 默认 `~/.local/share/rawref` | `RAWREF_DATA_DIR` → `XDG_DATA_HOME/rawref` → `$HOME/.local/share/rawref`；**无 `/tmp` fallback** |
| 目录权限 | plan 0600 blobs | `data_dir`/`blobs/` **0700**；`meta.db` + blob 文件 **0600** |
| condenser 宽行 tail | — | 21–40 宽行超字节阈值时 tail 不得被丢弃（已修） |

其余核心契约（raw-first、fail-open、exit 分离、阈值、marker 格式、ref 规则、WAL、`both` 拼接与 range 语义 §7.1、集成测试 t01–t16、`tests/cli_errors.rs` 管理路径）与 plan 一致。

**回归**（2026-08-31 最终验收）：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release` exit 0；`cargo test --locked` **92/92**（lib 52 + doc 4 + integration 17 + cli_errors 19）。

---

## 13. 扩展点

### 13.1 Streaming spill-to-disk

**插入点**：`runner::capture` 与 `stash::save`。

```
当前:  Command::output() → Vec<u8> → write_blob
目标:  超阈值流式写 temp/spill → blob 或分段 blob
       metadata 增加 spill_path / chunked 标志
```

**不变量约束**：raw-first 仍在 condense 前完成**完整**持久化；fail-open 语义不变。需定义：spill 文件权限、save 失败清理、与 offset/limit 的 seek 语义。

**连带修改**：`condenser` 输入可能改为「内存窗口 + spill 后端」或仅对 metadata 采样；集成测试需大输出 fixture。

### 13.2 命令专用 reducer

**插入点**：`handle_run` 在 `stash.save` 成功后、`condenser::condense` 之前，按 `command` / argv  dispatch。

```
stash.save → [optional: reducer::for_command(cmd)] → condense 或 reducer 产出 display
```

**不变量**：完整 raw **必须**已入库；reducer 只影响 display，不影响 blob。`output get` 路径不变。

候选：`git diff`、`pytest`、`cargo test` 等结构化摘要；回退到现有通用 condenser。

### 13.3 其他自然扩展

| 扩展 | 建议位置 | 状态 |
|------|----------|------|
| 可配置 TTL | `SaveArgs.ttl_secs` 已有；暴露 env/flag 于 `handle_run` | 未实现 |
| grep regex | `stash::grep_lines` 或新子命令 | 未实现 |
| save 事务性 | `stash::save`：temp blob + rename + INSERT 同一连接事务 | 未实现；crash 窗口仍可能 orphan |
| 读时 SHA 校验 | 所有 retrieve 路径 `read_and_verify_blob` | **已实现** |
| `both` range 流式 seek | 避免 both+offset/limit 时全量读入两 blob（§7.1 已知代价） | 未实现；仍全量读入+校验 |
| `purge-orphans` | 清理无 metadata 的 blob | 未实现 |

---

## 14. 开放问题

1. **grep 默认 channel**：实现为 `both`，plan 表格未写死；是否改为 `stdout` 以与 `get` 一致？
2. **crash 孤儿 blob 清理**：普通 save 错误已 rollback；是否仍需 `purge-orphans` 或 write-temp + rename + INSERT 同事务以消除 crash 窗口？
3. **fail-open 可观测性**：stash 失败仅 stderr 一行；是否需要 structured log / exit code 区分「stash 失败但命令成功」？（终端写失败已向对侧 warning，不覆盖 child exit。）
4. **condense 与二进制**：大体积二进制超阈值但 condensed 更短时，marker 嵌入是否 acceptable？是否应对非 UTF-8 禁用 condense？
5. **并发 SQLite BUSY**：重试策略 vs 立即 fail-open？
6. **`both` range 流式实现**：当前全量读入+校验后 slice；是否在超大 blob 上改为跨文件 seek 以避免 RAM？
7. **`duration_ms` 入库**：是否有 audit 需求写入 `refs` 表？
8. **容量上限**：是否在 save 前拒绝超 N GB 输出并 fail-open（避免 OOM）？
9. **ref 与多通道 marker**：两通道均精简时，agent 看到两个相同 ref_id 的 marker；是否需在文档/UX 上说明「一 ref 双通道」？

**已关闭（2026-08-31 验收，不再列为开放）**：retrieve 读路径 SHA 校验；data dir 无 `/tmp` fallback；0700/0600 权限；terminal write 可观测且不覆盖 child exit；`tail --channel both` CLI 拒绝；condenser 宽行 tail bug。

---

## 15. 修改风险速查

| 改动区域 | 可能破坏的不变量 | 必跑测试 |
|----------|------------------|----------|
| `handle_run` 顺序 | I2 raw-first、fail-open | t02,t03,t07 |
| `condenser` 阈值/算法 | marker 格式、无收益回退 | t02,t06, condenser 单元 |
| `stash::save` | blob 布局、ref 格式、0700/0600、rollback | stash 单元,t13 |
| `stash::read_*` | 全量 SHA 校验、Corrupted exit 3 | stash 单元 tampered-* |
| `stash::meta` expiry | I6 | t11,t15 |
| exit code 映射 | I4/I5 命名空间分离 | t05,t14,t15 |
| `grep`/`tail` 语义 | retrieve bypass | t09,t10 |
| `Both` range（§7.1） | plan both 拼接语义 | `test_channel_both_offset_limit*` |
| 新增 reducer | raw-first、get byte-exact | t03 + 新 fixture |

**发布前检查**：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo build --release`、`cargo test --locked`（2026-08-31 基线 92/92）。
