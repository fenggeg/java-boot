# JavaBoot Launcher

**Windows 桌面端的 Spring Boot 多服务启动/管理器**，基于 Tauri 2 + React + Rust 实现。定位类似"轻量版 IDEA Services 面板"：从磁盘选择一个 Maven 聚合工程，一键识别、勾选、启动、停止、重编译多个 Spring Boot 服务，并集中查看日志、端口、CPU/内存；同时内置项目文件浏览器（浏览 / 编辑）、服务打包与一键更新。

> **仅支持 Spring Boot 项目。** 判定标准与 IDEA 一致：模块 `packaging` 为 `jar` / `war` **且** `src/main/java` 下存在带 `@SpringBootApplication` 注解的类。工具包 / 公共模块（无主类的 jar）不会出现在服务列表中。


---

## 功能特性

### 服务发现
- 递归解析 Maven 聚合 pom 的 `<modules>`，按 IDEA 风格识别 Spring Boot 应用
- 主类（`@SpringBootApplication` FQCN）自动探测（DB 缓存 → pom → 源码扫描），首次成功后持久化，应用启动时后台预热

### 启动 / 编译
- **三档构建策略**（基于源码 mtime + classpath 缓存 key）：
  - `Skip`：源码未变 & classpath 缓存命中 → 直接跳到 spawn
  - `CompileCurrent`：只编当前模块（`mvn -pl <module> compile`）
  - `CompileAll`：编当前模块 + 依赖模块（`-am`）
- **Classpath 持久化缓存**：首次通过 `mvn dependency:build-classpath` 解析后，写入 `.javaboot-cp.txt` + `.javaboot-cp.key`（pom + maven_home 内容哈希验证）
- **冷启动合并调用**：缓存 miss 时把「编译 + 解析 classpath」合并为单次 Maven JVM（`compile dependency:build-classpath -Dmdep.outputFile=${project.build.directory}/...`），失败自动降级两段式
- **离线优先**：所有 Maven 调用先带 `-o` 跳过远程元数据检查，仅当输出命中离线类错误特征（缺构件/元数据）才在线重试——弱网环境显著提速，且不会掩盖真实编译错误
- **watcher 脏标记**：自动重启服务的源码监听器同步维护模块脏标记，干净时跳过全树 mtime 扫描
- **Maven 参数优化**：`mvn compile`（替代 `install`）、`-T 1C` 并行、`-Dspring-boot.repackage.skip=true`、`-Dmaven.test.skip=true`、`--no-transfer-progress`
- **MAVEN_OPTS 注入**：`-Xmx1g -Dfile.encoding=UTF-8`（保留用户已有值），避免大项目编译期 GC 抖动
- **启动命令**：直接 `java -cp <classpath> <MainClass>`，绕开 `spring-boot:run` 的重打包开销；超长 classpath 自动切换 `@argfile`
- **dev_mode 快速启动**（可在服务配置里单独开关）：
  - `-XX:TieredStopAtLevel=1`（只做 C1 编译）
  - `-XX:+AlwaysPreTouch`
  - `-Dspring.jmx.enabled=false`
  - `-Dspring.output.ansi.enabled=never`
  - `-Dspring.devtools.restart.enabled=false`
  - 可选 `-Dspring.main.lazy-initialization=true`（设置里开启，显著缩短 Spring 上下文启动；依赖 `@PostConstruct` 时序的应用慎用）

### 运行时监控
- 端口占用扫描 + 冲突提示（同一 Launcher 内两个服务抢同一端口高亮），噪声端口（JMX / DevTools / H2）后端统一过滤
- CPU / 内存实时展示（sysinfo），状态推送只 emit 有变化的服务快照，减少 IPC 噪声
- 日志聚合：stdout/stderr 分流，`Started ... in ... seconds` 命中即置为 running，`APPLICATION FAILED TO START` 命中即置为 error
- 切换服务标签自动跳到日志最底部，新日志到达时跟随滚动（上滚即暂停跟随）
- 日志面板支持虚拟滚动、INFO/WARN/ERROR 级别过滤、正则搜索高亮跳转、暂停打印、清空、未读提示、Tab 多开

### 进程生命周期
- Windows Job Object 绑定子进程 → 主程序退出 / 崩溃时自动带走 java 进程，杜绝残留
- **独立 daemon 守护进程**（v0.16.0+）：Java 进程的 spawn / 管道消费 / 退出监控 / 就绪判定 / CPU 内存指标整体下沉到常驻 daemon（`src-tauri/daemon/`），launcher UI 崩溃或重启时 daemon 独立存活，托管服务不受影响；UI 重启后通过对账（`daemon_reconcile`）恢复实时事实与日志续传
- **崩溃恢复**：daemon 启动时扫描磁盘上仍存活的 java 进程，三态处置——接管监控 / 干净重启（原 spec，新 run_id，日志续传）/ 忽略；launcher 启动时 daemon 在线则重建服务映射，离线回退本地恢复
- **委托启动**：daemon 在线且命令行未超长时，java 进程整体交给 daemon 托管；超长 classpath（`@argfile` 模式）保持本地启动避免引号语义差异
- **顶栏守护状态**：在线绿点 / 离线灰点 + 托管运行数 + 合计内存，实时 CPU / 内存由 `daemon-proc-metrics` 事件推送，4 秒周期对账刷新进程列表
- 无锁取消令牌（`AtomicBool`），并发 `stop_all`
- `stop()` 发出 kill 后轮询 sysinfo 等待 PID 真正退出（超时 8 秒），避免 JVM shutdown hook 撞上端口占用 / class 文件锁
- 支持异常退出后重启（`auto_restart` 开关，按服务粒度；防抖 + `RESTART_IN_PROGRESS` 标志防并发重编译）
- 服务操作按钮防抖：启动 / 停止 / 重启 / 编译 / 重新编译期间禁用，避免并发触发误杀刚启动的进程

### 服务打包
- 服务卡片「更多」菜单新增**打包**：执行 `mvn clean package -DskipTests`（多模块加 `-pl <module> -am`），保留 spring-boot repackage 生成可执行 fat jar
- 打包时不停止运行中的服务（本项目用 exploded classpath `java -cp target/classes:...` 启动，JVM 不锁定 fat jar，与 IDEA 行为一致）
- 项目行「更多」菜单支持**打包全部服务**（串行避免资源争抢），完成后弹窗列出每个服务的 jar 产物路径
- 打包产物智能识别（排除 `*-sources.jar` / `*-javadoc.jar` / `original-*`，取最大文件），弹窗提供「打开目录」（资源管理器定位 jar）和「复制路径」
- 服务卡片常驻**打开 target 目录**入口

### 项目文件浏览器
- **文件树**：懒加载 + 紧凑路径合并（`src/main/java` 单链目录折叠显示）
- **快速打开（Ctrl+P）**：项目级文件名 / 路径过滤跳转面板，按文件名前缀 > 文件名包含 > 路径包含排序，↑↓ 选择、回车打开、Esc 关闭；后台 `walk_files` 拉取全量扁平文件列表（5 万条目 + 24 层深度上限防御 junction 循环），结果按 project_id 缓存（TTL 5 秒）
- **Monaco 多标签编辑器**：替换原 Prism 手写高亮方案，内置语法高亮 / 行号 / 搜索（Ctrl+F）/ 代码折叠 / minimap / 括号配对着色 / sticky scroll；每个标签独立保存未保存内容（切换不丢编辑），脏标记圆点提示；切换标签时按 path 精确隔离滚动位置与 view state
- **查看模式**：Markdown 支持预览 ↔ 编辑切换；其他文本文件直接在单页内编辑；GBK / 超大文件（>2MB）只读保护；图片预览、二进制文件识别
- **右键菜单**：重命名、复制、粘贴、剪切、在文件管理器中显示（explorer 定位高亮）；支持目录间拖拽移动；同名粘贴自动生成 `(2)` 序号；标签栏右键支持关闭 / 关闭其他 / 关闭右侧 / 全部关闭、复制路径、重命名
- **快捷键**：文件树 Ctrl+C / Ctrl+V 复制粘贴选中条目；编辑器内 Ctrl+F 搜索（上限 2MB，与可编辑范围对齐）

### Git 集成（文件编辑器，只读）
- **Gutter 变更标记**：行号旁绿（新增）/ 黄（修改）/ 红（删除）标记，含 minimap 与 overview ruler 着色；`git diff HEAD -U0` 解析 hunk（纯删除画在 `newStart+1` 行）；打字不重算 diff（decoration 锚点 stickiness 自动跟随），仅文件加载与 `git://changed` 事件时刷新
- **Diff 对比面板**：工具栏 Diff 按钮打开并排独立面板，Monaco DiffEditor 对比 HEAD 版本（`git cat-file`）与当前缓冲区（实时跟随输入）
- **文件树状态标记**：`git status --porcelain=v1 -z` 解析，未跟踪 / 新增 / 修改 / 删除 / 重命名以彩色圆点标注
- **Blame 悬浮**：hover 行号显示提交摘要、作者、时间（`git blame --porcelain` 懒加载）
- **删除代码内联查看**：点击删除标记，view zone 内联展示被删除的原始代码
- **只读 & 安全**：全部 git 调用在 Rust 后端完成（`git_cli` 执行层 + `git_watcher` 监听），前端不直接执行 shell 命令；repoRoot canonicalize + filePath strip_prefix 校验 + 路径参数一律置于 `--` 分隔符之后，杜绝注入；`--no-optional-locks` 防止 status 写入 index 触发监听死循环；git 子进程并发上限 2；git 未安装显示轻量提示条、非 git 目录静默隐藏

### 环境与依赖探测
- **JDK 自动探测**：覆盖系统 `JAVA_HOME` / PATH / scoop shims（记录 `current` 稳定路径，升级后自动跟随）/ IDEA「Download JDK」落点 `~/.jdks` / JetBrains SDK 注册表 `jdk.table.xml`（支持 `$USER_HOME$` 宏）/ IDE 自带 JBR；探测结果进程内缓存（RwLock，读锁快速路径 + 写锁双重检查）
- **Maven 自动探测**：系统 `MAVEN_HOME` / PATH / scoop / IDEA 捆绑 maven3 / `~/.m2/wrapper/dists` 分发包
- **JAVA_HOME 失效回退**：项目配置或系统 `JAVA_HOME` 失效（如升级后旧目录被清理）时，多级回退（项目配置 → 系统环境变量 → 从 PATH / scoop shims 的 java 反推真实 home），预检不再因 PATH 残留 java 带病放行
- **环境变量注入**：项目级 / 服务级 `KEY=VALUE` 环境变量，启动时注入子进程（mvn 编译 + java 运行）；优先级：服务级 > 项目级 > 系统继承，允许覆盖 Launcher 内置的 `JAVA_HOME` / `MAVEN_HOME` / `PATH` / `MAVEN_OPTS`

### 应用自更新
- 顶栏「检查更新」入口，启动 3 秒后静默检查，此后每 4 小时复查；发现新版本按钮显示小红点
- 更新弹窗展示 Markdown 格式更新日志，支持下载 / 安装 / 取消（下载中可取消，`CancellationToken` 中止并删除半成品）
- 下载进度实时显示已下载 / 总大小与速度（固定 250ms 时间窗实测 + EMA 平滑，口径与任务管理器一致）
- **安全校验**：下载 URL host 必须在白名单（github.com / objects.githubusercontent.com / 自有 CDN）且 HTTPS；安装包路径必须位于 `update_dir()` 内、扩展名必须为 `.exe`；前端 `isGithubRelease` 运行时类型守卫校验响应字段

### 其他
- 多项目并存，每个项目可单独绑定 `JAVA_HOME` / `MAVEN_HOME`（覆盖系统 PATH）
- 全局配置：端口刷新间隔、日志缓冲行数、编译失败是否停旧进程、dev_mode 懒加载开关、退出时是否停掉全部子进程等
- **ErrorBoundary 自动恢复**：渲染错误时提供「尝试恢复」按钮重置组件状态（不刷新页面），最多自动恢复 2 次，超过后仅保留「重新加载」
- **浏览器快捷键拦截**：拦截 Tauri WebView 原生刷新（Ctrl+R / F5）、缩放（Ctrl+±/0）、开发者工具（F12）、Backspace 后退；Ctrl+F 交由编辑器内置搜索
- **深色模式**：亮 / 暗主题一键切换，Monaco 主题（`jb-light` / `jb-dark`）跟随

---

## 环境要求

**运行环境**
- Windows 10 / 11（Job Object、`taskkill /T /F` 均为 Windows 专用路径）
- JDK 8+（服务端所需版本自便，Launcher 只调用 `java.exe`）
- Maven 3.6+
- PowerShell 5.1+（系统自带；装有 PowerShell 7 (`pwsh`) 时优先使用）

**开发/打包**
- Node.js 18+
- Rust stable（`rustup target add x86_64-pc-windows-msvc`）
- VS Build Tools（Tauri Windows 打包依赖）

---

## 快速开始（开发）

```powershell
# 1. 装依赖
npm install

# 2. 开发模式（前端 + Tauri 一起起）
npm run tauri:dev

# 3. 打包 Windows 安装包（nsis）
npm run tauri:build
```

首次启动后：
1. **项目 → 添加项目**，选中根 `pom.xml` 或项目根目录
2. Launcher 扫描后弹出模块树，勾选要托管的 Spring Boot 服务
3. 每个服务可单独配置：`maven_opts`、`spring.profiles.active`、`dev_mode`、`auto_restart`、覆盖属性、环境变量、依赖
4. 服务卡片上的文件夹图标打开**文件面板**（Ctrl+P 快速打开文件，Ctrl+F 编辑器内搜索）；卡片「更多」菜单可**打包**或**打开 target 目录**
5. 顶栏「检查更新」可拉取最新版本

---

## 项目结构

```
├── src/                       # React 前端
│   ├── components/
│   │   ├── FilePanel.tsx      # 文件树 + 多标签编辑器 + 快速打开编排
│   │   ├── FileTreeRow.tsx    # 递归树行组件 + FileTypeIcon + 拖拽合法性 / drop 高亮
│   │   ├── QuickOpenPanel.tsx # Ctrl+P 快速打开面板
│   │   ├── MonacoCodeEditor.tsx # Monaco 代码编辑器（语法高亮 / 搜索 / 折叠 / minimap）
│   │   ├── LogViewer.tsx      # 日志虚拟列表 + 级别过滤 + 正则搜索
│   │   ├── ServiceCard.tsx    # 服务卡片（启停 / 编译 / 打包 / 更多菜单）
│   │   ├── ServiceList.tsx    # 服务列表 + 依赖编排
│   │   ├── ProjectConfigModal.tsx  # 项目配置（JAVA_HOME / MAVEN_HOME / 环境变量）
│   │   ├── ServiceConfigModal.tsx  # 服务配置（main_class / dev_mode / 覆盖属性 / 环境变量 / 依赖）
│   │   ├── SettingsDrawer.tsx # 全局设置抽屉
│   │   ├── UpdateModal.tsx    # 应用更新弹窗
│   │   ├── ErrorBoundary.tsx  # 渲染错误自动恢复
│   │   └── ...                # AddProject / AddService / TopBar / Icons
│   ├── utils/
│   │   └── envVars.ts         # 环境变量 / 覆盖属性公共解析与序列化
│   ├── api.ts                 # Tauri invoke 封装 + toErrMsg 错误归一化
│   ├── update.ts              # 更新检查 + isGithubRelease 类型守卫
│   ├── monaco-setup.ts        # Monaco Worker 入口（editor/json/css/html/ts）+ jb-light/jb-dark 主题
│   ├── languages.ts           # 扩展名 → Monaco 语言 ID 映射
│   ├── features/
│   │   └── git/               # Git 集成（api 封装 / useGitGutter / DiffView 面板）
│   ├── store.ts               # zustand（含 HMR 清理）
│   ├── theme.ts               # 亮 / 暗主题
│   └── types.ts
├── src-tauri/
│   ├── src/
│   │   ├── commands.rs        # Tauri 命令入口（project / service / process / files / config / git / util）
│   │   ├── git_cli.rs         # Git CLI 执行层（GitRunner + 固定前缀 + `--` 分隔）+ 纯函数解析器（hunk / status-z / blame / 路径校验）+ 单元测试
│   │   ├── git_watcher.rs     # git 目录监听（notify-debouncer-full + hash 去重防死循环 + git://changed 推送）
│   │   ├── db/                # SQLite (rusqlite + r2d2 连接池) - schema/models/mod，PRAGMA user_version 幂等迁移
│   │   ├── pom/               # Maven pom 解析 + SpringBoot 识别（module 路径越界校验）
│   │   ├── process/
│   │   │   ├── manager.rs     # 进程管理器（start/stop/stop_all、三档策略编排、restore_running、refresh_resource_usage、daemon 委托分流）
│   │   │   ├── delegate.rs    # daemon 委托启动 / 事件归一（P4：spawn/stop/rebind/normalize_event）
│   │   │   ├── build.rs       # classpath 缓存、mtime 决策、Maven 执行器（离线优先）、Java 主版本检测 + argfile JDK8 回退
│   │   │   ├── env.rs         # JAVA_HOME / MAVEN_OPTS 解析注入（windows crate 注册表 PATH 合并）
│   │   │   ├── log_pipe.rs    # 启动/失败模式匹配
│   │   │   └── job.rs         # Windows Job Object 封装
│   │   ├── ipc.rs             # daemon IPC 客户端（TCP 连接 / 握手 / 请求响应 / 事件转发）
│   │   ├── port/              # 端口占用扫描（噪声端口过滤）
│   │   ├── project_fs.rs      # 项目文件浏览/读写/重命名/复制/移动/资源管理器定位/walk_files
│   │   ├── watcher.rs         # 文件变更监听（notify）+ 模块脏标记 + worker panic 重建（指数退避）
│   │   ├── update.rs          # 应用自更新（下载白名单 / 安装包校验 / 取消令牌）
│   │   └── util.rs            # 通用工具（junction 解析、输出解码等）
│   ├── daemon/                # 独立 daemon 二进制 crate（进程托管 / 崩溃恢复 / 扫描 / 监控指标采集）
│   ├── shared/                # jb-core 共享 crate（协议 method/HelloResult、模型 ProcessInfo/SpawnRequest/RecoveryEntry）
│   └── Cargo.toml
└── package.json
```

---

## 常见问题

**Q：为什么我的公共 jar 模块不在服务列表里？**
A：只有 `src/main/java` 下存在 `@SpringBootApplication` 的模块才被判为可启动服务。这与 IDEA 判定 Spring Boot 应用的行为一致。工具包 / 公共 jar 会作为依赖被自动编入 classpath，无需单独出现在列表中。

**Q：启动时看到 `构建策略: Skip（classpath cache: hit）`，改了代码没重编译？**
A：`Skip` 只在**源码确认无变更**时触发（自动重启服务的 watcher 实时报告，其余走 mtime 扫描）。改任何 `.java` / `.xml` / `resources/**` 都会自动升到 `CompileCurrent`；对被依赖的模块也做了变更时会升到 `CompileAll`。

**Q：打包会停掉运行中的服务吗？**
A：不会。本项目用 exploded classpath `java -cp target/classes:...` 启动，JVM 不锁定 fat jar，与 IDEA 行为一致。打包成功 / 失败后均恢复打包前状态，运行中的服务不会被误标为 Stopped / Error。

**Q：能不能支持非 Spring Boot 项目 / 主类由用户手填？**
A：当前不支持。所有启动前置逻辑（日志匹配 `Started .* in .* seconds`、`APPLICATION FAILED TO START`；`spring.profiles.active`；classpath 组装）都是围绕 Spring Boot 设计的。如需支持普通 Java main 服务，会作为后续任务另行评估。

**Q：跨平台？**
A：当前只测过 Windows。macOS / Linux 上 Job Object、`taskkill` 路径需要另写实现，暂未适配。

---

## 技术栈

| 层 | 组件 |
|---|---|
| 前端 | React 18 + antd 5 + zustand + Vite 5 + TypeScript + Monaco Editor（语法高亮 / 搜索 / 折叠 / minimap）+ react-markdown |
| 桥接 | Tauri 2 (IPC / 事件) |
| 后端 | Rust (tokio, rusqlite + r2d2, sysinfo, notify + notify-debouncer-full, parking_lot, once_cell, quick-xml, reqwest, encoding_rs, windows crate) |
| Git 集成 | git CLI（只读：status / diff / cat-file / blame），Rust 端统一执行 + 路径校验 + `--` 分隔防注入 |
| 存储 | SQLite（用户配置目录），`PRAGMA user_version` 幂等迁移 |
| 平台 | Windows（Job Object + CREATE_NEW_PROCESS_GROUP） |

---

## 许可

本项目基于 [MIT License](./LICENSE) 开源。
