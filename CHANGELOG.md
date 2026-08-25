# Changelog

本项目所有显著变更将记录在此文件中。

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本 2.0.0](https://semver.org/lang/zh-CN/)。

## [Unreleased]

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

[Unreleased]: https://github.com/fenggeg/java-boot/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/fenggeg/java-boot/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/fenggeg/java-boot/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/fenggeg/java-boot/releases/tag/v0.1.0
