# JavaBoot Launcher

**Windows 桌面端的 Spring Boot 多服务启动/管理器**，基于 Tauri 2 + React + Rust 实现。定位类似"轻量版 IDEA Services 面板"：从磁盘选择一个 Maven 聚合工程，一键识别、勾选、启动、停止、重编译多个 Spring Boot 服务，并集中查看日志、端口、CPU/内存；同时内置项目文件浏览器（浏览 / 编辑 / Git 状态标记）与 PowerShell 终端。

> ⚠️ **仅支持 Spring Boot 项目。** 判定标准与 IDEA 一致：模块 `packaging` 为 `jar` / `war` **且** `src/main/java` 下存在带 `@SpringBootApplication` 注解的类。工具包 / 公共模块（无主类的 jar）不会出现在服务列表中。

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
- 端口占用扫描 + 冲突提示（同一 Launcher 内两个服务抢同一端口高亮）
- CPU / 内存实时展示（sysinfo）
- 日志聚合：stdout/stderr 分流，`Started ... in ... seconds` 命中即置为 running，`APPLICATION FAILED TO START` 命中即置为 error
- 切换服务标签自动跳到日志最底部，新日志到达时跟随滚动（上滚即暂停跟随）

### 进程生命周期
- Windows Job Object 绑定子进程 → 主程序退出 / 崩溃时自动带走 java 进程，杜绝残留
- 无锁取消令牌（`AtomicBool`），并发 `stop_all`
- 支持异常退出后重启（`auto_restart` 开关，按服务粒度；防抖 + 状态机防并发重编译）
- 应用重启后能识别磁盘上仍存活的 java 进程，恢复其运行状态

### Git 工作区面板
- 工作区状态：分支 / 领先落后 / 改动文件列表（暂存区与工作区分栏）
- 单文件 diff 查看、暂存 / 取消暂存、提交、提交历史 + 任意提交完整 diff
- Git 拉取 + 拉取后重启（`git pull && restart`）

### 项目文件浏览器
- **文件树**：懒加载 + 紧凑路径合并（`src/main/java` 单链目录折叠显示）；与 Git 面板互切时完整保留展开状态与打开的文件
- **多标签页编辑器**：每个标签独立保存未保存内容（切换不丢编辑），脏标记圆点提示
- **查看模式**：Markdown 支持预览 ↔ 编辑切换；其他文本文件直接在单页内编辑（Prism 实时语法高亮）；GBK / 超大文件只读保护；图片预览、二进制文件识别
- **右键菜单**：重命名、复制、粘贴、剪切、在文件管理器中显示（explorer 定位高亮）；支持目录间拖拽移动；同名粘贴自动生成 `(2)` 序号
- **Git 状态着色**：新增/未跟踪=绿、已修改=橙、已删除=红——覆盖文件树（含目录聚合改动计数徽标）、文件标签页、编辑器工具栏徽标
- **行级 diff 标记条**：代码区左缘按行标注修改（橙）/ 新增（绿），直接采用 `git diff HEAD --unified=0 --ignore-cr-at-eol` 结果，与 Git 面板完全同源（兼容 `core.autocrlf` 的 CRLF/LF 差异）；保存 / 切回面板自动刷新

### 集成终端
- 文件面板底部抽屉式 PowerShell 终端（优先 `pwsh.exe`，回退系统自带 `powershell.exe`，`-NoLogo -ExecutionPolicy Bypass`），工作目录为项目根
- 输入历史（↑/↓）、本地命令回显、清屏 / 重启 / 关闭（终止整棵进程树）
- 会话随面板切换保活，输出缓冲持久；应用退出统一回收

### 其他
- 多项目并存，每个项目可单独绑定 `JAVA_HOME` / `MAVEN_HOME`（覆盖系统 PATH）
- 全局配置：端口刷新间隔、日志缓冲行数、编译失败是否停旧进程、dev_mode 懒加载开关、退出时是否停掉全部子进程等

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
3. 每个服务可单独配置：`maven_opts`、`spring.profiles.active`、`dev_mode`、`auto_restart`
4. 服务卡片上的文件夹图标打开**文件面板**；底部「终端」抽屉可在项目根目录执行命令

---

## 项目结构

```
├── src/                       # React 前端
│   ├── components/
│   │   ├── FilePanel.tsx      # 文件树 + 多标签编辑器 + Git 标记 + 终端抽屉
│   │   ├── TerminalView.tsx   # 集成 PowerShell 终端（事件流 + 输入历史）
│   │   ├── GitPanel.tsx       # Git 工作区（status/diff/stage/commit/log）
│   │   └── ...                # 项目 / 服务 / 日志等 UI 组件
│   ├── api.ts                 # Tauri invoke 封装
│   ├── store.ts               # zustand
│   └── types.ts
├── src-tauri/
│   ├── src/
│   │   ├── commands.rs        # Tauri 命令入口
│   │   ├── db/                # SQLite (rusqlite bundled) - schema/models/mod
│   │   ├── pom/               # Maven pom 解析 + SpringBoot 识别
│   │   ├── process/
│   │   │   ├── manager.rs     # 进程管理器（start/stop/stop_all、三档策略编排）
│   │   │   ├── build.rs       # classpath 缓存、mtime 决策、Maven 执行器（离线优先）
│   │   │   ├── env.rs         # JAVA_HOME / MAVEN_OPTS 解析注入（注册表 PATH 合并）
│   │   │   ├── log_pipe.rs    # 启动/失败模式匹配
│   │   │   └── job.rs         # Windows Job Object 封装
│   │   ├── port/              # 端口占用扫描
│   │   ├── git.rs             # git pull/status/diff/stage/commit/log + 行级 diff hunk
│   │   ├── project_fs.rs      # 项目文件浏览/读写/重命名/复制/移动/资源管理器定位
│   │   ├── terminal.rs        # 集成终端会话（PowerShell + Job Object 托管）
│   │   ├── watcher.rs         # 文件变更监听（notify）+ 模块脏标记
│   │   ├── update.rs          # 应用自更新
│   │   └── util.rs            # 通用工具（junction 解析、输出解码等）
│   └── Cargo.toml
└── package.json
```

---

## 常见问题

**Q：为什么我的公共 jar 模块不在服务列表里？**
A：从 v0.1（当前）起，只有 `src/main/java` 下存在 `@SpringBootApplication` 的模块才被判为可启动服务。这与 IDEA 判定 Spring Boot 应用的行为一致。工具包 / 公共 jar 会作为依赖被自动编入 classpath，无需单独出现在列表中。

**Q：启动时看到 `构建策略: Skip（classpath cache: hit）`，改了代码没重编译？**
A：`Skip` 只在**源码确认无变更**时触发（自动重启服务的 watcher 实时报告，其余走 mtime 扫描）。改任何 `.java` / `.xml` / `resources/**` 都会自动升到 `CompileCurrent`；对被依赖的模块也做了变更时会升到 `CompileAll`。

**Q：编辑器里的行级颜色标记和 Git 面板不一致？**
A：两者使用同一个 `git diff` 引擎（工作区 vs HEAD，`--ignore-cr-at-eol` 抵消 autocrlf 的换行差异）。若仍不一致，切回文件面板会自动刷新；保存后立即刷新。

**Q：集成终端里能跑交互式程序吗？**
A：终端为管道模式（未接 ConPTY），常规 mvn / git / dir 没问题；需要密码输入或全屏 TUI 的程序（如 `vim`、交互式登录）不受支持，请用外部终端。「关闭」按钮会终止 shell 及其派生的整棵进程树。

**Q：能不能支持非 Spring Boot 项目 / 主类由用户手填？**
A：当前不支持。所有启动前置逻辑（日志匹配 `Started .* in .* seconds`、`APPLICATION FAILED TO START`；`spring.profiles.active`；classpath 组装）都是围绕 Spring Boot 设计的。如需支持普通 Java main 服务，会作为后续任务另行评估。

**Q：跨平台？**
A：当前只测过 Windows。macOS / Linux 上 Job Object、`taskkill` 路径需要另写实现，暂未适配。

---

## 技术栈

| 层 | 组件 |
|---|---|
| 前端 | React 18 + antd 5 + zustand + Vite 5 + TypeScript + Prism（语法高亮） |
| 桥接 | Tauri 2 (IPC / 事件) |
| 后端 | Rust (tokio, rusqlite bundled, sysinfo, notify, parking_lot, once_cell, quick-xml) |
| 存储 | SQLite（用户配置目录），schema 幂等迁移 |
| 平台 | Windows（Job Object + CREATE_NEW_PROCESS_GROUP） |

---

## 许可

本项目基于 [MIT License](./LICENSE) 开源。
