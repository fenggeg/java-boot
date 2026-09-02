# ADR-0001　Launcher(UI) + 常驻 Daemon 架构

- 状态：已接受（Accepted）

- 日期：2026-09-02

- 决策者：javaboot-launcher 架构组

- 相关的废弃记录：无（首条架构级 ADR）

## 背景

当前 `javaboot-launcher`（Tauri 2 + React）是一个**单进程全有状态**应用：所有
进程树 / 日志 / 扫描等有状态职责与 UI 生命周期强耦合，逐条对照本改造目标：

| # | 现状代码位置                                                             | 缺陷                                            |
| - | ------------------------------------------------------------------ | --------------------------------------------- |
| 1 | `src/process/*`，日志仅存内存（`log_buffer_lines`）                         | stdout/stderr 匿名管道由 UI 持有，UI 重启日志流断裂；历史日志重启即丢 |
| 2 | `src/process/job.rs` 的 `JobObject` 由 UI 进程创建（KILL\_ON\_JOB\_CLOSE） | UI 崩溃会把所有 java 子进程连坐杀死                        |
| 3 | `manager.rs::restore_running_services` 仅能识别存活 java PID             | 无法恢复启动参数 / 环境变量等上下文                           |
| 4 | `pom::scan_project` 串行全量扫描                                         | 大项目长时间空白等待，无进度推送、不可取消，二次打开重新扫                 |
| 5 | `log_pipe.rs` 仅依赖 `Started ... in ... seconds` 正则                  | 定制 banner 的工程会误判状态                            |

## 目标

把系统从「UI = 一切」改造为「UI = 无状态视图，daemon = 全部有状态职责」：

```
Launcher(Tauri UI, 可随意崩溃重启)
   └─ JSON-RPC 2.0 over Windows Named Pipe（请求/响应 + 服务端事件流 + 心跳重连）
javaboot-daemon.exe（Rust 常驻进程，UI 无关，不放入任何 Job Object）
   ├─ ScanService     pom 遍历 / 主类探测 / 进度流 / 可取消 / 结果缓存
   ├─ ProcService     进程 spawn / 优雅停止 / Job Object / 崩溃恢复三态 / 端口探测就绪
   ├─ LogService      子进程管道消费 → SQLite 批量写 + 文件镜像
   ├─ MonitorService  sysinfo 端口/CPU/内存采集 + 定时巡检
   └─ SQLite(WAL)     process_spec / service_run / service_log 三表
```

## 决策

### 决策 1　UI/Daemon 采用 Windows 命名管道 + JSON-RPC 2.0

- 管道名固定为 `\\.\pipe\javaboot-daemon`。

- 协议为 **JSON-RPC 2.0**：请求/响应带 `id`；事件用 **notification**（无 `id`）承载
  服务端到客户端的单向推送。

- 方法集（示例）：`scan.start` / `scan.cancel` / `proc.spawn` / `proc.stop` /
  `proc.list` / `log.tail` / `spec.get`；`daemon.hello` 为握手。

- 事件集：`scan.progress`（携带 scanId 流式推送）、`log.append`（run\_id + seq 增量）、
  `proc.status`、`daemon.hello`。

**为何不用其他通道**

| 候选              | 否决原因                              |
| --------------- | --------------------------------- |
| TCP localhost   | 触发 Windows 防火墙弹窗；端口冲突噪声；无内建身份隔离   |
| Unix socket     | Windows 10+ 支持差，语义与 Win32 进程模型不匹配 |
| 共享内存            | 无流式语义，无法承载事件流 / 心跳 / 重连           |
| 单独系统服务（SVCHOST） | 需管理员权限 + 安装服务，Leo 针对单用户桌面工具过重     |

命名管道是 Windows 原生、免防火墙、可设管道 ACL 的 IPC，天然匹配「随用随拉起、
UI 崩溃不影响 daemon」的目标。JSON-RPC 2.0 轻量、无状态、多语言友好，前端
`src/ipc/` 封装层可把 id 映射做 Promise，事件走订阅分发，与命令通道天然解耦。

### 决策 2　Daemon 生命周期：随用拉起 + 空闲自杀

- 版本握手：UI 连接时客户端发 `daemon.hello`（携带 `client_version`）；服务端回
  `daemon.hello`（携带 `daemon_version` + `min_client_version`）。

  - 不兼容判定：`client_version < min_client_version` 或 `daemon_version <
    min_client_version` → UI 弹「版本不兼容，请升级」并拒绝继续。

- 拉起：UI 启动即尝试连接命名管道。连接失败（管道不存在 / 拒绝连接）→ 从安装包
  附带路径拉起 `javaboot-daemon.exe`，轮询等待管道就绪，超时报错。

  - 用互斥（管道已占用即视为已在运行）保证**单实例**，防止双 daemon 冲突。

- 心跳：UI 每 5s 发 `ping`（JSON-RPC method，无副作用）；任何通知/响应都会重置
  daemon 侧的「UI 活跃计数器」。

- 重连：UI 侧连接断开后**指数退避**（1s→2s→…封顶 30s）重连；重连成功后做**全量对账**
  ——重新拉 `proc.list` + 端口状态 + 各 run 的 `log.tail` 游标，UI 从持久化事实重建视图。

- 自杀：当「无运行中服务」且「无 UI 连接」持续 10 分钟，daemon 自我退出。

  - 反例约定：任何正在托管的服务 run 存在，则绝不自杀。

**边界**：daemon 不放入任何 Job Object；UI 崩溃只丢「网络连接」，任务/日志/进程
全在 daemon 侧继续，UI 重连即恢复——这是本 ADR 最基本的正确性承诺。

### 决策 3　数据模型：运行事实三表归 daemon，配置仍归 launcher

配置数据（`projects` / `services` / `app_config` / `service_dependencies`，见
`schema.rs` v1\~v6）维持放在 **launcher 的 SQLite**，因为它们是用户可编辑的声明式
配置，易失、规模小、与 UI 编辑流强相关。

新增的**运行事实**统一放 **daemon 侧自己的 SQLite（WAL）**：

```sql
CREATE TABLE process_spec (
  run_id INTEGER PRIMARY KEY, project_id TEXT NOT NULL, module_name TEXT NOT NULL,
  main_class TEXT, classpath_key TEXT, jvm_args TEXT,   -- JSON array
  env_vars TEXT,        -- JSON object；敏感键值已脱敏为 "«redacted»"
  working_dir TEXT, dev_mode INTEGER, auto_restart INTEGER,
  log_file TEXT, launcher_version TEXT, created_at INTEGER
);
CREATE TABLE service_run (
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT, module_name TEXT,
  pid INTEGER, started_at INTEGER, exit_code INTEGER, exit_at INTEGER
);
CREATE TABLE service_log (
  run_id INTEGER NOT NULL, seq INTEGER NOT NULL, ts INTEGER NOT NULL,
  stream TEXT NOT NULL, level TEXT, body TEXT NOT NULL,
  PRIMARY KEY (run_id, seq)
) WITHOUT ROWID;
```

PRAGMA：`journal_mode=WAL; synchronous=NORMAL; busy_timeout=5000`。

- `run_id` 全局递增、跨 run 唯一；`service_log` 以 `(run_id, seq)` 为复合主键
  `WITHOUT ROWID`，支持 UI 用游标 `log.tail(run_id, seq)` 增量拉取。

- **双写语义**：

  1. spawn 前 `INSERT service_run` 拿到 `run_id`；
  2. 写 `process_spec`（SQLite + 工作目录旁 `.spec.json` 两份，供崩溃恢复反查）；
  3. spawn 成功回写 `pid`；
  4. 退出时回写 `exit_code / exit_at`。

- 环境变量持久化**脱敏**：键名匹配 `PASSWORD|SECRET|TOKEN|KEY|CREDENTIAL`
  （大小写不敏感）时，值统一替换为 `«redacted»`；脱敏在**写库前**进行，内存中仍保留
  明文用于实际 spawn。

### 决策 4　Job Object 所有权移交 daemon

- Job Object 继续由 **daemon** 创建并持有（daemon 自身绝不放入任何 Job），子进程整树挂入
  以便统一托管。

- **不设** **`KILL_ON_JOB_CLOSE`**：daemon 自身退出（含崩溃/升级/强杀）**不会**连坐杀子进程，
  这与 R3 崩溃恢复「枚举存活 java → 三态分类」互相支撑。停止一律由 daemon 显式
  `terminate_pid`（优雅终止）完成，8s 轮询真退出后再做端口释放检查。

- UI 不持有任何 Job 句柄；UI 崩溃不影响任何子进程。

### 决策 5　日志：管道消费 → 双写（SQLite + 文件镜像）

- daemon 持有子进程 stdout/stderr 管道并持续消费，防止缓冲区满反向阻塞子进程。

- 双写：

  - **SQLite**：`service_log` 结构化、可回溯查询；

  - **文件镜像**：`<working_dir>/.javaboot/<module>.log` 追加写，便于查看原始流。

- 写库走 `tokio::sync::mpsc` 缓冲区，**200ms 定时 或 500 条阈值** 触发批量 flush，
  事务内 `prepare_cached`；SQLite 写统一 `spawn_blocking`，禁在 async 上下文做阻塞 IO。

- UI 断连期间日志照常落 ★ daemon 侧 ★ 不丢；重连后按游标增量补发 `log.append`。

- 保留策略：默认 14 天，每小时后台清理；单 run 超 50MB 时保留首尾 5MB，中间掐断并
  插入 `[TRUNCATED]` 标记。

### 决策 6　崩溃恢复三态（对账驱动的状态上报）

daemon 启动时枚举存活 java PID，逐 PID 判定：

1. **spec 精确反查**：`.spec.json` / `process_spec` 里记录的 PID 仍存活 → 绑定该 run；
2. **命令行特征模糊匹配**：无 spec，但其命令行含目标 classpath/module 特征 → 建立一个
   `Unknown` 待确认条目；
3. **未知**：两者都不匹配 → `Unknown`。

恢复后向 UI 上报三态列表，UI 提供三种处置：

- **接管监控**（继承原 run，不改 PID / 日志归属）；

- **干净重启**（用原 spec 重建：新 run\_id、新 PID、原参数与环境，日志续传归档到新 run）；

- **忽略**。

### 决策 7　扫描服务：并行遍历 + 取消 + 进度流 + 缓存

- `ignore::WalkParallel` 并行遍历（自动遵守 `.gitignore`，跳过 `target/`），与
  `tokio_util::sync::CancellationToken` 联动，收到取消立即让回调返回 `WalkState::Quit`。

- pom 解析用 `quick-xml` **流式 Reader 定向提取** `<modules>/<module>/<packaging>/ <parent>`，禁止整树反序列化，避免大 pom 的内存与 CPU 尖峰。

- 进度：发现一个 module 上报一个 `scan.progress`（携带 scanId + 相对路径 + 计数），
  由 `scan.cancel` 随时打断；P2 验收要求 300+ module 全程有进度流且可取消。

- 结果缓存进 daemon SQLite，二次打开毫秒级返回（< 200ms）；缓存不命中才触发真扫，
  且取消不写坏缓存。

### 决策 8　就绪判定：端口探测为主，正则兜底

- 主通道：daemon 对目标端口做 **TCP connect 探测**（500ms 间隔，上限 300s），成功即
  running——对定制 banner 免疫。

- 兜底：保留现有 `Started ... in ... seconds` 正则。

- 任一命中即 `running`；`APPLICATION FAILED TO START` 判 `error`。

### 决策 9　监控服务：sysinfo 周期采样 + proc.metrics 事件

- `MonitorService` 每 2s 用 sysinfo 采每个托管进程 CPU / 内存，回填
  `ProcHandle.metrics`（`proc.list` / `daemon_reconcile` 据此返回真实指标），
  并把每次采样作为 `proc.metrics` 事件推送 UI。
- 采集是阻塞 sysinfo 调用，放 `spawn_blocking`；仅刷新进程维度控制开销。
- launcher 把 daemon 事件（`proc.metrics` / `proc.status` / `log.append` / 连接状态）
  经 Tauri `emit` 转发前端；前端 store 订阅并周期对账，顶栏展示连接状态与
  托管进程实时内存汇总。CPU 首次采样可为 0/低（差量计算），无端口进程就绪仍为
  Starting，不干扰就绪判定。

### 决策 10　启停/重启迁移：daemon 委托 + service_id↔run_id 映射

- launcher 仍是「编排者」（编译 / classpath / env / argfile / 端口冲突等留在本侧），
  但当 daemon 在线时，java 进程的 spawn / 管道消费 / 退出 / 就绪 / 指标由 daemon 承担。
- 新增 `process::delegate`：维护 `service_id ↔ run_id` 双向映射（全局单例），
  spawn 构造好 `SpawnRequest` 交给 `proc.spawn`，`stop`/`restart` 按映射委托 `proc.stop`。
- daemon 事件（`proc.status` / `proc.metrics` / `log.append`）只带 run_id，launcher
  按映射归一到 service 维度，复用既有的 `service://status` / `service://log` 事件通道，
  前端无需感知传输层变化。
- 超长命令行（@argfile / CLASSPATH 环境变量模式）暂保持本地启动，规避 daemon 侧
  `@argfile` 引号语义差异；daemon 离线时自动回退本地路径。

## 兼容与迁移

- launcher 侧存量 DB（projects/services/config）**原样保留、结构不变**，仅数据语义
  改为「配置」；运行事实服务由 daemon 私有 DB 承担，不共享内存。

- 改造期采用**渐进替换**：daemon 先接管 `proc.spawn/stop/log/tail`，配置 CRUD 命令
  过渡期可留在 launcher 侧；最终所有命令收敛到 `src/ipc/` 统一转发。

- 前端 `src/ipc/` 封装 JSON-RPC 细节，Zustand store 只感知语义方法，不感知传输协议，
  便于后续替换通道。

## 副作用 / 权衡

- **两进程 + IPC 序列化**带来 1 次进程边界拷贝，JSON-only 无二进制优化；对本工具
  （日志量大但低频推送、命令低频）可接受，且换取 UI「随意崩溃可恢复」。

- **双份 DB**（launcher 配置库 + daemon 事实库）需维护两套连接生命周期；通过统一
  `db` 抽象与 daemon 私有目录收敛，避免状态漂移。

- 首次引入 Named Pipe + 事件分发，需前端订阅生命周期（重连/失联/对账）的工程投入。

## 备选方案（已否决）

- 单进程内把状态移到后台线程 + UI 崩溃保护：无法做到 UI 崩溃/强杀后日志与进程独立
  存活，也不满足「UI 随意重启」。

- daemon 直接复用 launcher 同一 SQLite：会因 launcher 持锁导致 daemon 写库互斥，
  且运行事实与配置耦合，故单独建库。

## 相关要点

- 技术约束：tokio runtime；SQLite/文件一律 `spawn_blocking`；库代码 `thiserror`、
  命令边界 `anyhow`、运行时路径禁止 `unwrap/expect`。

- 全程 Windows 10/11，不引入 macOS/Linux 适配。

- 各模块先写单元测试：扫描取消、日志批量 flush、spec 脱敏、崩溃恢复三态判定。

