import * as api from "../api";
import type {FileContent} from "../types";

// ================================================================
// 文件类型分类
// ================================================================

export const IMAGE_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "avif",
]);

export const BINARY_EXTS = new Set([
  "jar", "class", "war", "ear", "zip", "tar", "gz", "7z", "rar",
  "exe", "dll", "lib", "so", "dylib", "o", "obj",
  "bin", "dat", "db", "sqlite", "wasm",
  "mp3", "mp4", "avi", "mov", "mkv", "flv", "wav", "flac",
  "ttf", "otf", "woff", "woff2", "eot",
]);

export type FileType = "image" | "binary" | "text";

export function getFileType(filename: string): FileType {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  if (IMAGE_EXTS.has(ext)) return "image";
  if (BINARY_EXTS.has(ext)) return "binary";
  return "text";
}

// ================================================================
// 自定义文件树节点数据
// ================================================================

export interface FileTreeNode {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileTreeNode[];
  loaded?: boolean; // 目录是否已懒加载子节点
  loading?: boolean;
  expanded?: boolean; // 目录是否展开
}

/** 打开的文件标签 */
export interface OpenTab {
  path: string;
  meta: FileContent;
  content: string;
  fileType: FileType;
  /** 图片 / 二进制的 asset 协议 URL */
  assetUrl?: string;
  /** 磁盘原始换行风格；保存时按原样还原（缓冲区内一律 LF） */
  eol?: "\n" | "\r\n";
}

/** 剪贴板：记录待复制 / 剪切的条目 */
export interface TreeClipboard {
  mode: "copy" | "cut";
  path: string;
}

/** 快速打开命中项 */
export interface QuickHit {
  path: string;
  name: string;
  dir: string;
  /** 排序分：越小越靠前 */
  score: number;
}

/** 紧凑路径最大合并层数（防止极端深目录导致请求风暴） */
export const MAX_COMPACT_DEPTH = 10;

/** 拖拽处理器接口 */
export interface DndHandlers {
  draggingPath: string | null;
  onDragStart: (path: string) => void;
  onDragEnd: () => void;
  /** 拖放到某个目录（path 为空串表示项目根） */
  onDropInto: (targetDir: string) => void;
}

// ================================================================
// 纯工具函数
// ================================================================

/** 把磁盘原始内容归一化为 LF 缓冲区 + 记录原 EOL 风格 */
export function toBufferEol(raw: string): { content: string; eol: "\n" | "\r\n" } {
  if (raw.includes("\r\n")) {
    return { content: raw.replace(/\r\n?/g, "\n"), eol: "\r\n" };
  }
  return { content: raw.replace(/\r/g, "\n"), eol: "\n" };
}

/** 取路径的目录部分（无分隔符时为空串） */
export function dirOf(p: string): string {
  const i = p.lastIndexOf("/");
  return i < 0 ? "" : p.slice(0, i);
}

/** 取路径的父目录（与 dirOf 同义，语义更贴合文件操作场景） */
export function parentOf(p: string): string {
  const idx = p.lastIndexOf("/");
  return idx < 0 ? "" : p.slice(0, idx);
}

/**
 * 加载目录并做「紧凑路径」合并：当目录下只有唯一子目录且无其他文件时，
 * 向下穿透并把路径段合并展示（如 src → main → java 显示为 src/main/java）。
 * 返回的节点 name 为合并路径，path 为最终目录的完整路径，children 已加载。
 */
export async function loadMergedNodes(
  projectId: string,
  path: string
): Promise<FileTreeNode[]> {
  const mergedNames: string[] = [];
  let cur = path;
  let entries = await api.listFiles(projectId, cur);
  // 连续单目录链：逐层向下钻取
  for (
    let i = 0;
    i < MAX_COMPACT_DEPTH && entries.length === 1 && entries[0]?.is_dir;
    i++
  ) {
    mergedNames.push(entries[0].name);
    cur = entries[0].path;
    entries = await api.listFiles(projectId, cur);
  }
  const nodes: FileTreeNode[] = [...entries]
    .sort((a, b) => {
      if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
      return a.name.localeCompare(b.name);
    })
    .map((e) => ({
      name: e.name,
      path: e.path,
      isDir: e.is_dir,
      loaded: !e.is_dir,
    }));
  if (mergedNames.length === 0) return nodes;
  // 合并为一个目录节点：name 展示合并路径，children 为最终层内容
  return [
    {
      name: mergedNames.join("/"),
      path: cur,
      isDir: true,
      children: nodes,
      loaded: true,
    },
  ];
}

/** 不可变更新：把 key 对应目录节点的 children 替换为加载结果 */
export function setChildren(
  nodes: FileTreeNode[],
  key: string,
  children: FileTreeNode[]
): FileTreeNode[] {
  return nodes.map((n) => {
    if (n.path === key) {
      return { ...n, children, loaded: true, loading: false };
    }
    if (n.children) {
      return { ...n, children: setChildren(n.children, key, children) };
    }
    return n;
  });
}

/** 不可变更新：切换 key 对应目录的展开/折叠状态 */
export function toggleExpand(
  nodes: FileTreeNode[],
  key: string
): FileTreeNode[] {
  return nodes.map((n) => {
    if (n.path === key) {
      return { ...n, expanded: !n.expanded };
    }
    if (n.children) {
      return { ...n, children: toggleExpand(n.children, key) };
    }
    return n;
  });
}

/** 收集子树内所有节点到 Map（path → 节点），用于刷新后恢复展开状态 */
export function collectNodeMap(
  nodes: FileTreeNode[],
  map: Map<string, FileTreeNode>
): void {
  for (const n of nodes) {
    map.set(n.path, n);
    if (n.children) collectNodeMap(n.children, map);
  }
}

/**
 * 把新加载的子树与旧子树按 path 合并：
 * 旧节点已展开的目录在刷新后保持展开（重命名 / 复制 / 移动后不丢失状态）
 */
export function mergePreserveExpand(
  fresh: FileTreeNode[],
  oldMap: Map<string, FileTreeNode>
): FileTreeNode[] {
  return fresh.map((n) => {
    const old = oldMap.get(n.path);
    const merged: FileTreeNode = old
      ? {...n, expanded: n.expanded || !!old.expanded}
      : n;
    if (merged.children) {
      merged.children = mergePreserveExpand(merged.children, oldMap);
    }
    return merged;
  });
}

/** 在树中查找指定 path 的节点 */
export function findNode(
  nodes: FileTreeNode[],
  key: string
): FileTreeNode | undefined {
  for (const n of nodes) {
    if (n.path === key) return n;
    if (n.children) {
      const found = findNode(n.children, key);
      if (found) return found;
    }
  }
  return undefined;
}

/** 递归标记某节点 loading */
export function markLoading(
  nodes: FileTreeNode[],
  key: string
): FileTreeNode[] {
  return nodes.map((n) => {
    if (n.path === key) {
      return { ...n, loading: true };
    }
    if (n.children) {
      return { ...n, children: markLoading(n.children, key) };
    }
    return n;
  });
}
