# Changelog

本项目所有显著变更将记录在此文件中。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本 2.0.0](https://semver.org/lang/zh-CN/)。

## [Unreleased]

## [0.16.0] - 2026-09-02

### 新增

- **独立 daemon 守护进程架构**：新增 `src-tauri/daemon/`（独立二进制 crate）与 `src-tauri/shared/`（`jb-core` 协议/模型共享 crate），将 Java 进程的 spawn / 管道消费 / 退出监控 / 就绪判定 / CPU 内存指标采集整体下沉到常驻 daemon；UI（launcher）崩溃或重启时 daemon 仍独立存活，托管服务不受影响，UI 重启后通过对账（`daemon_reconcile`）恢复实时事实与日志续传
- **IPC 客户端**（`src-tauri/src/ipc.rs`）：launcher 启动时自动连接 / 拉起 daemon，`IpcState` 管理 TCP 连接、握手（`HelloResult` 版本协商）、请求/响应、事件订阅；daemon 事件（日志 / 状态 / 监控指标 / 连接）转发到前端 `daemon-connected` / `daemon-disconnected` / `daemon-proc-metrics`
- **崩溃恢复**（P1）：daemon 启动时扫描磁盘上仍存活的 java 进程，三态处置——`recovery_takeover`（接管监控）、`recovery_restart`（用原 spec 干净重启，新 run_id，日志续传）、`recovery_ignore`（忽略）；launcher 启动时若 daemon 在线则走 `delegate::rebind` 重建服务映射，离线回退本地 `restore_running_services`
- **pom 扫描委托**（P2）：`daemon_scan_start` / `daemon_scan_cancel` 把 Maven 聚合 pom 解析下沉到 daemon，命中缓存秒级返回，否则后台扫描并通过事件推送进度 / 完成
- **监控闭环**（P3）：顶栏新增「守护」状态指示（在线绿点 / 离线灰点 + 托管运行数 + 合计内存），`App.tsx` 订阅 `daemon-proc-metrics` 实时更新 CPU / 内存，4 秒周期对账刷新进程列表；`store.ts` 新增 `daemonConnected` / `daemonHello` / `daemonProcesses` / `daemonMetricsAt` 状态与 `refreshDaemon` action
- **委托启动**（P4）：`process/delegate.rs` 在 daemon 在线且命令行未超长时，把 java 进程整体交给 daemon 托管（spawn / 管道 / 退出 / 就绪 / 指标均由 daemon 承担），`manager.rs` 的 `spawn_and_monitor` / `stop` 新增 daemon 分支；超长 classpath（`@argfile` 模式）保持本地启动避免引号语义差异
- **daemon 命令面**（`commands.rs`）：新增 12 个 Tauri 命令——`daemon_connected` / `daemon_hello` / `daemon_reconcile` / `daemon_ensure_started` / `daemon_spawn` / `daemon_stop` / `daemon_logtail` / `daemon_recovery_list` / `daemon_recovery_takeover` / `daemon_recovery_restart` / `daemon_recovery_ignore` / `daemon_scan_start` / `daemon_scan_cancel`
- **验收脚本**：`scripts/p0-daemon-smoke.ps1` ~ `p4-delegate.ps1` 覆盖 daemon 冒烟 / 崩溃恢复 / 扫描 / 监控闭环 / 委托启动五阶段验收
- **设计文档**：`docs/p0-design.md` / `docs/p1-design.md` 与 `docs/adr/` 记录 daemon 架构决策

### 变更

- **Cargo workspace 拆分**：`src-tauri/Cargo.toml` 新增 `[workspace] members = ["daemon", "shared"]`，主 crate 依赖 `jb-core = { path = "shared" }`；tokio 启用 `net` feature 支持 IPC TCP
- **服务恢复路径分流**：`lib.rs` 启动流程改为等 daemon 连接（2 秒延迟）后判断在线——在线走 `delegate::rebind` 重建映射（实时日志随事件恢复，无需重启服务），离线回退本地 `restore_running_services`
- **指标回填**：`manager.rs` 新增 `set_metrics` 方法，daemon 周期性回填的 CPU / 内存写入 runtime 并推送 `service://status`；`delegate::normalize_event` 把 daemon 进程事件归一到 service 维度驱动既有 `service://status` / `service://log`

### CI

- 构建流水线需适配 Cargo workspace（daemon / shared 子 crate 编译产物）

## [0.15.2] - 2026-09-02

### 修复

- **编辑器切换 tab 滚动位置跳回文件头**：`MonacoCodeEditor` 的 `previousPathRef` 初始为 `null`，首次切换标签时 `useLayoutEffect` 走 `prev === null` 分支，将初始标签的 viewState 错误地保存到新 path 名下，导致初始标签的滚动位置从未被保存，切回时回到文件头；改为在 `handleMount` 中将 `previousPathRef` 初始化为当前 `path`，删除 `prev === null` 分支，首次切换即走 `prev !== null && prev !== path` 分支正确保存初始标签状态
- **编辑器切换 tab 后 executeEdits 覆盖滚动位置**：`@monaco-editor/react` 的 `value` prop 变化时内部用 `executeEdits` 全量替换内容会重置滚动位置，而库的 `value` effect 在 `path` effect 之后、本组件恢复 effect 之前执行；恢复 viewState 的 `useEffect` 改用 `requestAnimationFrame` 延迟一帧，确保在库 `executeEdits` 完成后再恢复滚动位置
- **外部磁盘同步重置编辑器滚动位置**：`FilePanel` 的 `syncCleanTabsFromDisk` 重读未编辑标签内容时，当前激活 tab 的 `value` prop 变化触发库 `executeEdits` 全量替换，重置滚动位置；新增 `activePathRef` 实时镜像，`syncCleanTabsFromDisk` 更新激活 tab 内容前调用 `saveViewState`、更新后调用 `restoreViewState`（通过 `MonacoCodeEditorHandle` 新暴露的 `saveViewState`/`restoreViewState` 方法），保持外部同步时滚动位置不丢失

## [0.15.1] - 2026-09-02

### 修复

- **CPU 使用率超过 100%**：sysinfo 的 `cpu_usage()` 在采样窗口过短或首次采样时可能瞬时返回超过 100% 的值，`refresh_resource_usage` 采集时用 `.clamp(0.0, 100.0)` 限制到合法区间，避免服务卡片显示越界
- **编辑器切换 tab 回到文件头**：`MonacoCodeEditor` 的 `useLayoutEffect` 中 `previousPathRef` 初始为 `null`，首次切换标签时初始标签的滚动位置不会被保存，导致切回时回到文件头；新增 `prev === null` 分支，在首次切换时保存当前（初始）标签的视图状态

## [0.15.0] - 2026-09-01

### 新增

- **快速打开（Ctrl+P）**：文件浏览器新增项目级文件名/路径过滤跳转面板，`Ctrl+P` 唤起后输入文件名或路径片段，按文件名前缀 > 文件名包含 > 路径包含排序，↑↓ 选择、回车打开、Esc 关闭；打开时后台拉取全量扁平文件列表（`walkFiles` API），失败时保留上次结果退化为旧数据可用
- **ErrorBoundary 自动恢复**：渲染错误时新增「尝试恢复」按钮，重置组件状态（不刷新页面）重新渲染，最多自动恢复 2 次，超过后仅保留「重新加载」整页刷新

### 变更

- **FilePanel 拆分**：将 600+ 行的 `FilePanel.tsx` 拆分为 `filePanelUtils.ts`（文件类型分类、树节点类型、紧凑路径加载、不可变更新工具函数）、`FileTreeRow.tsx`（`FileTypeIcon` 与递归 `TreeRow` 行组件，含拖拽合法性判断与 drop 高亮）、`QuickOpenPanel.tsx`（快速打开面板）三个独立模块，`FilePanel.tsx` 仅保留面板编排逻辑
- **环境变量/覆盖属性公共逻辑提取**：`ProjectConfigModal` 与 `ServiceConfigModal` 中重复的 `parseEnvVars`/`serializeEnvVars`/`parseOverrideProperties`/`serializeOverrideProperties` 及 ID 生成器提取到 `src/utils/envVars.ts`，统一 ID 计数器避免两组件各自维护独立计数器导致 ID 碰撞；ID 生成改为 `Date.now()+random` 确保跨组件唯一
- **ServiceConfigModal 状态管理迁移 useReducer**：将散落的多个 `useState` 合并为单一 `useReducer`（`ConfigState` + `ConfigAction`），集中管理内存模式、端口、调试、覆盖属性、环境变量、依赖等配置字段，减少中间状态不一致
- **全局快捷键拦截移入 React 生命周期**：`main.tsx` 中顶层的 `document.addEventListener`/`window.addEventListener` 模块级副作用移入 `useGlobalKeyGuard` 自定义 Hook 的 `useEffect`，确保在 React 生命周期内执行并在卸载时清理监听器
- **注册表 PATH 读取改用 windows crate**：`process/env.rs` 的 `merge_registry_path` 从手写 `extern "system"` 绑定 `RegOpenKeyExW`/`RegQueryValueExW`/`RegCloseKey` 改为使用 `windows` crate 的 `Win32::System::Registry` 类型安全 API（`HKEY`/`KEY_READ`/`REG_VALUE_TYPE`/`PCWSTR`），`Cargo.toml` 启用 `Win32_System_Registry` feature

### 修复

- **watcher 重建无限循环**：worker panic 后的重建改为限制最大重试次数（5 次）+ 指数退避（5s/10s/20s/40s/80s），超过上限后停止重建避免根因持续存在时空转 CPU；正常退出（cancel 或 channel 断开）时重置重建计数
- **自动重启标志永久卡住**：`trigger_restart` 中 `RESTART_IN_PROGRESS` 标志的清理改为 RAII `RestartGuard`，确保 `compile_and_start` 正常返回、报错或 panic 时 guard drop 都会清理标志，避免永久卡住后续重启
- **退出超时死锁**：`lib.rs` 窗口关闭流程新增 10 秒兜底强杀线程，若 async task panic 或 runtime 提前关闭导致 `app.exit(0)` 未执行，10 秒后 `std::process::exit(0)` 强制退出，避免用户只能通过任务管理器强杀
- **Monaco 多标签滚动位置串扰**：`@monaco-editor/react` 自带 `saveViewState` 在切换标签时用「已更新为新 path」的闭包作为保存 key，导致把上一标签滚动位置错误覆盖到新标签名下；改为关闭库的 `saveViewState`，在组件内用 `useLayoutEffect`（先于库切换 model）按旧 path 保存、`useEffect`（库切换 model 后）按新 path 恢复，`viewStatesRef` 按 path 精确隔离
- **更新接口响应未校验**：`update.ts` 的 `checkForUpdate` 直接将 `res.json()` 断言为 `GithubRelease`，新增 `isGithubRelease` 运行时类型守卫校验 `tag_name`/`name`/`html_url`/`published_at`/`draft`/`prerelease`/`assets` 字段，格式异常时抛出明确错误而非访问 undefined
- **数据库迁移重复执行**：`schema.rs` 的 `run_migrations` 此前每次启动都顺序执行 v1~v6 全部迁移，改为读取 `PRAGMA user_version` 后仅执行未执行的版本，执行完毕更新 `user_version`，避免重复迁移与 seed 覆盖

### 优化

- **JDK/Maven 探测缓存改 RwLock**：`commands.rs` 的 `JDK_CACHE`/`MAVEN_CACHE` 从 `Mutex` 改为 `RwLock`，快速路径用读锁检查缓存命中（并发读不阻塞），慢路径用写锁 + 双重检查避免并发重复启动 JVM/Maven 探测进程
- **SYS 锁持锁时间缩短**：`manager.rs` 的 `restore_running` 与 `refresh_resource_usage` 拆分为两阶段——阶段 1 在 SYS 锁内仅采集进程存活状态/CPU 内存快照，阶段 2 释放 SYS 锁后仅持有 runtimes 锁写回，缩短 SYS 锁持锁时间，减少与 `wait_for_pid_exit`/`start` 的争用
- **Tab 未读状态精确订阅**：`App.tsx` 此前订阅整个 `logs` 对象导致任意服务一条日志触发所有 Tab 标签重算，改为精确订阅 `hasUnread` map（仅未读状态变化触发），用 ref 浅比较稳定引用避免每帧产生新对象导致 `useMemo` 失效
- **日志分类正则预编译**：`LogViewer.tsx` 的 `classifyLine` 每行调用时重复创建 `RegExp`，改为模块级预编译 `ERROR_RE`/`WARN_RE` 常量复用
- **store HMR 清理**：`store.ts` 新增 `import.meta.hot.dispose` 钩子，Vite 热更新时清除旧 `flushTimer` 与 `pendingLogs`，避免旧定时器触发新模块的 store
- **manager.rs start 流程拆分**：将 200+ 行的 `start` 方法拆分为 `prepare_start_placeholder`（清理残留 handle 并创建 placeholder）、`build_java_args`（构造 Java 启动参数）、`spawn_and_monitor`（spawn 子进程、绑定 Job Object、启动日志读取与 reaper）三个独立方法，降低单方法复杂度

## [0.14.0] - 2026-08-28

### 新增

- **服务打包**：服务卡片更多菜单新增「打包」操作，执行 `mvn clean package -DskipTests`（多模块加 `-pl <module> -am`），保留 spring-boot repackage 生成可执行 fat jar；打包时不停止运行中的服务（本项目用 exploded classpath `java -cp target/classes:...` 启动，JVM 不锁定 fat jar，与 IDEA 行为一致），打包后恢复原状态
- **项目批量打包**：项目行更多菜单新增「打包全部服务」操作，逐个打包项目下所有已添加服务（串行避免资源争抢），完成后弹窗列出每个服务的 jar 产物路径
- **打包产物定位**：打包成功后弹窗展示 jar 绝对路径与大小，提供「打开目录」（资源管理器定位 jar）和「复制路径」（写入剪贴板）两个操作；后端扫描 `target/*.jar` 智能识别产物（排除 `*-sources.jar`/`*-javadoc.jar`/`original-*`，取最大文件）
- **打开 target 目录**：服务卡片更多菜单新增「打开 target 目录」常驻项，随时在资源管理器中打开模块的 target 目录
- **文件树快捷键复制/粘贴**：文件树支持 Windows 快捷键 Ctrl+C 复制选中条目、Ctrl+V 粘贴到选中目录或其父目录，复用 `activePath` 选中状态

### 变更

- **移除终端功能**：彻底删除项目内所有终端相关功能（前端 + 后端 + 依赖），包括 `TerminalView.tsx` 组件、`terminal.rs` 后端模块、`FilePanel.tsx` 中终端抽屉（termOpen/termHeight/resize/工具栏按钮）、`api.ts` 中 4 个 terminal API、`styles.css` 中终端样式（约 220 行）、`commands.rs` 中 4 个 terminal 命令、`lib.rs` 中模块声明/命令注册/`kill_all` 调用；移除 `@xterm/xterm`、`@xterm/addon-fit` 前端依赖与 `portable-pty` 后端依赖；`Terminal` 图标保留（仅做 UI 占位）

### 优化

- **打包状态恢复**：打包成功/失败后均恢复打包前状态（`prev_status`），运行中的服务不被误标为 Stopped/Error

## [0.13.0] - 2026-08-28

### 变更

- **移除 Git 功能**：彻底删除项目内所有 Git 相关功能（前端 + 后端），包括 Git 工作区面板（状态/暂存/提交/历史/Diff）、文件树 Git 状态着色与目录聚合改动计数、编辑器行级 diff 标记条与 glyph margin 装饰、文件历史/回滚浮层、Git 拉取与拉取后重启、TopBar 的 Git 可用性检测提示
- **后端清理**：删除 `git.rs` 模块及 `lib.rs` 中全部 Git 命令注册；`commands.rs` 移除 Git 命令区块、`add_project` 中 `find_git_root`/`git_available` 探测、`add_service` 中 Git 归属逻辑（改为 `project_id = None`）、`delete_project` 中 `Pulling` 状态检查；`db/models.rs` 移除 `Project.git_available` 字段；`db/mod.rs` 同步 CRUD 与列投影；`pom/mod.rs` 删除 `find_git_root`；`error.rs` 删除 `AppError::Git` 变体
- **前端清理**：删除 `GitPanel`/`GitDiffModal`/`GitConflictModal`/`MonacoDiffEditor` 组件；`FilePanel.tsx` 移除全部 Git 集成（状态表、行级 diff、历史浮层、回滚、快速操作条）；`MonacoCodeEditor.tsx` 移除 diff glyph margin 装饰相关 props 与逻辑（`glyphMargin` 改为 false）；`Icons.tsx` 删除 GitPull/GitPush/GitPullRestart/GitBranch/History 图标；`TopBar.tsx` 移除 Git 不可用提示；`LogViewer.tsx` 移除 `sourceClass` 的 git 分支；`api.ts`/`types.ts`/`store.ts`/`App.tsx` 清理全部 Git API、类型与状态
- **样式清理**：`styles.css` 删除约 940 行 Git 相关样式（git-panel、文件树 Git 着色、glyph margin diff 装饰、git-quick-bar、git-hist 浮层、tree-lines、git-conflict 等）
- **数据库兼容**：`db/schema.rs` 保留 `git_available` 列定义（v1 migration 兼容旧库，列有 DEFAULT 0），model/CRUD 不再读写；`ServiceStatus::Pulling` enum 变体保留（序列化兼容）

### 优化

- **文件浏览器精简**：移除 Git 集成后，文件树、标签栏、编辑器工具栏不再渲染 Git 状态标记与行级 diff，界面更聚焦于文件浏览与编辑本身
- **README 更新**：移除 Git 工作区面板章节、文件浏览器 Git 着色/diff 标记条描述、项目结构中的 `GitPanel.tsx`/`git.rs`、FAQ 中 Git 相关问答

## [0.12.1] - 2026-08-28

### 修复

- **Monaco 编辑器首次加载失败（CSP 拦截 CDN）**：`@monaco-editor/react` 默认通过 `loader` 从 jsdelivr CDN 注入 monaco 脚本，被应用 CSP `script-src 'self' ...` 拦截，导致首次打开代码编辑器/Diff 弹窗时永远停留在加载状态；改为在 `monaco-setup.ts` 中调用 `loader.config({ monaco })` 注入本地已由 Vite 打包的 ESM monaco 实例，`init()` 检测到后直接 resolve 本地实例，完全跳过 CDN script 注入

## [0.12.0] - 2026-08-28

### 新增

- **Git Diff 变更导航**：Diff 弹窗工具栏新增上一处/下一处变更跳转按钮与计数显示（如 `1/5`），基于 Monaco DiffEditor `onDidUpdateDiff` 计算行级变更列表，`goToDiff` 循环跳转；diff 计算完成后自动定位至第一处变更
- **浏览器快捷键拦截**：拦截 Tauri WebView 中的原生快捷键行为，包括刷新（Ctrl/Cmd+R、Ctrl/Cmd+Shift+R、F5）、页面缩放（Ctrl+Plus/Minus/0）、开发者工具（F12、Ctrl+Shift+I）、Backspace 后退（非输入焦点时）；Ctrl+F 仍仅 preventDefault 以保留编辑器内置搜索

### 优化

- **快捷 diff 浮层**：阻止浮层点击事件冒泡避免误触关闭；浮层内文字可选（`user-select: text`）；尺寸与间距调整（宽度 380~720px、字号 13px、行高 22px），明暗主题下 diff 色块透明度与边框加深

### 修复

- **MonacoDiffEditor 语法错误**：修复 `useEffect` 注释行中混入字面量 `\n` 字符导致代码被压缩为单行、含非法字符的问题

## [0.11.0] - 2026-08-28

### 新增

- **Monaco 代码编辑器**：替换原 textarea+pre 双层叠加 + Prism 手写高亮方案，改用 Monaco Editor，内置语法高亮 / 行号 / 搜索 / 代码折叠 / minimap / 括号配对着色 / sticky scroll；新增 `MonacoCodeEditor` 组件，对接原 Git diff glyph margin 装饰与点击操作菜单
- **Monaco DiffEditor**：Git Diff 面板改用 Monaco DiffEditor 内置 diff 算法高亮差异，左侧原始版本只读、右侧修改后版本可编辑；新增 `MonacoDiffEditor` 组件
- **Monaco 主题配置**：新增 `monaco-setup.ts`，配置 Worker 入口并定义 `jb-light`/`jb-dark` 自定义主题，token 配色对齐原 Prism Xcode 风格；监听 `data-theme` 属性变化自动切换主题
- **git_diff_versions 命令**：新增后端命令返回 diff 两侧文件内容（HEAD 版本 + 工作区/暂存区版本），供 Monaco DiffEditor 渲染；未跟踪文件 original 为 None

### 变更

- **CSP 调整**：`script-src` 新增 `wasm-unsafe-eval`、新增 `worker-src 'self' blob:`，支持 Monaco Worker 运行
- **语言映射重构**：`getPrismLang` 改为 `getMonacoLang`，扩展名映射改为 Monaco 语言 ID（如 `markup` → `html`、`bash` → `shell`、`properties` → `ini`），未识别返回 `plaintext`
- **构建分包**：`vite.config.ts` 将 `monaco-editor` 与 `@monaco-editor/react` 独立为 monaco chunk，避免主包膨胀
- **GitConflictModal / GitDiffModal / GitPanel / UpdateModal**：适配 Monaco 编辑器，移除 Prism 相关引用

### 优化

- **移除 Prism 依赖**：删除 `prism-langs.ts` 与 `prismjs` / `@types/prismjs` 依赖，减少打包体积与维护成本

## [0.10.1] - 2026-08-27

### 修复

- **启动死锁（残留 handle）**：`start()` 检测到残留 handle 且 sysinfo 显示进程存活时，额外检查 runtime 状态；若为 `Error`/`Stopped`（用户主动请求启动说明认为服务未运行），清理残留 handle 与 PID 记录后允许重启，而非返回 `ServiceRunning` 拒绝启动
- **启动死锁（残留 placeholder）**：placeholder 残留同理，runtime 已标记 `Error`/`Stopped` 时视为上次启动失败的残留，清理而非拒绝；避免"状态=Error → 前端显示启动按钮但 `start()` 返回 ServiceRunning"的死锁

## [0.10.0] - 2026-08-27

### 新增

- **更新下载 URL 白名单**：`download_update` 校验下载地址 host 必须在白名单（github.com / objects.githubusercontent.com / 自有 CDN）且为 HTTPS，阻止非可信来源下载
- **安装包路径与类型校验**：`install_update` 校验安装包路径必须位于 `update_dir()` 内、扩展名必须为 `.exe`，阻止执行任意路径的可执行文件
- **Java 主版本检测与 argfile JDK8 回退**：命令行超长时先检测 Java 主版本号（解析 `java -version`），JDK 8 不支持 `@argfile`（JEP 294 为 JDK 9 引入）则自动回退到 CLASSPATH 环境变量方案；版本检测结果进程内缓存（64 条上限）避免批量启动反复探测
- **watcher worker 异常后自动重建**：文件监听 worker panic 退出后延迟 5 秒重建 watcher，避免永久失去自动重启能力；`unwatch` 时清除重启中标志，防止标志卡住阻塞后续 watch
- **自动重启 TOCTOU 竞态修复**：引入 `RESTART_IN_PROGRESS` 标志，原子地检查服务状态并设置标志，避免检查与 spawn 之间被并发事件触发重复编译；仅当服务配置了自动重启且正在运行时才触发
- **初始化失败错误提示**：`init()` 失败时在顶部展示 Alert 横幅并提供「重试」按钮，替代静默失败
- **错误信息归一化**：新增 `api.toErrMsg` 统一 TauriError / Error / string 的格式化输出，全前端错误提示改用此函数，消除 `catch (e: any)` 的 `any` 类型
- **walk_files 缓存**：Ctrl+P 文件列表遍历结果按 project_id 缓存（TTL 5 秒），避免每次打开弹层都全量遍历大型项目目录

### 变更

- **状态推送只 emit 变化服务**：CPU/内存采样与端口冲突刷新改为只推送有变化的服务快照，而非全量推送所有 runtimes，减少 IPC 噪声
- **端口列表整表替换**：`set_service_ports` 改为整表替换而非追加，避免端口变更后残留旧端口
- **mark_running 幂等**：已是 Running 状态时跳过 set_status + emit，避免日志中多次出现 "started" 关键字时反复推送
- **is_running 单锁判断**：一次性获取 handles 锁做判断，避免 handles 与 runtimes 两把锁的非原子竞态
- **日志缓冲构造新数组**：`flushLogs` 改为构造新数组而非原地 push，确保 zustand 引用相等判断生效；暂停服务也更新 lines 引用
- **已删除服务事件过滤**：`setRuntime` / `appendLog` / `flushLogs` 跳过已删除服务的事件，避免"复活"已删除服务

### 修复

- **路径穿越校验绕过**：`git::safe_join` 在目标文件尚不存在时 `canonicalize(full)` 返回 None 导致边界检查被跳过；改为 canonicalize parent 后拼文件名再校验，正确处理未创建文件
- **pom 模块路径越界**：`scan_recursive` 校验 module 路径不允许 `..` 或绝对路径，防止越界读取
- **read_file 超大文件内存耗尽**：`project_fs::read_file` 与 `git::read_file` 先检查文件大小再读入，超过上限直接拒绝，避免一次性读入超大文件
- **copy_recursive junction 循环**：递归复制增加深度上限（32 层），防御 Windows junction（file_type 不算 symlink）导致的无限递归
- **Mutex poison panic**：`update.rs` 下载取消状态锁改用 `safe_lock`，poison 时恢复而非 panic；`walk_files` 缓存锁同理
- **stop 后僵尸进程与句柄泄漏**：`build::wait_with_timeout` 超时强杀后 `child.wait()` 回收句柄；`terminal::kill` / `kill_all` 杀 shell 后 `wait()` 回收，避免僵尸句柄和 reader 线程长期阻塞
- **taskkill 失败静默**：`kill_process_tree_by_pid` 记录 taskkill 失败日志，便于排查"进程已退出"场景
- **emit 失败静默**：所有 `app.emit` 失败改为 `log::warn` 记录，便于排查 IPC 异常
- **save_run_pid 失败静默**：记录失败日志而非完全忽略
- **check_started 误匹配**：要求 "Started " 在行首附近（行长 ≤ 200）且包含完整三段关键字，减少日志中间出现 "Started xxx in xxx second" 的误匹配
- **theme localStorage 脏值**：严格校验 localStorage 值为 "dark"/"light" 之一，避免脏值导致主题异常
- **listen 泄漏**：App 初始化监听增加 `disposedRef`，防止 listen resolve 前 cleanup 已执行导致监听泄漏

### CI

- 无构建流水线变更

## [0.9.0] - 2026-08-27

### 新增

- **取消更新下载**：更新弹窗下载过程中支持取消，后端用 `CancellationToken` 中止下载循环、删除半成品文件；前端取消不弹错误提示，其他失败才提示。新增 `cancel_update` 命令与 `DownloadCancel` 全局状态（同一时刻只允许一个下载，新下载会取消旧令牌）
- **停止服务后等待进程真正退出**：`stop()` 发出 kill 后轮询 sysinfo 等待 PID 退出（超时 8 秒），解决 JVM shutdown hook 需 1~2 秒退出导致的后续 restart/recompile 撞上端口占用 / class 文件锁问题
- **服务操作按钮防抖**：启动 / 停止 / 重启 / 编译 / 重新编译期间禁用所有操作按钮，防止并发触发导致 handles map 上的 placeholder 被误判为堆叠残留而 kill 掉刚启动的进程
- **argfile 模式启动诊断**：java 启动时输出完整命令（`@path` 或 args 摘要 + cwd），启动失败时输出 argfile 路径与内容前 5 行预览，便于定位"退出码 1 且 stderr 为空"的启动失败
- **退出时清理文件监听**：应用关闭时先 `unwatch_all` 停止所有 watcher 并回收 worker 线程，避免退出竞态中 watcher 防抖 worker 触发 `trigger_restart`（其 `compile_and_start` 会先 stop 杀进程）导致 `stop_all_on_exit=false` 时服务被误杀
- **更新检查图标**：顶栏检查更新按钮改用带向上箭头的刷新弧线图标（Update），替代易误解的下载图标
- **重新编译并启动 Tooltip**：下拉菜单项增加说明 Tooltip，区分与「编译并启动」的行为差异（前者失败时服务停止，后者保留旧进程）

### 变更

- **噪声端口过滤后移到后端**：JMX/DevTools/H2 等噪声端口改由后端在 `runtime.ports` / `runtime.service_ports` 返回前统一过滤，前端删除重复的 `NOISE_PORTS` 列表，避免前后端漂移
- **启动流程统一走带依赖编排**：`handleStart` 统一调用 `startServiceWithDependencies`，后端根据实际依赖决定是否编排，避免前端预判依赖（`getServiceDependencies`）与实际启动之间的 TOCTOU 竞态，同时省掉一次 IPC 往返
- **终端退出只杀 shell 不杀进程树**：`kill_all` 改为只杀 shell 本身（`c.kill`），不杀进程树——用户可能在终端手动启动了服务，这些进程不应随应用退出被杀（服务生死由 `stop_all_on_exit` 配置控制）；shell 子进程会因 ConPTY 关闭失去终端但继续运行
- **设置图标改为通用齿轮**：原 brutalist slider-cluster 图标改为通用 gear 图标，语义更清晰

### 修复

- **argfile 路径含空格启动失败**：`std::process::Command::arg` 会把 `@C:\a b\args.txt` 自动转成 `"@C:\a b\args.txt"`（引号包整个参数），Java launcher 解析 `@argfile` 时不识别这种带引号形式导致找不到文件（退出码 1）。改用 `raw_arg` 直传，路径含空格时自行构造 `@"path"` 形式（引号紧跟 @ 之后，Java 支持）
- **JVM 启动早期失败诊断信息丢失**：退出码 1/2 时 stderr 可能尚未被 log reader 读完，启动失败后短暂等待 500ms 让 stderr 排空，避免诊断信息丢失
- **日志行 key 使用行索引**：LogViewer 虚拟列表行 key 从行内序号 `i` 改为绝对索引 `absIdx`，避免过滤/搜索时行 key 冲突导致 React 渲染异常

## [0.8.0] - 2026-08-27

### 新增

- **项目级 / 服务级环境变量注入**：支持在项目配置与服务配置中以 `KEY=VALUE` 形式自定义环境变量，启动服务时自动注入到子进程（mvn 编译 + java 运行）
- **环境变量优先级合并**：服务级同名变量覆盖项目级，项目级覆盖系统继承；允许覆盖 Launcher 内置的 JAVA_HOME / MAVEN_HOME / PATH / MAVEN_OPTS
- **环境变量编辑器 UI**：项目配置弹窗与服务配置弹窗新增 key-value 行内编辑器，支持增删行，复用 override_properties 的交互模式
- **数据库 v6 迁移**：projects 与 services 表新增 `env_vars TEXT` 列，存储格式与 `override_properties` 一致（JSON 数组 `[{key,value}]`）

## [0.7.0] - 2026-08-27

### 新增

- **IDEA 式变更快捷操作条**：编辑器 diff 标记行内联显示"还原/重做"快捷按钮，鼠标悬停时加宽并显示发光反馈
- **快捷操作条内联内容预览**：操作条内联展示改动前与当前内容的对比预览，辅助判断变更

### 修复

- 修复 diff 标记条点击无反应：输入层拦截了标记条的点击事件，导致交互完全失效
- 修复 diff 标记条仍被输入层拦截点击：强化标记条的 z-index 与事件穿透，确保点击可靠触发
- 修复光标输入错位：diff 标记条点击查看历史并回滚时，光标定位未同步更新

## [0.6.1] - 2026-08-27

### 修复

- 修复编辑器行级 git 标记未体现删除行：行级标记基于前端对「HEAD 内容 ↔ 编辑器内容」的 LCS diff 打标，被删除的行在缓冲区中无对应行，旧实现纯删除直接返回全部未变、混合差异块也只给增/改侧打标，导致文件内容里的删除在编辑器中完全不可见（Git 面板按 hunk 统计仍显示 `-N`，两处口径不一致）
- 行级标记新增「删除」类：差异块内有净删除时，在该块缓冲区末行的下一行绘制红色矮条（紧贴上一行与本行之间的间隙）；纯删除场景落在删除点后的第一行，文件尾删空则钳制到最后一行——语义与 VS Code 的 dirty-diff 装饰一致
- 实测多重复行 SQL 文件（+42/-3）此前删除零体现，修复后在删除位置正确标出；边界用例（中段纯删除 / 尾部整段删除 / 增删混块 / 清空内容）验证标记位置正确且不覆盖已有的修改 / 新增标记

## [0.6.0] - 2026-08-27

### 新增

- **标签栏右键菜单**：编辑器标签页支持右键操作——关闭 / 关闭其他 / 关闭右侧标签 / 全部关闭（多个脏标签合并为一次确认，不再逐个弹窗；激活优先级：当前 > 右键所在标签 > 最近相邻幸存者）、复制路径、重命名、在文件管理器中显示
- **项目级快速打开（Ctrl+P）**：全局唤起居中弹层，按文件名前缀 > 文件名包含 > 路径包含三级排序过滤项目内全部文件（两段式条目展示 文件名 + 目录路径），↑↓ 循环切换 / Enter 打开 / Esc 或点击遮罩关闭；后端新增 `walk_files` 命令递归扁平遍历全量文件（复用黑名单排除 node_modules/target 等，跳过符号链接，5 万条目 + 24 层深度上限防御 junction 循环），每次打开弹层后台重拉索引杜绝陈旧数据

### 优化

- 编辑器内搜索内容上限从 400KB 提升到 2MB，与后端可编辑/预览范围对齐——0.4~2MB 的文件此前打开搜索会静默显示"无匹配"，现可正常检索；同行命中可视列改为增量展开，消除 minified 单行大文件 O(n²) 的重复回走
- 超过 2MB 只读文件的搜索计数处明确提示「文件过大」（悬浮有说明），替代易误解的"无匹配"

## [0.5.1] - 2026-08-26

### 修复

- 修复更新下载网速显示与真实速率不匹配：原测速窗口由整数百分比变化驱动，时长不固定（快速链路仅数毫秒、慢速链路达数秒），EMA 对不等长窗口等权平均，叠加 CDN 突发送达导致显示值严重偏离真实吞吐；现改为固定 250ms 时间窗实测速度（窗口内新增字节 ÷ 实际耗时）+ 轻度 EMA 平滑（0.6/0.4），口径与系统任务管理器一致
- 网络停滞时速度约 1 秒内平滑回落到 0：流式读取加空闲超时采样，不再长时间停留在旧值
- 进度上报与测速统一按固定间隔节流（每秒 4 次），避免快速链路下事件刷爆 IPC 与前端渲染

## [0.5.0] - 2026-08-26

### 新增

- **编辑器行号栏**：编辑器与只读视图左缘显示行序号，宽度按总行数位数自适应（等宽字符画布实测），随垂直滚动同步；git diff 标记条自动右移避让（≤1 万行启用）
- **编辑器内搜索**：Ctrl+F 唤起右上角搜索面板——实时匹配计数、Enter / Shift+Enter 上下切换、Esc 关闭；命中处绘制半透明高亮块（当前命中加深），制表符按 tab-size 展开保证定位对齐
- **变更文件 ±行数标识**：文件树改动文件显示 `+新增 / -删除` 行数徽标（替代状态圆点），编辑器工具栏同步展示；git status 刷新后并发聚合 diff hunks，上限 300 文件

### 变更

- 禁用 webview 默认的全局查找（Ctrl+F / Cmd+F），由编辑器内置搜索替代

## [0.4.1] - 2026-08-26

### 修复

- 修复「在文件管理器中显示」打开资源管理器后不选中、不跳转的问题：repo root 来自 `git rev-parse --show-toplevel`（Git for Windows 输出正斜杠），拼出的混合分隔符路径导致 `explorer /select` 无法定位；现传给 explorer 前统一规范化为反斜杠绝对路径
- 修复编辑器行级 diff 标记与 Git 面板不一致（段落合并、行号漂移）的两个根因：
  - 中间区 LCS 由「前向填表 + 箭头回溯」替换为 suffix-DP + 正向贪心走位——旧算法在重复行密集的配置文件（yml 相同缩进键值对）中会选中劣质公共子序列，把分散的多处变更挤成一段连续块；新分块与 `git diff` hunk 完全一致（实测 application-dev.yml 三处变更由错误的 1 段恢复为正确的 3 段）
  - 标记行号推进遗漏：块分配循环中相同行未递增行号，多块场景下第二段起整体上移
- 修复外部修改后编辑器标记与 Git 面板不同步：在 IDE 等外部工具修改文件后切回，Git 面板读磁盘已刷新，但编辑器标签缓冲区仍是旧内容；现在窗口聚焦/面板可见刷新时对无未保存编辑的标签页从磁盘静默重读（保护 await 期间产生的新编辑与未保存草稿）
- 编辑器行级标记新增 git 语义抑制：被 `.gitignore` 忽略或被 `skip-worktree` / `assume-unchanged` 标记的文件不会出现在 `git status`，编辑器同样不再标线，两处口径完全一致

## [0.4.0] - 2026-08-26

### 新增

- **Git 推送**：Git 面板工具栏新增推送按钮，60 秒超时强杀进程树（防认证挂起），失败原因自动归类提示（非快进→引导先拉取、无上游、认证失败等）
- **冲突合并（IDEA 式）**：拉取产生冲突后自动弹出合并面板；本地修改 / 合并结果 / 远程修改三栏对比，基于共同祖先的行级 LCS 高亮；支持采用本地、采用远程、双侧合并快捷解决，中间结果可直接编辑后标记已解决；全部解决后一键完成合并提交，随时可中止合并恢复原状；面板顶部常驻冲突横幅入口
- **全功能终端**：集成终端由管道模式重写为 ConPTY 伪控制台（portable-pty），彩色输出、光标控制、交互式程序（python REPL / ssh / 密码输入）完整可用；前端 xterm.js 与 PTY 直连——点击终端直接打字，键盘数据原样透传（回显、行编辑、PSReadLine 历史、Ctrl+C 均由 shell 原生处理），移除独立输入行；窗口尺寸自适应行列数，明暗主题跟随
- **更新下载速度监控**：下载进度实时显示已下载/总大小与速度（EMA 平滑，500ms 刷新）
- **自动检测更新**：启动 3 秒后静默检查更新，此后每 4 小时复查；发现新版本时「检查更新」按钮显示小红点提示

### 变更

- 更新下载中底部不再显示冗余的进度百分比按钮，进度条 + 速度信息即可

## [0.3.0] - 2026-08-26

### 新增

- **集成终端**：文件面板底部抽屉内置 PowerShell 终端（优先 pwsh，回退 powershell），工作目录为项目根；输出事件流渲染 + 输入历史，子进程以 Job Object 托管、退出时回收整棵进程树
- **文件树与多标签编辑器**：懒加载 + 紧凑路径合并（`src/main/java` 单链折叠）；右键重命名 / 复制 / 剪切 / 粘贴 / 资源管理器定位、拖拽移动；多标签独立保留未保存内容，支持图片 / 二进制 / GBK 只读预览与 Markdown 预览切换
- **Git 工作台**：文件树与编辑器全面接入 Git 状态着色（新增/未跟踪=绿、修改=橙、删除=红、重命名=紫、冲突=红），目录聚合改动计数徽标；代码区左缘行级 diff 标记（绿=新增、橙=修改）

### 变更

- Git 面板 diff「工作区改动」口径改为工作区 vs HEAD（原为 vs 暂存区），与编辑器行级标记、文件树状态点三方一致，并忽略行尾 CR 抵消 `core.autocrlf` 换行差异
- 文件树与 Git 面板状态归类统一共用 `gitChangeKind`（新增 `renamed` / `conflict` 两类），消除重命名 / 冲突文件两处显示不一致
- 启动链路优化：Maven 离线优先（`-o` 失败自动回退在线）、冷启动单次 JVM 合并「编译 + classpath 解析」（约省 1.5~3.5s）、watcher 脏标记跳过全树 mtime 扫描、注入 `MAVEN_OPTS` 基线（`-Xmx1g -Dfile.encoding=UTF-8`）
- 新增「Spring Bean 懒加载」设置项（dev_mode 下注入 `lazy-initialization=true`，加速上下文启动）
- git 可执行文件解析与 repo root 解析进程级缓存，文件树 git 标记加载从 4+ 次进程 spawn 降为 1 次
- 应用视图切换重构：文件 / Git 面板常驻挂载仅切可见性，往返切换保留目录展开与打开的标签

### 修复

- 修复编辑器行级 diff 标记行位置错位：改由前端对「HEAD 内容 vs 编辑器缓冲区」做 LCS 行级 diff，标记位置与显示行精确对齐，不再受未保存编辑 / 外部修改影响；编辑器三层（底层 / 输入层 / 只读视图）行高统一为 22px，消除随行号线性漂移导致的长文件 diff 条与光标偏移
- 修复 Git 面板弹窗未跟踪文件不显示新增行的问题：渲染为「整文件新增」合成 diff，与文件树全绿标记一致
- 修复窗口失焦期间外部（IDE 等）改动 / 提交后，文件树状态点、行级标记与 Git 面板不刷新的问题：窗口重新聚焦自动刷新
- 修复切换服务时日志视图停留在上一服务滚动位置的问题（新旧服务行数相同时滚动 effect 不触发）

## [0.2.0] - 2026-08-25

### 新增

- **IDEA 开发环境适配**：JDK 探测新增 IDEA「Download JDK」落点 `~/.jdks`、JetBrains SDK 注册表 `jdk.table.xml`（支持 `$USER_HOME$` 宏展开，覆盖 PyCharm / Android Studio）、IDE 自带 JBR；Maven 探测新增 IDEA 捆绑 maven3 与 `~/.m2/wrapper/dists` 分发包
- **JAVA_HOME 启动日志**：服务启动时输出实际生效的 JDK 路径，便于排查 Maven 报「JAVA_HOME is not defined correctly」类问题

### 变更

- scoop 安装的 JDK / Maven 改为记录 `current` 稳定路径，环境升级后项目配置自动跟随、不再失效

### 修复

- 修复项目配置或系统 `JAVA_HOME` 失效（如 JDK 升级后旧目录被清理）导致一键启动全部报「JAVA_HOME is not defined correctly」的问题：注入前校验有效性并多级回退（项目配置 → 系统环境变量 → 从 PATH / scoop shims 的 java 反推真实 home），预检不再因 PATH 残留 java 带病放行

## [0.1.2] - 2026-08-25

### 新增

- **项目筛选**：侧边栏新增筛选开关，可仅显示存在运行中服务的项目（状态持久化，随服务启停实时刷新）
- **编辑态语法高亮**：文件编辑模式实时显示语法颜色（高亮底层 + 透明输入层叠加，滚动同步），与查看模式同一套配色
- **Git 面板返回文件树**：Git 工作区面板新增「返回文件树」按钮，与文件树双向切换

### 变更

- Git 拉取 / 拉取并重启快捷入口从项目「更多」菜单移除，「Git 工作区」入口集成到文件树头部（仅 Git 可用项目显示），Git 相关操作统一收敛至文件树模块

### 修复

- 修复「检查更新」请求失败（CORS）的问题：改用 Tauri http 插件经 Rust 侧发出请求，绕过 webview 跨域限制
- 修复 package-lock.json 与 package.json 不同步导致 CI 构建失败的问题

### CI

- 构建工作流改为仅 tag 推送 / 手动触发，master 推送不再自动构建
- 新增轻量校验工作流：推送时自动执行 npm ci（锁文件同步校验）、typecheck、lint

## [0.1.1] - 2026-08-25

### 新增

- **检查更新**：顶栏新增检查更新入口，弹窗展示 Markdown 格式更新日志，支持立即更新与取消（下载/安装后端逻辑待接入）
- **自定义标题栏**：移除系统原生标题栏，窗口控制（最小化/最大化还原/关闭）融入顶栏，支持拖拽移动窗口与双击最大化
- **侧边栏宽度调整**：服务列表侧边栏支持拖拽调整宽度（与文件树分隔条交互一致），宽度持久化到本地

### 修复

- 修复服务 Tab 栏与日志页右键菜单无法弹出的问题
- 修复服务卡片运行端口信息被遮挡的问题（放不下时自动换行显示）
- 修复日志 Tab 未读徽标渲染为方块伪影的问题

### 优化

- 顶栏视觉与整体风格统一，融合无割裂感
- 「停止全部」确认气泡配色与排版优化，与整体设计语言一致
- 「添加项目」弹框中目录选择按钮与文本框间距优化

## [0.1.0] - 2026-07-17

### 新增

- **服务管理**：Spring Boot 多项目/多服务扫描、启动、停止、重启、重新编译启动、清理编译产物
- **依赖编排**：服务依赖管理，按拓扑序启动，项目下服务一键批量启动
- **运行监控**：实时展示 PID、CPU、内存、监听端口，端口冲突检测与告警
- **自动重启**：服务级自动重启开关（带防抖）
- **实时日志**：日志面板支持虚拟滚动、INFO/WARN/ERROR 级别过滤、正则搜索高亮与跳转、暂停打印、清空、未读提示、Tab 多开
- **文件浏览**：项目文件树浏览，支持文本（语法高亮/编辑保存/编码检测）、图片预览、Markdown 渲染、二进制文件识别
- **Git 集成**：工作区状态面板、提交历史、Diff 查看、拉取、拉取并重启
- **环境配置**：项目级 JDK / Maven 配置，服务级 main_class、dev_mode、JVM 参数与属性覆盖
- **深色模式**：亮/暗主题一键切换，跟随系统配色

### 平台

- 基于 Tauri 2 + React + Zustand 构建，Windows x64 NSIS 安装器（免管理员权限）

[Unreleased]: https://github.com/fenggeg/java-boot/compare/v0.13.0...HEAD
[0.13.0]: https://github.com/fenggeg/java-boot/compare/v0.12.1...v0.13.0
[0.12.1]: https://github.com/fenggeg/java-boot/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/fenggeg/java-boot/compare/v0.1.2...v0.12.0
[0.1.2]: https://github.com/fenggeg/java-boot/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/fenggeg/java-boot/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/fenggeg/java-boot/releases/tag/v0.1.0
