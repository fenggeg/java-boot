---
name: "version-release"
description: "执行 javaboot-launcher 的版本发布全流程（同步版本号→CHANGELOG→本地预检→提交→打 tag 触发 CI）。当用户说「发版」「发布 vX.Y.Z」「提交发布」「release」或要求按流程打 tag 时使用。"
---

# 版本发布（Version Release）

本项目为 Tauri 2 + React + Rust 桌面应用（javaboot-launcher），发布产物由 GitHub Actions 在 tag 推送时自动构建 NSIS 安装包并发布 GitHub Release。本技能定义每次发版的完整执行流程，必须逐步执行，不得跳过预检。

## 触发条件

用户说「发版」「发布 vX.Y.Z」「提交发布」「走发布流程」「release」等，或要求为修复/新功能打 tag。

## 版本号规则

- 修复 / 内部优化 / 无新功能：patch 递增（0.19.3 → 0.19.4）
- 新增用户可见功能：minor 递增（0.19 → 0.20）
- 不发布大版本（major），除非用户明确要求

## 前置检查

1. 查看 `git status --short`，确认待发布改动范围；**排除无关目录**（如 `codefree-code-review/`、构建产物），只暂存发布相关文件
2. 若存在上次遗留的版本号漂移（如 package-lock 与 package.json 不一致），一并修正

## 执行步骤

### 1. 同步版本号（5 处）

把旧版本号（如 0.19.3）改为新版本号（如 0.19.4）：

| 文件 | 位置 | 同步方式 |
|---|---|---|
| `package.json` | 顶层 `"version"` | 手动 Edit |
| `package-lock.json` | 顶层 `version` + `packages[""].version` **两处** | 手动 Edit 或 `npm install --package-lock-only` 自动同步 |
| `src-tauri/tauri.conf.json` | 顶层 `"version"` | 手动 Edit |
| `src-tauri/Cargo.toml` | `[package] version` | 手动 Edit |
| `src-tauri/Cargo.lock` | `name = "javaboot-launcher"` 下方 `version` | 在 `src-tauri` 下跑 `cargo check` 自动同步 |

注意：`npm install --package-lock-only` 和 `cargo check` 能自动同步 lock 文件，推荐优先使用。

### 2. 更新 CHANGELOG.md

在 `## [Unreleased]` 章节之后插入新版本章节：

```markdown
## [0.19.4] - YYYY-MM-DD

### 修复

- 描述修复内容（说明根因 + 方案要点）
```

章节分类（按需使用，不需要的分类不写）：
- `### 新增`：新功能
- `### 变更`：行为/配置变更
- `### 修复`：bug 修复（建议写明根因）
- `### 优化`：性能/体验优化

格式遵循 Keep a Changelog。版本标题使用原样 `## [0.19.4]`（不要转义方括号），历史章节保持原样不动。

### 3. 本地预检（必须全部通过）

```powershell
# 项目根目录
npm ci
npm run typecheck
npm run lint          # 0 错误
npm run build         # 0 警告

# src-tauri 目录
cargo check
cargo test            # 全绿（含 git_cli 集成测试）
```

任一项失败即停止，修复后再继续。

### 4. 提交发布

- 只 `git add` 发布相关文件（版本文件 + CHANGELOG + 本次修复/功能的源文件），**不要 `git add -A` 或 `git add .`**
- Commit message 格式（不带任何工具签名）：

```
:tada: release(master): 发布 v0.19.4：<一句话要点>

- 版本号同步 5 处：package.json / package-lock.json（顶层+packages 两处）/ tauri.conf.json / Cargo.toml（含 Cargo.lock）
- 修复/新增：<要点逐条>
```

- **禁止在 commit 中出现真实姓名（高云峰）**，版权署名一律用 GitHub 用户名 `fenggeg`

### 5. 推送 + 打 tag

```powershell
git push origin master
git tag -a v0.19.4 -m "JavaBoot Launcher v0.19.4"
git push origin v0.19.4
```

tag 推送即触发 `.github/workflows/tauri-build.yml`（`on: push: tags: v*`），自动构建 NSIS 安装包并发布 GitHub Release。无需轮询监控，向用户报告「tag 已推送，CI 将自动构建并发布」。

最后确认 `git status --short` 干净（仅剩被忽略的目录）。

## 发布失败处理

- **CI 构建失败**：删除远端 tag 重打后修复重试
  ```powershell
  git push origin :refs/tags/vX.Y.Z
  git tag -d vX.Y.Z
  ```
  修复后重新走步骤 4~5。
- **GitHub Release 创建失败（`Resource not accessible by integration`）**：这是 tauri-build.yml 中 `softprops/action-gh-release` 与 `tauri-action` 争抢同一 tag Release 导致的（v0.19.2 根因）。确认 workflow 已移除冗余的 `softprops` 步骤，仅由 `tauri-action` 创建并上传。
- **版本号漂移**：检查 package-lock 两处版本是否一致。

## 关键约定

- 每次发版版本号必须 +1，不能重复使用已推送过的 tag
- 发布 workflow（tauri-build.yml）已优化：rust-cache 编译缓存、浅克隆 depth 20、`CARGO_INCREMENTAL=0`；**不要改动 `[profile.release]` 的 LTO/codegen-units 配置**（体积与启动速度优先）
- ci-check.yml 仅在手动 workflow_dispatch 触发，本地预检已覆盖其内容
