# P0 阶段设计说明：daemon 骨架 + IPC + 数据层(R2) + 日志采集(R6)

> 交付序 P0。验收标准：`taskkill`/强杀 UI 后重启，java 服务无感、日志在 UI 上无缝续传，
> 且重启前的日志可回溯查询。手动验收脚本：`scripts/p0-daemon-smoke.ps1`（本机已通过）。

## 1. 工程结构落地

```
src-tauri/
  Cargo.toml            # 主包（launcher），新增 workspace members
  shared/  jb-core      # 协议/模型/脱敏/常量，纯库、零 IO
  daemon/  javaboot-daemon.exe   # dev: target/debug/ 下
  src/ipc.rs            # launcher 侧 JSON-RPC 客户端（R1 数据面）
```

## 2. 新增/改动文件一览

| 文件                            | 职责         | 要点                                                               |
| ----------------------------- | ---------- | ---------------------------------------------------------------- |
| `shared/src/lib.rs`           | jb-core 根  | 暴露 protocol/model/redact/consts                                  |
| `shared/src/consts.rs`        | 协议常量       | 管道名、版本、心跳/重连/空闲自杀/日志阈值                                           |
| `shared/src/model.rs`         | 数据模型       | `ProcessSpec`/`ServiceRun`/`LogLine`/`SpawnRequest`/`ProcStatus` |
| `shared/src/protocol.rs`      | JSON-RPC   | 请求/响应/通知 + NDJSON 编解码 + 方法/事件常量                                  |
| `shared/src/redact.rs`        | 脱敏         | 键含 PASSWORD/SECRET/TOKEN/KEY/CREDENTIAL → `«redacted»`           |
| `daemon/src/main.rs`          | 入口         | 单实例 + runtime 上下文构建 AppState                                     |
| `daemon/src/app.rs`           | 装配         | Store/LogPipeline/JobObject/ProcService + 事件总线                   |
| `daemon/src/store.rs`         | 数据层        | WAL 三表 + spawn\_blocking 封装 + 批量写 + tail                         |
| `daemon/src/log_pipe.rs`      | 日志管线       | mpsc 攒批(200ms/500条) + 双写 + 镜像                                    |
| `daemon/src/proc.rs`          | 进程         | spawn/stop/管道 reader/退出码/JobObject                               |
| `daemon/src/job.rs`           | Job Object | KILL\_ON\_JOB\_CLOSE + 按 PID Terminate                           |
| `daemon/src/server.rs`        | 服务端        | 命名管道 accept + 会话 + 路由 + 心跳/空闲自杀/清理                               |
| `launcher src/ipc.rs`         | 客户端        | 连接/hello/请求/事件/心跳/退避重连/拉起 daemon                                 |
| `launcher src/commands.rs`    | 命令代理       | daemon\_\* 系列转发命令                                                |
| `scripts/p0-daemon-smoke.ps1` | 验收         | 双客户端模拟 UI 崩溃→重启                                                  |

## 3. 关键设计决定（可复现决策的过程笔记）

### 3.1 协议：NDJSON over Named Pipe，JSON-RPC 2.0

- 管道名 `\\.\pipe\javaboot-daemon`。

- 一字节流，按 `\n` 分帧；`serde_json::to_string` 产紧凑 JSON（不含裸换行）。

- 请求带 `id`（u64），响应携带同 `id`；事件用无 `id` 的 notification（`log.append`/`proc.status`）。

- 握手门禁：未 `daemon.hello` 只允许 HELLO；违反返回 `ERR_HANDSHAKE_REQUIRED`。

### 3.2 运行时注意：AppState 必须在 tokio runtime 上下文内构建

- `LogPipeline::spawn` 内部 `tokio::spawn`；若在 `main` 里 `block_on` 之前构造会触发
  「no reactor running」panic。故 `AppState::new()` 放入 `rt.block_on` 内。

### 3.3 进程优雅停止：Job Object + 按 PID Terminate，非 signal

- daemon 把子进程挂进 Job（KILL\_ON\_JOB\_CLOSE）；UI 崩溃不影响。

- `proc.stop` 经 `OpenProcess(PROCESS_TERMINATE)+TerminateProcess(pid)` 终止进程，
  然后轮询 `pid_slot` 清空（lifecycle 完成 `child.wait()` 收尾），8s 超时。

- 为什么不用「keep child in slot + lifecycle wait」：std/tokio 的 `Child::wait()`
  与 `Child::kill()` 互斥借用 `&mut self`，无法在等待中并发 kill；用 PID Terminate 解耦。

### 3.4 日志双写一致性

- 每条日志经 `proc.spawn_reader` 赋 `(run_id, seq自增)` → `LogPipeline.tx`。

- 后台攒批：200ms 定时 / 500 条阈值；flush 时 `store.write_logs`（内部 spawn\_blocking，
  `prepare_cached` 批量 INSERT OR IGNORE）+ `.javaboot/<module>.log` append（独立 spawn\_blocking）。

- 事件总线：`log.events`(broadcast) → `app` 转发为 `log.append` notification 广播给所有会话。

- UI 断连不影响写库；重连按 `(run_id, after_seq)` 增量补发。

### 3.5 脱敏边界

- 明文 `SpawnRequest.env_vars` 只用于实际 spawn；`ProcessSpec::from_request` 写库前
  经 `redact::redact_map` 脱敏，`process_spec.env_vars` 与 `.spec.json` 均为脱敏后 JSON。

## 4. 该阶段未做（进入 P1/P2）

- 端口就绪判定（R5，TCP connect 探测 + 正则兜底）——P1

- 崩溃恢复三态（见 ADR 决策 6）——P1

- 扫描 `ScanService`（R4，WalkParallel+CancellationToken+进度流）——P2

- 前端 `src/ipc/`（JS 封装层）与 Zustand 状态接入——P1 起随迁移推进

## 5. 手动验收（已在本机执行通过）

```
powershell -ExecutionPolicy Bypass -File scripts\p0-daemon-smoke.ps1
```

断言（全部通过）：

1. 客户端 1 断连（模拟 UI 崩溃）后，进程仍存活（`proc.list` 找到同 run\_id/pid）
2. 重连（模拟 UI 重启）后，`log.tail` 可回溯全部历史日志且条数不减少
3. `spec.get` 中 `env_vars` 敏感键已脱敏为 `«redacted»`
4. `proc.stop` 优雅终止后，进程从 `proc.list` 移除

