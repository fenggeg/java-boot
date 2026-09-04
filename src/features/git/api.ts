// Git 集成前端 API 封装：所有 git 调用都在 Rust 端完成，
// 这里只做 invoke 包装与 TS 类型声明（与后端 serde camelCase 契约解耦）。

import { invoke } from "@tauri-apps/api/core";

// ============================ 类型契约 ============================

/** 单个 hunk（行号 1-based；newLines=0 表示纯删除，start 是间隙前一行） */
export interface Hunk {
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
}

/** 单文件 diff 结果（gutter 数据源） */
export interface FileDiff {
  path: string;
  /** added / modified / deleted / renamed / unmodified / binary */
  status: string;
  isBinary: boolean;
  hunks: Hunk[];
}

/** 文件状态（git status 条目） */
export type GitStatus =
  | "untracked"
  | "added"
  | "modified"
  | "deleted"
  | "renamed"
  | "copied"
  | "unmodified";

export interface FileStatusEntry {
  path: string;
  oldPath: string | null;
  status: GitStatus;
  staged: boolean;
}

/** blame 单行归属 */
export interface BlameLine {
  sha: string;
  finalLine: number;
  author: string;
  /** epoch 秒 */
  time: number;
  summary: string;
}

/** git 可用性 + 真实仓库根（rev-parse --show-toplevel 解析） */
export interface GitAvailability {
  installed: boolean;
  isRepo: boolean;
  repoRoot: string | null;
}

// ============================ API ============================

/** 探测 git 可用性与仓库状态，并解析真实仓库根 */
export const gitAvailability = (repoRoot: string) =>
  invoke<GitAvailability>("git_availability", { repoRoot });

/** 全仓库文件状态列表（P1 文件树标记） */
export const gitStatusAll = (repoRoot: string) =>
  invoke<FileStatusEntry[]>("git_status_all", { repoRoot });

/** 单文件 diff hunk（P0 gutter 数据源） */
export const gitFileDiff = (repoRoot: string, filePath: string) =>
  invoke<FileDiff>("git_file_diff", { repoRoot, filePath });

/** HEAD 版本内容（P0 DiffEditor original）；新文件不在 HEAD → null */
export const gitFileAtHead = (repoRoot: string, filePath: string) =>
  invoke<string | null>("git_file_at_head", { repoRoot, filePath });

/** 单文件 blame（P2 hover 归属） */
export const gitBlame = (repoRoot: string, filePath: string) =>
  invoke<BlameLine[]>("git_blame", { repoRoot, filePath });

/** 后端 git 监听推送的事件名（有实质变化时触发） */
export const GIT_CHANGED_EVENT = "git://changed";

/** 状态 → 展示文案 */
export const GIT_STATUS_LABEL: Record<string, string> = {
  untracked: "未跟踪",
  added: "新增",
  modified: "已修改",
  deleted: "已删除",
  renamed: "已重命名",
  copied: "已复制",
};
