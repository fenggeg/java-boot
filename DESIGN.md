# JavaBoot Launcher — Design Spec

> 方向：**Vercel / Stripe 式「工程极简」**（工程控制台，不是 Apple 生活方式软件）  
> 替换对象：现有 Apple HIG（毛玻璃 / 大圆角 / 多彩 glow / 弹簧动效）  
> 范围：桌面端主界面（TopBar · 侧栏 · 日志 · 编辑器 Dock · Modal/Drawer）

---

## 1. Objective

把 JavaBoot Launcher 从「精致 macOS 小工具」改成「工程师每天开着看的本地控制台」：信息密度更高、装饰更少、状态一眼可读、深浅主题都像同一套产品。

成功标准：

- 打开截图给人的第一印象是 **Vercel/控制台**，不是 **iOS 设置页**
- 运行 / 异常 / 停止 在列表中 0.5 秒内可区分
- 主界面几乎无投影、无毛玻璃、无彩虹 glow
- token 改完后，组件零散 inline 颜色可大幅收敛

---

## 2. Product Context

| 项 | 值 |
|----|-----|
| 产品 | JavaBoot Launcher（Tauri 2 + React + antd 5） |
| 用户 | 本机跑多 Spring Boot 服务的后端/全栈开发者 |
| 主场景 | 看日志、启停服务、开文件/Diff、盯端口与异常 |
| 非场景 | 营销落地页、消费级内容流、需要「可爱」的场景 |
| 现状基线 | `src/styles/tokens.css`（Apple HIG）+ `src/theme-tokens.ts`（antd） |

---

## 3. Visual Foundations

### 3.1 色板（工程灰阶 + 单一强调）

**Light**

| Token | 值 | 用途 |
|-------|-----|------|
| `--bg` | `#fafafa` | 应用底 |
| `--bg-elevated` / `--surface` | `#ffffff` | 主面板、卡片、输入 |
| `--surface-2` | `#f5f5f5` | 次级条、hover 底 |
| `--surface-3` | `#ebebeb` | 更重填充 / 选中底 |
| `--border` | `#e5e5e5` | 1px 结构线（实色） |
| `--border-2` | `#d4d4d4` | 悬停/更强分隔 |
| `--border-3` | `#a3a3a3` | 仅特殊强调 |
| `--text` | `#0a0a0a` | 主文字 |
| `--text-2` | `#525252` | 次要 |
| `--text-3` | `#737373` | 弱化 |
| `--text-4` | `#a3a3a3` | 占位 |
| `--blue` | `#0070f3` | **唯一品牌强调**（主按钮、焦点、链接） |
| `--green` | `#16a34a` | 运行中 |
| `--orange` | `#d97706` | 启动中/编译中/停止中 |
| `--red` | `#dc2626` | 错误/危险 |
| `--purple` | `#7c3aed` | 拉取中等次级状态（少用） |

**Dark**

| Token | 值 |
|-------|-----|
| `--bg` | `#0a0a0a` |
| `--surface` | `#111111` |
| `--surface-2` | `#1a1a1a` |
| `--surface-3` | `#262626` |
| `--border` | `#262626` |
| `--border-2` | `#404040` |
| `--text` | `#ededed` |
| `--text-2` | `#a1a1a1` |
| `--text-3` | `#737373` |
| `--blue` | `#3291ff` |
| `--green` | `#22c55e` |
| `--orange` | `#f59e0b` |
| `--red` | `#ef4444` |

**禁用**：`--*-glow` 作为装饰；大面积彩色底；状态以外的彩色图标。

### 3.2 字体

| 用途 | 栈 |
|------|-----|
| UI | `"Inter", "Segoe UI", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif` |
| Mono | `"JetBrains Mono", "SF Mono", ui-monospace, Consolas, monospace` |

Scale（紧凑桌面）：

| 角色 | Size / Weight / LH |
|------|-------------------|
| 标题（空态 hero） | 14–15 / 600 / 1.4 |
| 面板头 | 12 / 500 / 1.4 · letter-spacing 0.01em |
| 正文/列表 | 13 / 400 / 1.45 |
| 元数据/徽标 | 11–12 / 500 · tabular-nums |
| 日志/代码 | 12 mono / 1.55 |

**不要** 18+ 大标题占主界面；标题感靠字重与灰阶，不靠字号。

### 3.3 圆角

| Token | 值 | 用途 |
|-------|-----|------|
| `--r-sm` | `4px` | 按钮、chip、输入 |
| `--r-md` | `6px` | 卡片、弹层 |
| `--r-lg` | `8px` | Drawer/Modal 最大 |
| `--r-pill` | `9999px` | 仅状态点容器或极小标签 |

禁止 12–20px 大圆角。

### 3.4 边框与 elevation

- 结构分隔：**1px solid `--border`**，不用半透明毛玻璃
- 层级：优先 **边框 + 背景微差**（surface vs bg）
- 阴影：默认 **无**；浮层最多 `0 8px 30px rgba(0,0,0,0.12)`（dark 更淡）
- TopBar / 侧栏：**实色 + 底/右边线**，去掉 `backdrop-filter` 与半透明 surface

### 3.5 动效

| 项 | 值 |
|----|-----|
| 默认时长 | 120–180ms |
| 曲线 | `cubic-bezier(0.16, 1, 0.3, 1)`（ease-out-expo 类） |
| 禁止 | spring 过冲（`--ease-spring`）、大面积 fade-in 交错长动画 |

### 3.6 状态色语义（服务状态）

| 状态 | 视觉 |
|------|------|
| stopped | 6px 实心圆 `--text-4` + 文案「已停止」 |
| running | 实心 `--green`（可 1.2s 微弱 pulse，仅圆点） |
| starting / recompiling / stopping | `--orange` |
| error | `--red` |
| pulling | `--purple` |

侧栏服务行：**默认无彩色底**；error 行可用 `border-left: 2px solid red` 或整行极淡 `rgba(220,38,38,0.04)`。

### 3.7 组件语言摘要

| 元素 | 规则 |
|------|------|
| 主按钮 | 实底 `--blue` + 白字，radius 4px，高 30–32px |
| 次按钮 | 白/`surface` + 1px border，hover `surface-2` |
| 危险 | 文字/描边红，确认再实底红 |
| 图标按钮 | 28×28，hover `surface-2`，无 glow |
| Tab（日志） | 下边线 active `--text`，无大圆角 pill |
| 输入 | 1px border，focus：border `--blue` + 极轻 ring |
| 表格/列表行 | 分隔线，hover `surface-2`，不整行大圆角卡 |

---

## 4. Accessibility

- 正文对比：light 下 `#0a0a0a` on `#ffffff` ≥ 14:1；`--text-3` 不用于关键信息
- 焦点：键盘可见 `outline: 2px solid var(--blue); outline-offset: 2px`（或 token ring）
- 状态不只靠颜色：圆点旁保留中文标签或 aria-label
- 动效尊重 `prefers-reduced-motion`

---

## 5. Voice & Tone

- UI 文案：短、工程化（「停止全部」「端口冲突」「收起编辑器」）
- 空态：一句话 + 主 CTA，无营销口号
- 错误：原因优先，少感叹号

---

## 6. Implementation Practices

- **单一 token 源**：CSS 变量在 `src/styles/tokens.css`；antd 在 `src/theme-tokens.ts`，数值必须同源映射
- 消灭组件内硬编码 `#0071e3` / `rgba(0,0,0,0.06)` hover，改 `var(--*)`
- antd `ConfigProvider`：`borderRadius: 4`，colorPrimary → `#0070f3` / dark `#3291ff`
- 毛玻璃相关类（`--surface-blur`、`backdrop-filter`）迁移期可保留变量但 **UI 不再引用**
- 分屏/Dock 行为不变，只改视觉 token 与表面材质

---

## 7. Anti-Patterns（本项目明确不要）

- 紫蓝粉渐变 hero / radial glow 画布
- 大圆角 + 大阴影「卡片墙」
- 彩虹 status glow（绿 glow、橙 glow…）
- 毛玻璃 TopBar/侧栏
- 弹簧过冲动画
- 用 14+ 种颜色表示优先级
- 英雄区巨字营销文案

---

## 8. Decision-Making

| 决策 | 依据 |
|------|------|
| 实线边框替代毛玻璃 | 控制台需要稳定边界，振镜材质干扰密度阅读 |
| 单蓝强调 | Vercel/Stripe 靠状态色+灰阶，品牌色只给可点/焦点 |
| radius 4–8 | 工程工具；10–20 偏消费 iOS |
| Inter + JetBrains | 工程默认组合，中英混排可接受；不强行 SF Pro |
| 分期改 token | 行为已稳定，先换皮肤再抠个别组件 |

---

## 9. Workflow（改造分期）

见会话中「分期实施」：P0 token/antd → P1 表面与 TopBar/侧栏 → P2 列表与空态/Modal → P3 深色校对与清理。
