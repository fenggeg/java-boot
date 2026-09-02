# P1 阶段设计说明：崩溃恢复(R3) + 就绪判定(R5)

> 交付序 P1。验收标准：`taskkill /F` 强杀 UI 后所有服务存活；daemon 崩溃/重启后服务三态
> 正确分类；「干净重启」后端口、日志归属新 run\_id。手动验收脚本：`scripts/p1-crash-recovery.ps1`（本机已通过）。

## 1. 本次新增/改动

| 模块          | 改动                                                 | 要点                    |
| ----------- | -------------------------------------------------- | --------------------- |
| `proc.rs`   | 就绪判定 + 状态机 + 崩溃恢复                                  | 见下                    |
| `job.rs`    | **移除** **`KILL_ON_JOB_CLOSE`**                     | 支撑 R3：daemon 退出不连坐子进程 |
| `store.rs`  | `list_specs_all` / `list_run_pids`                 | 供恢复分类反查               |
| `server.rs` | 启动时 `recover()`、`recovery.*` 路由、hello 上报 pending   | <br />                |
| jb-core     | `RecoveryEntry` / `RecoveryKind` / `recovery.*` 方法 | <br />                |
| launcher    | `daemon_recovery_*` 代理命令                           | UI 处置入口               |

## 2. 就绪判定（R5）

- 主通道：若 `SpawnRequest.startup_port` 有值，后端对 `127.0.0.1:port` 做 **TCP connect**
  探测，500ms 间隔、上限 300s，成功即置 `Running`。

- 兜底：管道 reader 对每行做 `Started\s+\S+\s+in\s+.*seconds` 正则与
  `APPLICATION FAILED TO START` / `BUILD FAILURE` 匹配，命中即置 `Running`/`Error`。

- 状态机：`Starting → (Running | Error) → Stopped`；受 `advance_if_starting` 保护，
  避免 Running 被误回退。每次转移发 `proc.status` 事件。

单元测试覆盖：就绪正则命中 Spring 启动行、失败标记、噪声行忽略。

## 3. 崩溃恢复（R3）

### 3.1 三态判定（daemon 启动时执行）

```
枚举存活 java 进程 (sysinfo)
├─ service_run.pid 精确命中 且 有 process_spec  → Exact（可接管/可干净重启，带 run_id+spec）
├─ 命令行含某 spec 的 module_name / main_class  → Fuzzy（归属待确认，无 run_id）
└─ 其余                                     → Unknown
```

结果存 `ProcService.recovery`，经 `recovery.list` 上报；`hello.has_pending_recovery` 提示 UI。

### 3.2 处置（任选其一）

- **接管监控** `recovery.takeover {pid}`：把它编入 `runs`（adopted、无管道），`proc.list`
  可见、`proc.stop` 可终止。

- **干净重启** `recovery.restart {pid}`：用原 spec `spec_to_request`（env 已脱敏键剔除，
  见限制）→ 新 run\_id/spawn；日志归属新 run，续传靠 append。

- **忽略** `recovery.ignore {pid}`：仅出队。

### 3.3 关键决策：Job 不设 KILL\_ON\_JOB\_CLOSE

- 若 daemon 的 Job 设了 KillOnJobClose，daemon 一退（含崩溃/升级）子进程必死，R3 无从恢复。

- 改为「Job 托管 + 显式 `terminate_pid` 优雅停止」：daemon 退出不连坐，停止仍可靠。

- 语义已在 ADR-0001 决策 4 同步更新（状态：Accepted）。

### 3.4 已知限制（文档化）

- spec 中**已脱敏**的环境变量无法回填空密，`recovery.restart` 重放时剔除这些键——
  无法恢复凭据类环境（干净重启不传输 secret）。

- 接管/重启不对**未由本 daemon 管道持有的进程**做日志采集（接管仅跟踪 pid 与状态）。

单元测试覆盖：`cmd_contains` 模糊特征匹配、`spec_to_request` 剔除脱敏 env。

## 4. 手动验收（已在本机执行通过）

```
powershell -ExecutionPolicy Bypass -File scripts\p1-crash-recovery.ps1
```

断言（全部通过）：

1. R5：banner 服务（打 `Started DemoApplication in ...seconds`）被正则判定为 `running`
2. R3：强杀 daemon 后子进程存活（验证 Job 无 KillOnJobClose）
3. R3：新 daemon 启动 `recovery.list` 把该进程分类为 `exact`、`had_spec=true`、run\_id 一致
4. R3：`recovery.takeover` 后 `proc.list` 可见该 pid，状态 `running`

