# JavaBoot Launcher

**Windows 桌面端的 Spring Boot 多服务启动/管理器**，基于 Tauri 2 + React + Rust 实现。定位类似"轻量版 IDEA Services 面板"：从磁盘选择一个 Maven 聚合工程，一键识别、勾选、启动、停止、重编译多个 Spring Boot 服务，并集中查看日志、端口、CPU/内存。

> ⚠️ **仅支持 Spring Boot 项目。** 判定标准与 IDEA 一致：模块 `packaging` 为 `jar` / `war` **且** `src/main/java` 下存在带 `@SpringBootApplication` 注解的类。工具包 / 公共模块（无主类的 jar）不会出现在服务列表中。

---

## 功能特性

### 服务发现
- 递归解析 Maven 聚合 pom 的 `<modules>`，按 IDEA 风格识别 Spring Boot 应用
- 主类（`@SpringBootApplication` FQCN）自动探测，首次成功启动后持久化到本地数据库，后续免扫

### 启动 / 编译
- **三档构建策略**（基于源码 mtime + classpath 缓存 key）：
  - `Skip`：源码未变 & classpath 缓存命中 → 直接跳到 spawn
  - `CompileCurrent`：只编当前模块（`mvn -pl <module> compile`）
  - `CompileAll`：编当前模块 + 依赖模块（`-am`）
- **Classpath 持久化缓存**：首次通过 `mvn dependency:build-classpath` 解析后，写入 `.javaboot-cp.txt` + `.javaboot-cp.key`（pom + maven_home 内容哈希验证）
- **Maven 参数优化**：`mvn compile`（替代 `install`）、`-T 1C`、`-Dspring-boot.repackage.skip=true`、`-Dmaven.test.skip=true`、`--no-transfer-progress`
- **启动命令**：直接 `java -cp <classpath> <MainClass>`，绕开 `spring-boot:run` 的重打包开销
- **dev_mode 快速启动**（可在服务配置里单独开关）：
  - `-XX:TieredStopAtLevel=1`（只做 C1 编译）
  - `-XX:+AlwaysPreTouch`
  - `-Dspring.jmx.enabled=false`
  - `-Dspring.output.ansi.enabled=never`
  - `-Dspring.devtools.restart.enabled=false`

### 运行时监控
- 端口占用扫描 + 冲突提示（同一 Launcher 内两个服务抢同一端口高亮）
- CPU / 内存实时展示（sysinfo）
- 日志聚合：stdout/stderr 分流，`Started ... in ... seconds` 命中即置为 running，`APPLICATION FAILED TO START` 命中即置为 error

### 进程生命周期
- Windows Job Object 绑定子进程 → 主程序退出 / 崩溃时自动带走 java 进程，杜绝残留
- 无锁取消令牌（`AtomicBool`），并发 `stop_all`
- 支持异常退出后重启（`auto_restart` 开关，按服务粒度）
- 应用重启后能识别磁盘上仍存活的 java 进程，恢复其运行状态

### 其他
- 多项目并存，每个项目可单独绑定 `JAVA_HOME` / `MAVEN_HOME`（覆盖系统 PATH）
- Git 拉取 + 拉取后重启（`git pull && restart`）
- 全局配置：端口刷新间隔、日志缓冲行数、退出时是否停掉全部子进程等

---

## 环境要求

**运行环境**
- Windows 10 / 11（Job Object、`taskkill /T /F` 均为 Windows 专用路径）
- JDK 8+（服务端所需版本自便，Launcher 只调用 `java.exe`）
- Maven 3.6+

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

# 3. 打包 Windows 安装包（msi + nsis）
npm run tauri:build
```

首次启动后：
1. **项目 → 添加项目**，选中根 `pom.xml` 或项目根目录
2. Launcher 扫描后弹出模块树，勾选要托管的 Spring Boot 服务
3. 每个服务可单独配置：`maven_opts`、`spring.profiles.active`、`dev_mode`、`auto_restart`

---

## 项目结构

```
├── src/                       # React 前端
│   ├── components/            # 项目 / 服务 / 日志等 UI 组件
│   ├── api.ts                 # Tauri invoke 封装
│   ├── store.ts               # zustand
│   └── types.ts
├── src-tauri/
│   ├── src/
│   │   ├── commands.rs        # Tauri 命令入口
│   │   ├── db/                # SQLite (rusqlite bundled) - schema/models/mod
│   │   ├── pom/               # Maven pom 解析 + SpringBoot 识别
│   │   ├── process/
│   │   │   ├── manager.rs     # 进程管理器（薄壳）
│   │   │   ├── build.rs       # classpath 缓存、mtime 决策、主类探测
│   │   │   ├── env.rs         # JAVA_HOME / MAVEN_HOME 解析（OnceLock 缓存）
│   │   │   ├── log_pipe.rs    # 启动/失败模式匹配
│   │   │   └── job.rs         # Windows Job Object 封装
│   │   ├── port/              # 端口占用扫描
│   │   ├── git/               # git pull
│   │   └── config.rs
│   └── Cargo.toml
└── package.json
```

---

## 常见问题

**Q：为什么我的公共 jar 模块不在服务列表里？**
A：从 v0.1（当前）起，只有 `src/main/java` 下存在 `@SpringBootApplication` 的模块才被判为可启动服务。这与 IDEA 判定 Spring Boot 应用的行为一致。工具包 / 公共 jar 会作为依赖被自动编入 classpath，无需单独出现在列表中。

**Q：启动时看到 `构建策略: Skip（classpath cache: hit）`，改了代码没重编译？**
A：`Skip` 只在**源码 mtime 与上次一致**时触发。改任何 `.java` / `.xml` / `resources/**` 都会自动升到 `CompileCurrent`；对被依赖的模块也做了变更时会升到 `CompileAll`。若你确认改了代码但仍是 Skip，删掉模块下 `.javaboot-cp.key` 强制重解析。

**Q：能不能支持非 Spring Boot 项目 / 主类由用户手填？**
A：当前不支持。所有启动前置逻辑（日志匹配 `Started .* in .* seconds`、`APPLICATION FAILED TO START`；`spring.profiles.active`；classpath 组装）都是围绕 Spring Boot 设计的。如需支持普通 Java main 服务，会作为后续任务另行评估。

**Q：跨平台？**
A：当前只测过 Windows。macOS / Linux 上 Job Object、`taskkill` 路径需要另写实现，暂未适配。

---

## 技术栈

| 层 | 组件 |
|---|---|
| 前端 | React 18 + antd 5 + zustand + Vite 5 + TypeScript |
| 桥接 | Tauri 2 (IPC / 事件) |
| 后端 | Rust (tokio, rusqlite bundled, sysinfo, parking_lot, once_cell, quick-xml, futures) |
| 存储 | SQLite（用户配置目录），schema 幂等迁移 |
| 平台 | Windows（Job Object + CREATE_NEW_PROCESS_GROUP） |

---

## 许可

本项目基于 [MIT License](./LICENSE) 开源。
