import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {App, Dropdown, Input, Modal, Segmented, Spin, Tooltip} from "antd";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {convertFileSrc} from "@tauri-apps/api/core";
import {
  Binary,
  CaretDown,
  CaretRight,
  ChevronLeft,
  ClipboardPaste,
  Copy,
  Edit,
  File,
  Folder,
  FolderOpen,
  GitBranch,
  Image as ImageIcon,
  Save,
  Scissors,
  Terminal,
  X,
} from "./Icons";
import {Prism} from "../prism-langs";
import {getPrismLang, isMarkdown} from "../languages";
import * as api from "../api";
import type {
  DirGitAgg,
  FileContent,
  FileGitStatus,
  GitChange,
  GitCommitInfo,
  Project,
} from "../types";
import {gitChangeKind} from "../types";
import TerminalView from "./TerminalView";

interface Props {
  project: Project;
  onClose: () => void;
  /** 打开同项目的 Git 工作区面板 */
  onOpenGit: (project: Project) => void;
  /** 面板是否可见（重新可见时刷新 git 状态） */
  visible?: boolean;
}

/** 代码区行高（px）：视图 / 编辑底层 / 输入层必须一致，diff 标记按此定位 */
const LINE_H = 22;
/** 代码区顶部内边距（px），与 CSS 保持一致 */
const GUTTER_PAD = 12;
/**
 * 编辑器搜索内容上限（字符）：与后端 MAX_EDIT_SIZE(2MB) 对齐——
 * 后端 2MB 内均可编辑/预览，搜索应当可用；再大则禁扫防按键卡顿
 */
const SEARCH_LIMIT = 2_000_000;

// ================================================================
// 文件类型分类
// ================================================================

const IMAGE_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "avif",
]);

const BINARY_EXTS = new Set([
  "jar", "class", "war", "ear", "zip", "tar", "gz", "7z", "rar",
  "exe", "dll", "lib", "so", "dylib", "o", "obj",
  "bin", "dat", "db", "sqlite", "wasm",
  "mp3", "mp4", "avi", "mov", "mkv", "flv", "wav", "flac",
  "ttf", "otf", "woff", "woff2", "eot",
]);

type FileType = "image" | "binary" | "text";

function getFileType(filename: string): FileType {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  if (IMAGE_EXTS.has(ext)) return "image";
  if (BINARY_EXTS.has(ext)) return "binary";
  return "text";
}

/** 按文件名渲染类型图标（文件树 / 标签栏 / 快速打开共用） */
function FileTypeIcon({name, size}: {name: string; size: number}) {
  const ft = getFileType(name);
  return (
    <span
      className={`file-tree-icon file ${
        ft === "image" ? "file-type-image" : ft === "binary" ? "file-type-binary" : ""
      }`}
    >
      {ft === "image" ? (
        <ImageIcon size={size} />
      ) : ft === "binary" ? (
        <Binary size={size} />
      ) : (
        <File size={size} />
      )}
    </span>
  );
}

/** 语法高亮：返回 HTML 字符串（无匹配语言时返回转义后的纯文本） */
function highlightCode(code: string, lang: string | null): string {
  if (!lang) return escapeHtml(code);
  const grammar = Prism.languages[lang];
  if (!grammar) return escapeHtml(code);
  try {
    return Prism.highlight(code, grammar, lang);
  } catch {
    return escapeHtml(code);
  }
}

/** HTML 转义，防止代码内容被当作标签解析 */
function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// ================================================================
// 自定义文件树节点数据（独立于 antd Tree）
// ================================================================
interface FileTreeNode {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileTreeNode[];
  loaded?: boolean; // 目录是否已懒加载子节点
  loading?: boolean;
  expanded?: boolean; // 目录是否展开
}

/** 打开的文件标签 */
interface OpenTab {
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
interface TreeClipboard {
  mode: "copy" | "cut";
  path: string;
}

/** 快速打开命中项 */
interface QuickHit {
  path: string;
  name: string;
  dir: string;
  /** 排序分：越小越靠前 */
  score: number;
}

/** 由 porcelain 记录归一化状态：与 Git 面板共用 gitChangeKind，保证两处归类一致 */
function classifyChange(ch: GitChange): FileGitStatus {
  return gitChangeKind(ch);
}

/**
 * 行级 diff 标记：0=未变 1=修改 2=新增
 * 3=删除：该位置上方相对 HEAD 有行被移除（缓冲区中已无对应行），
 * 标记画在删除点之后的第一行上，与 Git 面板的「-N」口径对应
 */
type LineKind = 0 | 1 | 2 | 3;

/** 行比较的归一化：忽略行尾 CR，抵消 core.autocrlf 的 CRLF/LF 差异 */
function normLine(l: string): string {
  return l.endsWith("\r") ? l.slice(0, -1) : l;
}

/**
 * 统一缓冲区换行为 LF，并记录磁盘原始 EOL。
 * 受控 textarea 的 DOM 值会被浏览器强制归一化为 LF：state 里保留 CRLF 会让
 * value prop 与 DOM 值永久不一致，任何一次无关 re-render（CPU 监控等每秒刷新）
 * 都会触发 React 重写 textarea.value，打断光标位置与输入法组合，
 * 表现为插入内容落在光标错位处。缓冲区内一律 LF，保存时按记录的 EOL 还原。
 */
function toBufferEol(raw: string): { content: string; eol: "\n" | "\r\n" } {
  if (raw.includes("\r\n")) {
    return { content: raw.replace(/\r\n?/g, "\n"), eol: "\r\n" };
  }
  return { content: raw.replace(/\r/g, "\n"), eol: "\n" };
}

/**
 * 行级 LCS diff（HEAD 内容 → 当前编辑器内容），返回与编辑器行号精确对齐的标记。
 * 不再使用 git hunk 行号映射磁盘文件——那在缓冲区与磁盘不一致（未保存编辑、
 * 外部修改）时整体错位。这里直接对显示中的内容做 diff，位置永远正确。
 * 策略：公共前后缀裁剪 + 中间区 suffix-DP 正向贪心；中间区过大时降级为整段「修改」。
 */
function diffLineKinds(head: string, buf: string): LineKind[] {
  const a = head.split("\n");
  const b = buf.split("\n");
  const kinds: LineKind[] = new Array(b.length).fill(0);
  let pre = 0;
  while (
    pre < a.length &&
    pre < b.length &&
    normLine(a[pre]!) === normLine(b[pre]!)
  ) {
    pre++;
  }
  let suf = 0;
  while (
    suf < a.length - pre &&
    suf < b.length - pre &&
    normLine(a[a.length - 1 - suf]!) === normLine(b[b.length - 1 - suf]!)
  ) {
    suf++;
  }
  const am = a.length - pre - suf;
  const bm = b.length - pre - suf;
  if (bm === 0) {
    // 纯删除：缓冲区无行可标，删除条落在紧随删除点的第一行（贴文件尾则用最后一行）
    if (am > 0 && b.length > 0) {
      kinds[Math.min(pre, b.length - 1)] = 3;
    }
    return kinds;
  }
  if (am === 0) {
    // 纯新增
    for (let i = pre; i < pre + bm; i++) kinds[i] = 2;
    return kinds;
  }
  if (am * bm > 1_600_000) {
    // 差异区过大：整段标为修改（位置仍精确，只是不细分橙/绿）
    for (let i = pre; i < pre + bm; i++) kinds[i] = 1;
    return kinds;
  }
  // 中间区 suffix-DP（dp[i][j] = a[i..] 与 b[j..] 的 LCS 长度），
  // 随后从左上角正向贪心走位。不能用「前向填表+箭头回溯」：
  // 在重复行密集的配置文件里它会选中合法但劣质的最长公共子序列——
  // 把分散的多处变更挤成一段连续块、行号整体漂移（实测 yml 三段变一段）。
  // 正向走位每步优先消费相同行、并列时先删除后新增，分块与 git diff 一致。
  const m = am;
  const n = bm;
  const dp: Uint32Array[] = Array.from({length: m + 1}, () => new Uint32Array(n + 1));
  for (let i = m - 1; i >= 0; i--) {
    const av = normLine(a[pre + i]!);
    const row = dp[i]!;
    const next = dp[i + 1]!;
    for (let j = n - 1; j >= 0; j--) {
      row[j] =
        av === normLine(b[pre + j]!)
          ? next[j + 1]! + 1
          : Math.max(next[j]!, row[j + 1]!);
    }
  }
  // 正向收集操作序列（0=HEAD侧删除 1=缓冲区新增 2=相同），再按连续块正向分配：
  // 每个差异块内前 min(删,增) 个新增行标为「修改」，其余为「新增」（与 git hunk 口径一致）
  const ops: number[] = [];
  let i = 0;
  let j = 0;
  while (i < m && j < n) {
    if (normLine(a[pre + i]!) === normLine(b[pre + j]!)) {
      ops.push(2);
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) {
      ops.push(0);
      i++;
    } else {
      ops.push(1);
      j++;
    }
  }
  while (i < m) {
    ops.push(0);
    i++;
  }
  while (j < n) {
    ops.push(1);
    j++;
  }
  // 正向走位得到的已是文档顺序，无需翻转（旧的箭头回溯从尾部出发才需要）
  let row = pre;
  let t = 0;
  while (t < ops.length) {
    const op = ops[t]!;
    if (op === 2) {
      // 相同行占缓冲区一行，必须推进标记行号，否则后续差异块整体上移
      row++;
      t++;
      continue;
    }
    let delN = 0;
    let addN = 0;
    let t2 = t;
    while (t2 < ops.length && ops[t2] !== 2) {
      if (ops[t2] === 0) delN++;
      else addN++;
      t2++;
    }
    const modN = Math.min(delN, addN);
    for (let k = 0; k < addN; k++) {
      kinds[row + k] = k < modN ? 1 : 2;
    }
    row += addN;
    // 块内净删除：在该差异块缓冲区末行的下一行标「删除」（EOF 处贴最后一行）；
    // 目标行只会是后续公共行或尾缀行（kind 0），不会覆盖已打的修改/新增
    if (delN > addN) {
      const target = Math.min(row, b.length - 1);
      if (kinds[target] === 0) kinds[target] = 3;
    }
    t = t2;
  }
  return kinds;
}

/** 紧凑路径最大合并层数（防止极端深目录导致请求风暴） */
const MAX_COMPACT_DEPTH = 10;

/**
 * 加载目录并做「紧凑路径」合并：当目录下只有唯一子目录且无其他文件时，
 * 向下穿透并把路径段合并展示（如 src → main → java 显示为 src/main/java）。
 * 返回的节点 name 为合并路径，path 为最终目录的完整路径，children 已加载。
 */
async function loadMergedNodes(
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
function setChildren(
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
function toggleExpand(
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
function collectNodeMap(
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
function mergePreserveExpand(
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

// ================================================================
// 单个树行（递归组件）— 根据文件类型显示不同图标；支持右键菜单与拖拽移动
// ================================================================
interface DndHandlers {
  draggingPath: string | null;
  onDragStart: (path: string) => void;
  onDragEnd: () => void;
  /** 拖放到某个目录（path 为空串表示项目根） */
  onDropInto: (targetDir: string) => void;
}

/** 目录聚合状态 → 展示优先级（冲突 > 红 > 紫 > 橙 > 绿） */
function dirStatusClass(agg?: DirGitAgg): string {
  if (!agg || agg.count === 0) return "";
  if (agg.conflict) return "gs-conflict";
  if (agg.deleted) return "gs-deleted";
  if (agg.modified) return "gs-modified";
  if (agg.renamed) return "gs-renamed";
  if (agg.added) return "gs-added";
  return "";
}

/** 状态中文名（编辑器徽标用），与 Git 面板 KIND_META 文案一致 */
function statusLabel(k: FileGitStatus): string {
  switch (k) {
    case "added":
      return "新增";
    case "untracked":
      return "未跟踪";
    case "deleted":
      return "已删除";
    case "renamed":
      return "重命名";
    case "conflict":
      return "冲突";
    default:
      return "已修改";
  }
}

/** 取路径的目录部分（无分隔符时为空串） */
function dirOf(p: string): string {
  const i = p.lastIndexOf("/");
  return i < 0 ? "" : p.slice(0, i);
}

function TreeRow({
  node,
  depth,
  activePath,
  onSelect,
  onToggle,
  onContextMenu,
  dnd,
  statusFor,
  statsFor,
}: {
  node: FileTreeNode;
  depth: number;
  activePath: string | null;
  onSelect: (path: string) => void;
  onToggle: (path: string) => void;
  onContextMenu: (e: React.MouseEvent, path: string, isDir: boolean) => void;
  dnd: DndHandlers;
  /** 节点 → git 状态查找（文件返回 kind，目录返回聚合 agg） */
  statusFor: (node: FileTreeNode) => { kind?: FileGitStatus; agg?: DirGitAgg };
  /** 路径 → ±行数统计（git diff 聚合，异步到达） */
  statsFor?: (path: string) => {a: number; d: number} | undefined;
}) {
  const gs = statusFor(node);
  const [dropHover, setDropHover] = useState(false);
  const isActive = activePath === node.path;

  // 拖放合法性：目标目录不能是被拖动条目自身或其子孙目录
  // （把子目录拖回父目录 / 祖先目录是合法移动）
  const droppable =
    !!node.isDir &&
    !!dnd.draggingPath &&
    node.path !== dnd.draggingPath &&
    !node.path.startsWith(dnd.draggingPath + "/");

  const commonHandlers = {
    onContextMenu: (e: React.MouseEvent) =>
      onContextMenu(e, node.path, node.isDir),
    draggable: true,
    onDragStart: (e: React.DragEvent) => {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", node.path);
      dnd.onDragStart(node.path);
    },
    onDragEnd: () => {
      setDropHover(false);
      dnd.onDragEnd();
    },
  };

  if (node.isDir) {
    const aggClass = dirStatusClass(gs.agg);
    return (
      <>
        <div
          className={[
            "file-tree-row",
            dropHover && droppable ? "drop-target" : "",
          ].join(" ")}
          style={{ paddingLeft: 8 + depth * 14 }}
          onClick={() => onToggle(node.path)}
          onDragOver={(e) => {
            if (droppable) {
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              setDropHover(true);
            }
          }}
          onDragLeave={() => setDropHover(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDropHover(false);
            if (droppable) dnd.onDropInto(node.path);
          }}
          {...commonHandlers}
        >
          <span className="file-tree-caret">
            {node.loading ? (
              <Spin size="small" style={{ transform: "scale(0.6)" }} />
            ) : node.expanded ? (
              <CaretDown size={10} />
            ) : (
              <CaretRight size={10} />
            )}
          </span>
          <span className={`file-tree-icon folder ${aggClass}`}>
            <Folder size={14} />
          </span>
          <span className={`file-tree-name ${aggClass}`}>{node.name}</span>
          {gs.agg && gs.agg.count > 0 && (
            <span className={`file-tree-badge ${aggClass}`}>
              {gs.agg.count}
            </span>
          )}
        </div>
        {node.expanded && node.children && node.children.length > 0 && (
          <div className="file-tree-children">
            {node.children.map((child) => (
              <TreeRow
                key={child.path}
                node={child}
                depth={depth + 1}
                activePath={activePath}
                onSelect={onSelect}
                onToggle={onToggle}
                onContextMenu={onContextMenu}
                dnd={dnd}
                statusFor={statusFor}
                statsFor={statsFor}
              />
            ))}
          </div>
        )}
        {node.expanded && node.children && node.children.length === 0 && (
          <div
            className="file-tree-empty-hint"
            style={{ paddingLeft: 8 + (depth + 1) * 14 }}
          >
            空目录
          </div>
        )}
      </>
    );
  }

  return (
    <div
      className={`file-tree-row file ${isActive ? "active" : ""}`}
      style={{ paddingLeft: 8 + depth * 14 }}
      onClick={() => onSelect(node.path)}
      {...commonHandlers}
    >
      <span className="file-tree-caret" />
      <FileTypeIcon name={node.name} size={14} />
      <span className={`file-tree-name ${gs.kind ? `gs-${gs.kind}` : ""}`}>
        {node.name}
      </span>
      {gs.kind &&
        (() => {
          const s = statsFor?.(node.path);
          if (s && (s.a > 0 || s.d > 0)) {
            return (
              <span
                className="tree-lines"
                title={`${statusLabel(gs.kind)}：+${s.a} / -${s.d} 行`}
              >
                {s.a > 0 && <b className="la">+{s.a}</b>}
                {s.d > 0 && <b className="ld">-{s.d}</b>}
              </span>
            );
          }
          return (
            <span
              className={`file-tree-dot dot-${gs.kind}`}
              title={statusLabel(gs.kind)}
            />
          );
        })()}
    </div>
  );
}

// ================================================================
// 主组件
// ================================================================
export default function FilePanel({
  project,
  onClose,
  onOpenGit,
  visible = true,
}: Props) {
  const { message, modal } = App.useApp();
  const [treeData, setTreeData] = useState<FileTreeNode[]>(() => [
    {
      name: project.name,
      path: "",
      isDir: true,
      loaded: false,
      expanded: true,
    },
  ]);
  // 多标签打开的文件（保留各自未保存内容，切换不丢编辑）
  const [tabs, setTabs] = useState<OpenTab[]>([]);
  // tabs 的实时镜像：异步流程里读取最新标签列表（避免闭包过期）
  const tabsRef = useRef<OpenTab[]>([]);
  tabsRef.current = tabs;
  const [activePath, setActivePath] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 编辑态语法高亮底层（与输入层滚动同步）
  const editorUnderlayRef = useRef<HTMLPreElement>(null);
  // 文本/标记文件的查看模式：仅 Markdown 使用（预览 ↔ 编辑）；
  // 其他文本文件无需切换，直接在单页内编辑（只读文件静态展示）
  const [viewMode, setViewMode] = useState<"view" | "edit">("view");

  // ---- 行号栏 ----
  /** 等宽字符实测宽度（画布测量），供行号宽与搜索高亮定位使用 */
  const [charW, setCharW] = useState(7.8);
  const lnGutterRef = useRef<HTMLDivElement>(null);
  const editorInputRef = useRef<HTMLTextAreaElement>(null);
  const readonlyPreRef = useRef<HTMLPreElement>(null);

  // ---- 编辑器内搜索 ----
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [matchIdx, setMatchIdx] = useState(0);
  const matchLayerRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // 右键菜单 / 剪贴板 / 重命名弹窗 / 拖拽
  const [ctxMenu, setCtxMenu] = useState<{
    x: number;
    y: number;
    path: string;
    isDir: boolean;
  } | null>(null);
  // 标签页右键菜单（锚点定位与文件树菜单同一套做法）
  const [tabCtx, setTabCtx] = useState<{
    x: number;
    y: number;
    path: string;
  } | null>(null);
  const [clipboard, setClipboard] = useState<TreeClipboard | null>(null);
  const [renaming, setRenaming] = useState<{ path: string; name: string } | null>(
    null
  );
  const [draggingPath, setDraggingPath] = useState<string | null>(null);

  // Git 状态标记：文件级状态 + 目录聚合状态
  const [statusMap, setStatusMap] = useState<Map<string, FileGitStatus>>(
    new Map()
  );
  const [dirAgg, setDirAgg] = useState<Map<string, DirGitAgg>>(new Map());
  const gitTimerRef = useRef<number | undefined>(undefined);
  // 变更文件 ± 行数统计（git diff hunks 聚合）：路径 → {新增, 删除}
  const [lineStats, setLineStats] = useState<
    Map<string, {a: number; d: number}>
  >(new Map());
  const lineStatSeqRef = useRef(0);

  // 行级 diff 标记（工作区内容 vs HEAD）：与激活文件行号对齐
  const [lineKinds, setLineKinds] = useState<LineKind[] | null>(null);

  // ================================================================
  // 文件历史 / 回滚浮层（点击左缘 diff 标记条唤起，交互类似 IDEA 的行标记历史）
  // ================================================================
  /** 浮层锚点：触发行的下标（0 基）；null=关闭 */
  const [histAnchor, setHistAnchor] = useState<number | null>(null);
  const [histLoading, setHistLoading] = useState(false);
  const [histErr, setHistErr] = useState<string | null>(null);
  const [histCommits, setHistCommits] = useState<GitCommitInfo[]>([]);
  /** 预览中的提交 */
  const [histPreviewHash, setHistPreviewHash] = useState<string | null>(null);
  const [histPreviewLoading, setHistPreviewLoading] = useState(false);
  const [histPreviewText, setHistPreviewText] = useState("");
  /** 回滚二次确认 armed 的提交哈希（3 秒未确认自动复位） */
  const [rollbackArm, setRollbackArm] = useState<string | null>(null);
  // diff 刷新序号：保存成功 / 面板重新可见时递增，强制重算行标记
  const [diffRev, setDiffRev] = useState(0);
  const gutterInnerRef = useRef<HTMLDivElement | null>(null);

  // 集成终端抽屉
  const [termOpen, setTermOpen] = useState(false);
  const [termHeight, setTermHeight] = useState<number>(() => {
    const saved = localStorage.getItem("jb_term_height");
    return saved ? Math.max(120, parseInt(saved, 10) || 220) : 220;
  });
  const termResizingRef = useRef(false);

  // 可拖拽宽度：文件树宽度持久化到 localStorage
  const [treeWidth, setTreeWidth] = useState<number>(() => {
    const saved = localStorage.getItem("jb_file_tree_width");
    return saved ? parseInt(saved, 10) || 240 : 240;
  });
  const draggingRef = useRef(false);
  const panelBodyRef = useRef<HTMLDivElement>(null);

  const activeTab = useMemo(
    () => tabs.find((t) => t.path === activePath) ?? null,
    [tabs, activePath]
  );

  /** 激活文件的 git 状态（编辑器徽标用） */
  const activeGitKind = useMemo(
    () => (activeTab ? statusMap.get(activeTab.path) : undefined),
    [activeTab, statusMap]
  );

  // 切换激活标签时回到查看模式
  useEffect(() => {
    setViewMode("view");
  }, [activePath]);

  // ================================================================
  // Git 状态：文件树 / 标签栏着色 + 目录聚合计数
  // ================================================================

  /** 拉取 git status 并构建 文件状态表 / 目录聚合表 */
  const refreshGitStatus = useCallback(async () => {
    if (!project.git_available) {
      setStatusMap((prev) => (prev.size ? new Map() : prev));
      setDirAgg((prev) => (prev.size ? new Map() : prev));
      return;
    }
    try {
      const st = await api.gitStatus(project.id);
      const fmap = new Map<string, FileGitStatus>();
      const dmap = new Map<string, DirGitAgg>();
      const bump = (rel: string) => {
        let agg = dmap.get(rel);
        if (!agg) {
          agg = {
            modified: false,
            added: false,
            deleted: false,
            renamed: false,
            conflict: false,
            count: 0,
          };
          dmap.set(rel, agg);
        }
        return agg;
      };
      for (const ch of st.changes) {
        if (fmap.has(ch.path)) continue;
        const kind = classifyChange(ch);
        fmap.set(ch.path, kind);
        // 根目录与所有祖先目录聚合
        const rootAgg = bump("");
        rootAgg.count++;
        if (kind === "modified") rootAgg.modified = true;
        else if (kind === "deleted") rootAgg.deleted = true;
        else if (kind === "renamed") rootAgg.renamed = true;
        else if (kind === "conflict") rootAgg.conflict = true;
        else rootAgg.added = true;
        const segs = ch.path.split("/");
        segs.pop();
        let cur = "";
        for (const s of segs) {
          cur = cur ? `${cur}/${s}` : s;
          const agg = bump(cur);
          agg.count++;
          if (kind === "modified") agg.modified = true;
          else if (kind === "deleted") agg.deleted = true;
          else if (kind === "renamed") agg.renamed = true;
          else if (kind === "conflict") agg.conflict = true;
          else agg.added = true;
        }
      }
      setStatusMap(fmap);
      setDirAgg(dmap);
    } catch {
      // 非 git 仓库 / git 不可用：静默清空标记
      setStatusMap(new Map());
      setDirAgg(new Map());
    }
  }, [project.id, project.git_available]);

  /** 防抖刷新（保存/重命名/移动等操作后调用，避免频繁 spawn git） */
  const scheduleGitRefresh = useCallback(() => {
    window.clearTimeout(gitTimerRef.current);
    gitTimerRef.current = window.setTimeout(
      () => void refreshGitStatus(),
      300
    );
  }, [refreshGitStatus]);

  // 变更文件 ± 行数聚合：对每个已跟踪改动文件取 unified=0 hunks 求和。
  // 并发分批拉取，序号失效防止状态刷新竞态；未跟踪文件无 diff（整文件新增）
  // 不参与统计。上限 300 个文件防请求风暴。
  useEffect(() => {
    if (!project.git_available || statusMap.size === 0) {
      setLineStats((prev) => (prev.size ? new Map() : prev));
      return;
    }
    const seq = ++lineStatSeqRef.current;
    const paths = [...statusMap.entries()]
      .filter(([, k]) => k !== "untracked")
      .map(([p]) => p)
      .slice(0, 300);
    (async () => {
      const out = new Map<string, {a: number; d: number}>();
      const CH = 8;
      for (let i = 0; i < paths.length; i += CH) {
        const part = await Promise.all(
          paths.slice(i, i + CH).map(async (p) => {
            try {
              const hs = await api.gitDiffHunks(project.id, p);
              let a = 0;
              let d = 0;
              for (const h of hs) {
                a += h.new_lines;
                d += h.del_lines;
              }
              return [p, {a, d}] as const;
            } catch {
              return null;
            }
          })
        );
        for (const it of part) if (it) out.set(it[0], it[1]);
        if (seq !== lineStatSeqRef.current) return; // 已有更新的刷新
      }
      if (seq !== lineStatSeqRef.current) return;
      setLineStats(out);
    })();
  }, [statusMap, project.id, project.git_available]);

  /**
   * 外部修改同步：把无未保存编辑的文本标签页从磁盘静默重读。
   * Git 面板 / 文件树状态始终读磁盘，若编辑器缓冲区停留在外部修改前，
   * 行级 diff 标记（HEAD vs 缓冲区）会与面板不一致——聚焦/可见刷新时一并重读。
   * updater 内二次校验「仍未保存」，保护 await 期间用户产生的新编辑。
   */
  const syncCleanTabsFromDisk = useCallback(async () => {
    const clean = tabsRef.current.filter(
      (t) =>
        t.fileType === "text" && t.content === t.meta.content
    );
    for (const t of clean) {
      try {
        const fresh = await api.readProjectFile(project.id, t.path);
        const buf = toBufferEol(fresh.content);
        setTabs((prev) =>
          prev.map((tb) =>
            tb.path === t.path && tb.content === tb.meta.content
              ? {
                  ...tb,
                  content: buf.content,
                  meta: {...fresh, content: buf.content},
                  eol: tb.eol ?? buf.eol,
                }
              : tb
          )
        );
      } catch {
        /* 文件可能已被外部删除 / 移动：保留原标签内容 */
      }
    }
  }, [project.id]);

  // 面板重新可见 / 切换项目时刷新 git 状态（含行级 diff，可能在外部被改动）
  useEffect(() => {
    if (visible) {
      void refreshGitStatus();
      setDiffRev((v) => v + 1);
      void syncCleanTabsFromDisk();
    }
  }, [visible, refreshGitStatus, syncCleanTabsFromDisk]);

  // 窗口重新聚焦时刷新：覆盖在 IDE 等外部工具中编辑/提交后切回的场景，
  // 否则文件树状态点与行级 diff 标记会停留在过期数据上
  useEffect(() => {
    if (!visible) return;
    const onFocus = () => {
      scheduleGitRefresh();
      setDiffRev((v) => v + 1);
      void syncCleanTabsFromDisk();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [visible, scheduleGitRefresh, syncCleanTabsFromDisk]);

  // 懒加载目录子节点（含紧凑路径合并）
  const loadChildren = useCallback(async (path: string) => {
    // 标记目标节点 loading
    setTreeData((prev) => markLoading(prev, path));
    try {
      const nodes = await loadMergedNodes(project.id, path);
      setTreeData((prev) => setChildren(prev, path, nodes));
    } catch (e: any) {
      message.error(`加载目录失败: ${e}`);
      setTreeData((prev) => setChildren(prev, path, []));
    }
  }, [project.id, message]);

  /**
   * 刷新某目录内容（文件操作后调用）：
   * 先快照当前展开状态，重载后按 path 恢复，展开的目录不会折叠
   */
  const refreshDir = useCallback(
    async (dirPath: string) => {
      const snapshot = new Map<string, FileTreeNode>();
      const target = findNode(treeData, dirPath);
      if (target?.children) collectNodeMap(target.children, snapshot);
      try {
        const fresh = await loadMergedNodes(project.id, dirPath);
        const merged = mergePreserveExpand(fresh, snapshot);
        setTreeData((prev) => setChildren(prev, dirPath, merged));
      } catch (e) {
        message.error(`刷新目录失败: ${e}`);
      }
    },
    [project.id, treeData, message]
  );

  // 递归标记某节点 loading
  function markLoading(nodes: FileTreeNode[], key: string): FileTreeNode[] {
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

  // 展开/折叠目录
  const handleToggle = useCallback((path: string) => {
    setTreeData((prev) => {
      // 如果目录还没加载，先加载再展开
      const target = findNode(prev, path);
      if (target && !target.loaded) {
        // 异步加载，加载完后展开
        loadChildren(path).then(() => {
          setTreeData((p) => toggleExpand(p, path));
        });
        return prev;
      }
      return toggleExpand(prev, path);
    });
  }, [loadChildren]);

  // 查找节点
  function findNode(nodes: FileTreeNode[], key: string): FileTreeNode | undefined {
    for (const n of nodes) {
      if (n.path === key) return n;
      if (n.children) {
        const found = findNode(n.children, key);
        if (found) return found;
      }
    }
    return undefined;
  }

  // 初始加载根目录
  useEffect(() => {
    loadChildren("");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 切换项目时重置文件树、标签和编辑器
  useEffect(() => {
    setTreeData([
      {
        name: project.name,
        path: "",
        isDir: true,
        loaded: false,
        expanded: true,
      },
    ]);
    setTabs([]);
    setActivePath(null);
    setError(null);
    setViewMode("view");
    setClipboard(null);
    loadChildren("");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.id]);

  /** 打开文件（已有标签则直接激活；未保存的内容随标签保留，无需确认丢弃） */
  const openFile = useCallback(
    async (path: string) => {
      const existing = tabs.find((t) => t.path === path);
      if (existing) {
        setActivePath(path);
        return;
      }
      setLoading(true);
      setError(null);

      const filename = path.split("/").pop() ?? path;
      const fileType = getFileType(filename);

      try {
        if (fileType === "image") {
          // 图片：获取绝对路径后通过 Tauri asset 协议展示
          const absPath = await api.getFileAbsPath(project.id, path);
          setTabs((prev) => [
            ...prev,
            {
              path,
              fileType,
              content: "",
              assetUrl: convertFileSrc(absPath),
              meta: {
                content: "",
                encoding: "binary",
                readonly: true,
                size: 0,
              },
            },
          ]);
          setActivePath(path);
        } else if (fileType === "binary") {
          // 二进制文件：不读取文本内容，只展示大小提示
          let size = 0;
          try {
            const meta = await api.readProjectFile(project.id, path);
            size = meta.size;
          } catch {
            /* 二进制可能无法按文本读取，属正常情况 */
          }
          const absPath = await api.getFileAbsPath(project.id, path);
          setTabs((prev) => [
            ...prev,
            {
              path,
              fileType,
              content: "",
              assetUrl: convertFileSrc(absPath),
              meta: { content: "", encoding: "binary", readonly: true, size },
            },
          ]);
          setActivePath(path);
        } else {
          // 文本文件
          const meta = await api.readProjectFile(project.id, path);
          const buf = toBufferEol(meta.content);
          setTabs((prev) => [
            ...prev,
            {
              path,
              fileType,
              content: buf.content,
              meta: {...meta, content: buf.content},
              eol: buf.eol,
            },
          ]);
          setActivePath(path);
        }
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [project.id, tabs]
  );

  /** 关闭标签：脏标签需确认放弃修改 */
  const closeTab = useCallback(
    (path: string) => {
      const idx = tabs.findIndex((t) => t.path === path);
      if (idx < 0) return;
      const tab = tabs[idx]!;
      const doClose = () => {
        setTabs((prev) => prev.filter((t) => t.path !== path));
        if (activePath === path) {
          // 激活相邻标签（优先右侧，其次左侧）
          const next =
            tabs[idx + 1]?.path ?? tabs[idx - 1]?.path ?? null;
          setActivePath(next);
        }
      };
      if (tab.content !== tab.meta.content) {
        modal.confirm({
          title: "放弃未保存的修改？",
          content: `${tab.path} 有未保存的修改，关闭后将丢失。`,
          okText: "放弃并关闭",
          okButtonProps: { danger: true },
          cancelText: "取消",
          onOk: doClose,
        });
      } else {
        doClose();
      }
    },
    [tabs, activePath, modal]
  );

  /**
   * 批量关闭标签（标签右键菜单「关闭其他 / 右侧 / 全部」复用）：
   * 脏标签合并为一次确认，避免逐个弹窗。关闭后的激活顺序：
   * 当前激活未关 > keepPath（发起菜单的标签）> 与被关标签最相邻的幸存者（同距偏右）
   */
  const closeMany = useCallback(
    (targets: string[], keepPath?: string | null) => {
      const all = tabsRef.current;
      const tset = new Set(targets);
      const remaining = all.filter((t) => !tset.has(t.path));
      if (remaining.length === all.length) return;
      const dirtyN = all.reduce(
        (n, t) =>
          tset.has(t.path) && t.content !== t.meta.content ? n + 1 : n,
        0
      );
      let next: string | null = null;
      if (activePath && !tset.has(activePath)) next = activePath;
      else if (keepPath && !tset.has(keepPath)) next = keepPath;
      else if (remaining.length > 0) {
        const idxOf = new Map(all.map((t, i) => [t.path, i] as const));
        let bestD = Infinity;
        let bestIdx = -1;
        for (const r of remaining) {
          const ri = idxOf.get(r.path)!;
          for (const c of targets) {
            const d = Math.abs(ri - idxOf.get(c)!);
            if (d < bestD || (d === bestD && ri > bestIdx)) {
              bestD = d;
              bestIdx = ri;
              next = r.path;
            }
          }
        }
      }
      const doClose = () => {
        setTabs(remaining);
        setActivePath(next);
      };
      if (dirtyN > 0) {
        modal.confirm({
          title: "放弃未保存的修改？",
          content: `有 ${dirtyN} 个标签有未保存的修改，关闭后将丢失。`,
          okText: "放弃并关闭",
          okButtonProps: { danger: true },
          cancelText: "取消",
          onOk: doClose,
        });
      } else {
        doClose();
      }
    },
    [activePath, modal]
  );

  /** 更新当前激活标签的内容 */
  const updateActiveContent = useCallback(
    (content: string) => {
      setTabs((prev) =>
        prev.map((t) => (t.path === activePath ? { ...t, content } : t))
      );
    },
    [activePath]
  );

  const handleSave = useCallback(async () => {
    if (!activeTab || activeTab.meta.readonly) return;
    setSaving(true);
    try {
      // CRLF 文件按磁盘原样还原换行，缓冲区内归一化的 LF 不落盘
      const out =
        activeTab.eol === "\r\n"
          ? activeTab.content.replace(/\n/g, "\r\n")
          : activeTab.content;
      await api.writeProjectFile(project.id, activeTab.path, out);
      const size = new Blob([activeTab.content]).size;
      setTabs((prev) =>
        prev.map((t) =>
          t.path === activeTab.path
            ? { ...t, meta: { ...t.meta, content: t.content, size } }
            : t
        )
      );
      message.success("已保存");
      // 保存会改变工作区状态，刷新文件树 / 标签的 git 标记与行级 diff
      scheduleGitRefresh();
      setDiffRev((v) => v + 1);
    } catch (e: any) {
      message.error(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  }, [activeTab, project.id, message, scheduleGitRefresh]);

  const dirty = useMemo(
    () => !!activeTab && activeTab.content !== activeTab.meta.content,
    [activeTab]
  );

  // ================================================================
  // 行级 diff 标记（HEAD 内容 vs 当前编辑器内容）
  // 前端 LCS 直接对显示中的内容计算：标记位置永远与编辑器行号精确对齐，
  // 不受未保存编辑 / 外部修改 / 暂存状态影响；忽略行尾 CR 与 Git 面板 diff 同口径
  // ================================================================

  useEffect(() => {
    if (!activeTab || activeTab.fileType !== "text" || !project.git_available) {
      setLineKinds(null);
      return;
    }
    void diffRev;
    let cancelled = false;
    // 防抖：编辑时停止输入 400ms 后才计算
    const timer = window.setTimeout(async () => {
      try {
        const lines = activeTab.content.split("\n");
        if (activeTab.content.length > 400_000 || lines.length > 6000) {
          setLineKinds(null);
          return;
        }
        const head = await api.gitFileHead(project.id, activeTab.path);
        if (cancelled) return;
        if (head.suppress) {
          // ignored / skip-worktree / assume-unchanged：git status 不会展示其差异，
          // 编辑器同样不标，保证与 Git 面板一致
          setLineKinds(null);
          return;
        }
        if (head.head === null) {
          // 未跟踪 / HEAD 中不存在：整个文件标记为新增
          setLineKinds(lines.map(() => 2 as LineKind));
          return;
        }
        const kinds = diffLineKinds(head.head, activeTab.content);
        setLineKinds(kinds.some((k) => k !== 0) ? kinds : null);
      } catch {
        if (!cancelled) setLineKinds(null);
      }
    }, 400);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
    // 仅依赖原始字段：activeTab 对象随输入每击重建，避免无谓的重跑
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeTab?.path,
    activeTab?.content,
    activeTab?.fileType,
    project.id,
    project.git_available,
    diffRev,
  ]);

  // 滚动同步：diff 条 / 行号栏随代码区垂直滚动平移
  const syncGutter = useCallback((top: number) => {
    if (gutterInnerRef.current) {
      gutterInnerRef.current.style.transform = `translateY(${-top}px)`;
    }
    if (lnGutterRef.current) {
      lnGutterRef.current.style.transform = `translateY(${-top}px)`;
    }
    // 搜索命中层需双向平移，由 syncGutter 处理（含水平方向）
    if (matchLayerRef.current) {
      const sc = editorInputRef.current ?? readonlyPreRef.current;
      const sl = sc ? sc.scrollLeft : 0;
      matchLayerRef.current.style.transform = `translate(${-sl}px, ${-top}px)`;
    }
  }, []);

  useEffect(() => {
    syncGutter(0);
  }, [activePath, lineKinds, syncGutter]);

  /** 打开浮层并异步拉取该文件的提交历史（--follow，跟随重命名） */
  const openHist = (lineIdx: number) => {
    const path = activeTab?.path;
    if (!path) return;
    setHistAnchor(lineIdx);
    setRollbackArm(null);
    setHistPreviewHash(null);
    setHistPreviewText("");
    setHistErr(null);
    setHistCommits([]);
    setHistLoading(true);
    api
      .gitFileLog(project.id, path)
      .then((cs) => setHistCommits(cs))
      .catch((e) => setHistErr(String(e)))
      .finally(() => setHistLoading(false));
  };

  // 回滚确认 3 秒未点击自动复位；切文件 / 关浮层同样复位
  useEffect(() => {
    if (!rollbackArm) return;
    const t = window.setTimeout(() => setRollbackArm(null), 3000);
    return () => window.clearTimeout(t);
  }, [rollbackArm]);
  useEffect(() => setRollbackArm(null), [activePath]);

  /** 展开/收起某个提交的文件内容预览 */
  const toggleHistPreview = async (hash: string) => {
    const path = activeTab?.path;
    if (!path || !project.git_available) return;
    if (histPreviewHash === hash) {
      setHistPreviewHash(null);
      return;
    }
    setHistPreviewHash(hash);
    setHistPreviewLoading(true);
    setHistPreviewText("");
    try {
      setHistPreviewText(await api.gitShowFile(project.id, hash, path));
    } catch (e) {
      message.error(`读取历史版本失败: ${e}`);
      setHistPreviewHash(null);
    } finally {
      setHistPreviewLoading(false);
    }
  };

  /**
   * 整文件回滚到指定提交：`git show hash:path` 的内容原样写回磁盘
   * （保留该版本的原始换行），并同步当前标签缓冲区、清除 dirty 标记。
   * 当前未保存编辑会被覆盖——由「确认回滚」二次确认兜底。
   */
  const rollbackToFile = async (hash: string) => {
    const path = activeTab?.path;
    if (!path) return;
    try {
      const raw = await api.gitShowFile(project.id, hash, path);
      await api.writeProjectFile(project.id, path, raw);
      const buf = toBufferEol(raw);
      const size = new Blob([raw]).size;
      setTabs((prev) =>
        prev.map((t) =>
          t.path === path
            ? {
                ...t,
                content: buf.content,
                meta: {...t.meta, content: buf.content, size},
                eol: buf.eol,
              }
            : t
        )
      );
      const short = histCommits.find((c) => c.hash === hash)?.short_hash ?? "";
      message.success(`已回滚到 ${short}`);
      setHistAnchor(null);
      setDiffRev((v) => v + 1);
      scheduleGitRefresh();
    } catch (e) {
      message.error(`回滚失败: ${e}`);
    }
  };

  /** 左缘 diff 条（绿=新增行，橙=修改行，红=行删除点）；left 为行号栏宽度偏移。
   *  标记条可点击：弹出文件历史 / 回滚浮层 */
  const renderDiffGutter = (left: number) => {
    if (!lineKinds || !lineKinds.some((k) => k !== 0)) return null;
    const bars: React.ReactNode[] = [];
    for (let idx = 0; idx < lineKinds.length; idx++) {
      const k = lineKinds[idx]!;
      if (k === 0) continue;
      bars.push(
        <div
          key={idx}
          className={`diff-bar ${k === 2 ? "add" : k === 3 ? "del" : "mod"}`}
          style={{ top: GUTTER_PAD + idx * LINE_H }}
          title="查看文件历史 / 回滚"
          onClick={(e) => {
            e.stopPropagation();
            openHist(idx);
          }}
        />
      );
    }
    return (
      <div className="file-diff-gutter" role="group" aria-label="git 变更标记" style={{ left }}>
        <div className="file-diff-gutter-inner" ref={gutterInnerRef}>
          {bars}
        </div>
      </div>
    );
  };

  /** 文件历史 / 回滚浮层：锚在触发 diff 条附近，列出该文件提交历史，
   *  支持展开预览历史内容与整文件回滚（交互类似 IDEA 的行标记历史） */
  const renderHistPopover = () => {
    if (histAnchor === null || !activeTab) return null;
    const top = Math.max(
      8,
      Math.min(GUTTER_PAD + histAnchor * LINE_H - 10, window.innerHeight * 0.4)
    );
    const name = activeTab.path.replace(/\\/g, "/").split("/").pop();
    return (
      <div className="git-hist-pop" style={{top}} onClick={(e) => e.stopPropagation()}>
        <div className="git-hist-head">
          <span className="git-hist-title" title={activeTab.path}>
            文件历史 · {name}
          </span>
          <button
            className="icon-btn sm"
            aria-label="关闭历史"
            onClick={() => setHistAnchor(null)}
          >
            ✕
          </button>
        </div>
        <div className="git-hist-list">
          {histLoading && <div className="git-hist-empty">加载中…</div>}
          {!histLoading && histErr && (
            <div className="git-hist-empty">{histErr}</div>
          )}
          {!histLoading && !histErr && histCommits.length === 0 && (
            <div className="git-hist-empty">暂无提交历史</div>
          )}
          {histCommits.map((c) => (
            <div
              key={c.hash}
              className={`git-hist-item${histPreviewHash === c.hash ? " active" : ""}`}
            >
              <button
                className="git-hist-msg"
                title={c.message}
                onClick={() => void toggleHistPreview(c.hash)}
              >
                {c.message || "(无提交信息)"}
              </button>
              <div className="git-hist-meta">
                <code>{c.short_hash}</code>
                <span>{c.author}</span>
                <span>{c.date.slice(0, 10)}</span>
                <span className="git-hist-actions">
                  {rollbackArm === c.hash ? (
                    <>
                      <a
                        className="git-hist-danger"
                        onClick={() => void rollbackToFile(c.hash)}
                      >
                        确认回滚
                      </a>
                      <a onClick={() => setRollbackArm(null)}>取消</a>
                    </>
                  ) : (
                    <a
                      className="git-hist-danger"
                      onClick={() => setRollbackArm(c.hash)}
                    >
                      回滚到此版本
                    </a>
                  )}
                </span>
              </div>
            </div>
          ))}
        </div>
        {histPreviewHash && (
          <pre className="git-hist-preview">
            {histPreviewLoading
              ? "加载中…"
              : histPreviewText.length > 200_000
                ? `${histPreviewText.slice(0, 200_000)}\n…（预览已截断）`
                : histPreviewText}
          </pre>
        )}
      </div>
    );
  };

  // ================================================================
  // 行号栏 / 编辑器内搜索
  // ================================================================

  /** 活动文本文件总行数 */
  const activeLines = useMemo(
    () =>
      activeTab && activeTab.fileType === "text"
        ? activeTab.content.split("\n").length
        : 0,
    [activeTab?.content, activeTab?.fileType]
  );

  const isMdPreview =
    !!activeTab &&
    activeTab.fileType === "text" &&
    isMarkdown(activeTab.path) &&
    (activeTab.meta.readonly || viewMode === "view");

  /** 行号栏显示条件：文本文件、≤1 万行、非 Markdown 预览 */
  const showLineNumbers =
    !!activeTab &&
    activeTab.fileType === "text" &&
    activeLines > 0 &&
    activeLines <= 10000 &&
    !isMdPreview;

  /** 行号栏宽度（右对齐数字 + 右侧间距） */
  const lnWidth = showLineNumbers
    ? Math.max(2, String(activeLines).length) * charW + 18
    : 0;

  // 等宽字符宽实测：行号宽与搜索高亮 x 定位都依赖它
  useEffect(() => {
    const el = editorInputRef.current ?? readonlyPreRef.current;
    if (!el) return;
    try {
      const cs = getComputedStyle(el);
      const ctx = document.createElement("canvas").getContext("2d");
      if (!ctx) return;
      ctx.font = `${cs.fontStyle} ${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
      const w = ctx.measureText("0").width;
      if (w > 0) setCharW(w);
    } catch {
      /* 测量失败沿用默认值 */
    }
  }, [activePath, viewMode, activeTab?.meta.readonly]);

  interface SearchHit {
    line: number;
    visCol: number;
    len: number;
  }

  /** 内容超上限（>2MB 只读大文件）时搜索禁用，输入框提示而非静默"无匹配" */
  const searchTooLarge =
    !!activeTab &&
    activeTab.fileType === "text" &&
    activeTab.content.length > SEARCH_LIMIT;

  const searchHits = useMemo<SearchHit[]>(() => {
    const src = searchOpen ? activeTab?.content : undefined;
    if (!src || !searchQuery || src.length > SEARCH_LIMIT) return [];
    const hay = src.toLowerCase();
    const needle = searchQuery.toLowerCase();
    const lineStarts: number[] = [0];
    for (let i = 0; i < src.length; i++) {
      if (src[i] === "\n") lineStarts.push(i + 1);
    }
    // 制表符按 tab-size=2 展开的可视宽度：段 [a, b) 自可视基点 v0 起累计
    const visWidth = (a: number, b: number, v0: number): number => {
      let v = v0;
      for (let k = a; k < b; k++) {
        v += src.charCodeAt(k) === 9 ? 2 - (v % 2) : 1;
      }
      return v;
    };
    const res: SearchHit[] = [];
    let from = 0;
    // 命中按位置递增：同行命中从上一命中处增量展开可视列，
    // 避免 minified 单行大文件上 O(命中数 × 行首距离) 的重复回走（最坏 O(n²)）
    let prevAt = -1;
    let prevSegStart = -1;
    let prevV = 0;
    while (res.length < 2000) {
      const at = hay.indexOf(needle, from);
      if (at < 0) break;
      let lo = 0;
      let hi = lineStarts.length - 1;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (lineStarts[mid]! <= at) lo = mid;
        else hi = mid - 1;
      }
      const segStart = lineStarts[lo]!;
      const v =
        prevAt >= 0 && prevSegStart === segStart
          ? visWidth(prevAt, at, prevV)
          : visWidth(segStart, at, 0);
      res.push({line: lo, visCol: v, len: needle.length});
      prevAt = at;
      prevSegStart = segStart;
      prevV = v;
      from = at + Math.max(needle.length, 1);
    }
    return res;
  }, [searchOpen, searchQuery, activeTab?.content]);

  // 命中列表变化时收敛当前索引
  useEffect(() => {
    setMatchIdx((i) => (searchHits.length ? i % searchHits.length : 0));
  }, [searchHits.length]);

  /** 滚动到指定命中（垂直取最近可视位置，水平按需露出） */
  const gotoHit = useCallback(
    (idx: number) => {
      const m = searchHits[idx];
      const sc = editorInputRef.current ?? readonlyPreRef.current;
      if (!m || !sc) return;
      const top = GUTTER_PAD + m.line * LINE_H;
      const left = lnWidth + m.visCol * charW;
      const ch = sc.clientHeight;
      const cw = sc.clientWidth;
      let st = sc.scrollTop;
      if (top < st + GUTTER_PAD) st = Math.max(0, top - GUTTER_PAD);
      else if (top + LINE_H > st + ch - GUTTER_PAD)
        st = top - ch + LINE_H + GUTTER_PAD;
      sc.scrollTop = st;
      let sl = sc.scrollLeft;
      if (left < sl + GUTTER_PAD) sl = Math.max(0, left - GUTTER_PAD);
      else if (left + m.len * charW > sl + cw - GUTTER_PAD)
        sl = left - cw + m.len * charW + GUTTER_PAD;
      sc.scrollLeft = sl;
    },
    [searchHits, lnWidth, charW]
  );

  useEffect(() => {
    gotoHit(matchIdx);
  }, [matchIdx, gotoHit]);

  const stepHit = useCallback(
    (dir: number) => {
      if (!searchHits.length) return;
      setMatchIdx(
        (i) => (i + dir + searchHits.length) % searchHits.length
      );
    },
    [searchHits.length]
  );

  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setSearchQuery("");
    setMatchIdx(0);
    editorInputRef.current?.focus();
  }, []);

  // ================================================================
  // 快速打开（Ctrl+P）：项目级文件名 / 路径过滤跳转
  // ================================================================
  const [quickOpen, setQuickOpen] = useState(false);
  const [quickQuery, setQuickQuery] = useState("");
  const [quickIdx, setQuickIdx] = useState(0);
  // 全量扁平文件列表；每次打开弹层都后台重拉，杜绝树操作后的陈旧索引
  const [flatFiles, setFlatFiles] = useState<api.FlatFile[] | null>(null);
  const quickListRef = useRef<HTMLDivElement>(null);

  /** 拉取全量文件列表；失败时保留上次结果（退化为旧数据可用） */
  const loadFlatFiles = useCallback(async () => {
    try {
      const files = await api.walkFiles(project.id);
      setFlatFiles(files);
    } catch {
      /* 保留上一次列表 */
    }
  }, [project.id]);

  /**
   * 过滤与排序（小写不区分大小写）：文件名前缀 > 文件名包含（位置越靠前越优）
   * > 路径包含。空查询展示前 50 个当浏览列表。全量收集后排序取前 100——
   * 过滤本身就要扫全部条目，截断省不出成本，只换掉排序正确性。
   */
  const quickResults = useMemo<QuickHit[]>(() => {
    if (!flatFiles) return [];
    const q = quickQuery.trim().toLowerCase();
    if (!q) {
      return flatFiles.slice(0, 50).map((f) => ({
        path: f.path,
        name: f.name,
        dir: dirOf(f.path),
        score: 0,
      }));
    }
    const out: QuickHit[] = [];
    for (const f of flatFiles) {
      const pl = f.path.toLowerCase();
      const slash = pl.lastIndexOf("/");
      const nl = slash < 0 ? pl : pl.slice(slash + 1);
      let score: number | null;
      if (nl.startsWith(q)) {
        score = 0;
      } else {
        const ni = nl.indexOf(q);
        if (ni >= 0) {
          score = 1 + Math.min(ni, 999) / 1000;
        } else {
          const pi = pl.indexOf(q);
          score = pi >= 0 ? 10 + Math.min(pi, 999) / 1000 : null;
        }
      }
      if (score !== null) {
        out.push({path: f.path, name: f.name, dir: dirOf(f.path), score});
      }
    }
    out.sort(
      (a, b) =>
        a.score - b.score ||
        a.path.length - b.path.length ||
        a.path.localeCompare(b.path)
    );
    return out.slice(0, 100);
  }, [flatFiles, quickQuery]);

  // 结果集变化时收敛选中下标
  useEffect(() => {
    setQuickIdx((i) =>
      quickResults.length ? Math.min(i, quickResults.length - 1) : 0
    );
  }, [quickResults.length]);

  // 选中项滚动进可视区
  useEffect(() => {
    quickListRef.current
      ?.querySelector(".quick-open-item.sel")
      ?.scrollIntoView({block: "nearest"});
  }, [quickIdx]);

  const openQuickOpen = useCallback(() => {
    setQuickOpen(true);
    setQuickQuery("");
    setQuickIdx(0);
    void loadFlatFiles();
  }, [loadFlatFiles]);

  const closeQuickOpen = useCallback(() => {
    setQuickOpen(false);
    setQuickQuery("");
    setQuickIdx(0);
  }, []);

  /** 回车 / 点击：打开选中文件并收起弹层 */
  const acceptQuick = useCallback(() => {
    const hit = quickResults[quickIdx];
    if (!hit) return;
    void openFile(hit.path);
    closeQuickOpen();
  }, [quickResults, quickIdx, openFile, closeQuickOpen]);

  // Ctrl+P 全局唤起；Esc 在输入框内 stopPropagation 关闭，不外溢
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        !e.shiftKey &&
        e.key.toLowerCase() === "p"
      ) {
        e.preventDefault();
        e.stopPropagation();
        openQuickOpen();
      } else if (e.key === "Escape" && quickOpen) {
        closeQuickOpen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openQuickOpen, quickOpen, closeQuickOpen]);

  // Ctrl+F 打开编辑器搜索（全局默认查找已在 main.tsx 拦截）；Esc 关闭。
  // 快速打开弹层存在时不响应，避免按键穿透到被遮罩的编辑器
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "f") {
        if (
          !quickOpen &&
          activeTab &&
          activeTab.fileType === "text" &&
          !isMdPreview
        ) {
          e.preventDefault();
          setSearchOpen(true);
          requestAnimationFrame(() => searchInputRef.current?.select());
        }
      } else if (e.key === "Escape" && searchOpen) {
        closeSearch();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activeTab, isMdPreview, searchOpen, closeSearch, quickOpen]);

  /** 搜索命中高亮层（绝对定位半透明色块；padLeft = 行号栏宽 + 内容左 padding） */
  const renderHitLayer = (padLeft: number) => {
    if (!searchOpen || searchHits.length === 0) return null;
    return (
      <div className="search-hit-layer" ref={matchLayerRef} aria-hidden>
        {searchHits.map((m, i) => (
          <span
            key={i}
            className={`search-hit${i === matchIdx ? " cur" : ""}`}
            style={{
              top: GUTTER_PAD + m.line * LINE_H + 3,
              left: padLeft + m.visCol * charW,
              width: Math.max(4, m.len * charW),
            }}
          />
        ))}
      </div>
    );
  };

  /** 文件树节点 → git 状态查找 */
  const statusFor = useCallback(
    (node: FileTreeNode): { kind?: FileGitStatus; agg?: DirGitAgg } =>
      node.isDir
        ? { agg: dirAgg.get(node.path) }
        : { kind: statusMap.get(node.path) },
    [dirAgg, statusMap]
  );

  const statsFor = useCallback(
    (path: string) => lineStats.get(path),
    [lineStats]
  );

  // ================================================================
  // 文件操作：重命名 / 复制 / 剪切 / 粘贴 / 资源管理器 / 拖拽移动
  // ================================================================

  /** 重命名 / 移动后同步更新打开的标签路径（含子路径前缀改写） */
  const remapTabPaths = useCallback(
    (oldPath: string, newPath: string) => {
      setTabs((prev) =>
        prev.map((t) => {
          if (t.path === oldPath) return { ...t, path: newPath };
          if (t.path.startsWith(oldPath + "/")) {
            return { ...t, path: newPath + t.path.slice(oldPath.length) };
          }
          return t;
        })
      );
      setActivePath((cur) => {
        if (cur === oldPath) return newPath;
        if (cur && cur.startsWith(oldPath + "/")) {
          return newPath + cur.slice(oldPath.length);
        }
        return cur;
      });
    },
    []
  );

  const parentOf = (p: string) => {
    const idx = p.lastIndexOf("/");
    return idx < 0 ? "" : p.slice(0, idx);
  };

  const doRename = useCallback(async () => {
    if (!renaming) return;
    const name = renaming.name.trim();
    if (!name) return;
    try {
      const newPath = await api.fsRename(project.id, renaming.path, name);
      const parent = parentOf(renaming.path);
      await refreshDir(parent);
      remapTabPaths(renaming.path, newPath);
      scheduleGitRefresh();
      message.success("已重命名");
      setRenaming(null);
    } catch (e: any) {
      message.error(`重命名失败: ${e}`);
    }
  }, [renaming, project.id, refreshDir, remapTabPaths, message, scheduleGitRefresh]);

  const handlePaste = useCallback(
    async (targetDir: string) => {
      if (!clipboard) return;
      const src = clipboard.path;
      // 目标不能是自身或其子目录（后端同样校验，前端提前拦截给友好提示）
      if (targetDir === src || targetDir.startsWith(src + "/")) {
        message.warning("不能粘贴到条目自身内部");
        return;
      }
      const isCut = clipboard.mode === "cut";
      try {
        const newPath = isCut
          ? await api.fsMoveEntry(project.id, src, targetDir)
          : await api.fsCopyEntry(project.id, src, targetDir);
        const targets = new Set<string>([targetDir]);
        if (isCut) targets.add(parentOf(src));
        for (const t of targets) void refreshDir(t);
        if (isCut) {
          remapTabPaths(src, newPath);
          setClipboard(null);
        }
        scheduleGitRefresh();
        message.success(isCut ? "已移动" : "已粘贴");
      } catch (e: any) {
        message.error(`${isCut ? "移动" : "粘贴"}失败: ${e}`);
      }
    },
    [clipboard, project.id, refreshDir, remapTabPaths, message, scheduleGitRefresh]
  );

  /** 拖拽放下：把拖动条目移动到目标目录 */
  const handleDropInto = useCallback(
    async (targetDir: string) => {
      const src = draggingPath;
      setDraggingPath(null);
      if (!src || src === targetDir) return;
      if (targetDir.startsWith(src + "/")) {
        message.warning("不能移动到自身内部");
        return;
      }
      const sameParent = parentOf(src) === targetDir;
      if (sameParent) return;
      try {
        const newPath = await api.fsMoveEntry(project.id, src, targetDir);
        const targets = new Set<string>([targetDir, parentOf(src)]);
        for (const t of targets) void refreshDir(t);
        remapTabPaths(src, newPath);
        scheduleGitRefresh();
        message.success("已移动");
      } catch (e: any) {
        message.error(`移动失败: ${e}`);
      }
    },
    [draggingPath, project.id, refreshDir, remapTabPaths, message, scheduleGitRefresh]
  );

  // ---- 可拖拽分隔条 ----
  const startDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    draggingRef.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, []);

  // ---- 终端抽屉高度拖拽 ----
  const startTermResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    termResizingRef.current = true;
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (draggingRef.current && panelBodyRef.current) {
        const rect = panelBodyRef.current.getBoundingClientRect();
        const newWidth = Math.max(160, Math.min(600, e.clientX - rect.left));
        setTreeWidth(newWidth);
      }
      if (termResizingRef.current) {
        const h = window.innerHeight - e.clientY - 40;
        const next = Math.max(120, Math.min(window.innerHeight * 0.7, h));
        setTermHeight(next);
      }
    };
    const onUp = () => {
      if (draggingRef.current) {
        draggingRef.current = false;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        setTreeWidth((w) => {
          localStorage.setItem("jb_file_tree_width", String(w));
          return w;
        });
      }
      if (termResizingRef.current) {
        termResizingRef.current = false;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
        setTermHeight((h) => {
          localStorage.setItem("jb_term_height", String(h));
          return h;
        });
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, []);

  const dnd: DndHandlers = {
    draggingPath,
    onDragStart: setDraggingPath,
    onDragEnd: () => setDraggingPath(null),
    onDropInto: (target) => void handleDropInto(target),
  };

  return (
    <div className="file-panel">
      <div className="file-panel-header">
        <span className="file-panel-title">
          <Folder size={14} style={{ color: "#0071e3" }} />
          {project.name}
          <span className="file-panel-sub">文件</span>
        </span>
        <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
          {project.git_available && (
            <Tooltip title="Git 工作区">
              <button
                className="icon-btn sm"
                onClick={() => onOpenGit(project)}
                aria-label="Git 工作区"
              >
                <GitBranch size={13} />
              </button>
            </Tooltip>
          )}
          {activeTab && activeTab.fileType === "text" && (
            <>
              <span className="file-enc-badge">{activeTab.meta.encoding}</span>
              {activeTab.meta.readonly && (
                <span className="file-enc-badge readonly">只读</span>
              )}
              <Tooltip title="保存 (Ctrl+S)">
                <button
                  className="icon-btn sm accent"
                  onClick={handleSave}
                  disabled={!dirty || activeTab.meta.readonly}
                  aria-label="保存"
                >
                  <Save size={13} />
                </button>
              </Tooltip>
            </>
          )}
          <Tooltip title="终端">
            <button
              className={`icon-btn sm ${termOpen ? "accent" : ""}`}
              onClick={() => setTermOpen((v) => !v)}
              aria-label="终端"
            >
              <Terminal size={13} />
            </button>
          </Tooltip>
          <Tooltip title="返回日志">
            <button
              className="icon-btn sm"
              onClick={onClose}
              aria-label="返回日志"
            >
              <ChevronLeft size={13} />
            </button>
          </Tooltip>
        </div>
      </div>

      <div className="file-panel-body" ref={panelBodyRef}>
        {/* 左侧文件树（自定义，点击整行展开/折叠；支持右键菜单与拖拽移动） */}
        <div className="file-tree" style={{ width: treeWidth, flexShrink: 0 }}>
          {treeData.map((node) => (
            <TreeRow
              key={node.path}
              node={node}
              depth={0}
              activePath={activePath}
              onSelect={openFile}
              onToggle={handleToggle}
              onContextMenu={(e, path, isDir) => {
                e.preventDefault();
                e.stopPropagation();
                setCtxMenu({ x: e.clientX, y: e.clientY, path, isDir });
              }}
              dnd={dnd}
              statusFor={statusFor}
              statsFor={statsFor}
            />
          ))}
        </div>

        {/* 可拖拽分隔条 */}
        <div
          className="file-tree-resizer"
          onMouseDown={startDrag}
        />

        {/* 右侧编辑器/预览区 */}
        <div className="file-editor">
          {/* 已打开文件的标签栏 */}
          {tabs.length > 0 && (
            <div className="file-tabs">
              {tabs.map((t) => {
                const tabDirty = t.content !== t.meta.content;
                const tKind = statusMap.get(t.path);
                return (
                  <div
                    key={t.path}
                    className={`file-tab ${t.path === activePath ? "active" : ""}`}
                    onClick={() => setActivePath(t.path)}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      setTabCtx({ x: e.clientX, y: e.clientY, path: t.path });
                    }}
                    title={t.path}
                  >
                    <FileTypeIcon
                      name={t.path.split("/").pop() ?? ""}
                      size={12}
                    />
                    <span
                      className={`file-tab-name ${tKind ? `gs-${tKind}` : ""}`}
                    >
                      {t.path.split("/").pop()}
                    </span>
                    {tabDirty ? (
                      <span className="file-tab-dirty" />
                    ) : tKind ? (
                      <span
                        className={`file-tree-dot dot-${tKind}`}
                        title={statusLabel(tKind)}
                      />
                    ) : null}
                    <button
                      className="file-tab-close"
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(t.path);
                      }}
                      aria-label="关闭标签"
                    >
                      <X size={10} />
                    </button>
                  </div>
                );
              })}
            </div>
          )}

          {loading ? (
            <div style={{ padding: 40, textAlign: "center" }}>
              <Spin />
            </div>
          ) : activeTab && activeTab.fileType === "image" && activeTab.assetUrl ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{activeTab.path}</span>
                <span className="file-type-badge image">图片</span>
              </div>
              <div className="file-image-preview">
                <img
                  src={activeTab.assetUrl}
                  alt={activeTab.path}
                  style={{ maxWidth: "100%", maxHeight: "100%", objectFit: "contain" }}
                  onError={() => {
                    setError("图片加载失败");
                  }}
                />
              </div>
            </>
          ) : activeTab && activeTab.fileType === "binary" ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{activeTab.path}</span>
                <span className="file-type-badge binary">二进制</span>
              </div>
              <div className="file-binary-hint">
                <Binary size={40} />
                <div>这是一个二进制文件，不支持在线预览</div>
                <div style={{ fontSize: 11, color: "var(--text-4)" }}>
                  {activeTab.meta.size.toLocaleString()} B
                </div>
              </div>
            </>
          ) : activeTab ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{activeTab.path}</span>
                {activeGitKind && (
                  <span className={`git-badge g-${activeGitKind}`}>
                    {statusLabel(activeGitKind)}
                  </span>
                )}
                {(() => {
                  const s = activeTab
                    ? lineStats.get(activeTab.path)
                    : undefined;
                  return s && (s.a > 0 || s.d > 0) ? (
                    <span
                      className="tree-lines editor-lines"
                      title={`新增 ${s.a} 行 / 删除 ${s.d} 行`}
                    >
                      {s.a > 0 && <b className="la">+{s.a}</b>}
                      {s.d > 0 && <b className="ld">-{s.d}</b>}
                    </span>
                  ) : null;
                })()}
                <div
                  style={{
                    marginLeft: "auto",
                    display: "flex",
                    gap: 10,
                    alignItems: "center",
                  }}
                >
                  {activeTab.fileType === "text" &&
                    isMarkdown(activeTab.path) &&
                    !activeTab.meta.readonly && (
                      <Segmented
                        size="small"
                        value={viewMode}
                        onChange={(v) => setViewMode(v as "view" | "edit")}
                        options={[
                          { label: "预览", value: "view" },
                          { label: "编辑", value: "edit" },
                        ]}
                      />
                    )}
                  <span className="file-editor-size">
                    {activeTab.meta.size.toLocaleString()} B
                  </span>
                </div>
              </div>
              {isMarkdown(activeTab.path) &&
              (activeTab.meta.readonly || viewMode === "view") ? (
                <div className="file-preview-scroll">
                  <div className="file-preview-markdown">
                    <ReactMarkdown
                      remarkPlugins={[remarkGfm]}
                      components={{
                        code({className, children, ...props}) {
                          const match = /language-(\w+)/.exec(className ?? "");
                          if (match) {
                            const lang = match[1] ?? "";
                            const grammar = Prism.languages[lang];
                            const raw = Array.isArray(children)
                              ? children.join("")
                              : String(children ?? "");
                            const html = grammar
                              ? Prism.highlight(raw, grammar, lang)
                              : escapeHtml(raw);
                            return (
                              <code
                                className={`${className ?? ""} file-md-code`}
                                dangerouslySetInnerHTML={{ __html: html }}
                                {...props}
                              />
                            );
                          }
                          return (
                            <code className={className} {...props}>
                              {children}
                            </code>
                          );
                        },
                      }}
                    >
                      {activeTab.content}
                    </ReactMarkdown>
                  </div>
                </div>
              ) : activeTab.meta.readonly ? (
                <div className="file-code-wrap" onClick={() => setHistAnchor(null)}>
                  {renderDiffGutter(lnWidth)}
                  {showLineNumbers && (
                    <div className="file-ln-gutter" aria-hidden style={{width: lnWidth}}>
                      <div className="file-ln-inner" ref={lnGutterRef} style={{paddingTop: GUTTER_PAD}}>
                        {Array.from({length: activeLines}, (_, i) => (
                          <div key={i} className="file-ln">{i + 1}</div>
                        ))}
                      </div>
                    </div>
                  )}
                  <pre
                    ref={readonlyPreRef}
                    className="file-code-view"
                    style={{paddingLeft: 16 + lnWidth}}
                    onScroll={(e) => syncGutter(e.currentTarget.scrollTop)}
                    dangerouslySetInnerHTML={{
                      __html: highlightCode(
                        activeTab.content,
                        getPrismLang(activeTab.path)
                      ),
                    }}
                  />
                  {renderHitLayer(16 + lnWidth)}
                  {renderHistPopover()}
                </div>
              ) : (
                <div className="file-editor-overlay" onClick={() => setHistAnchor(null)}>
                  {/* 左缘 git diff 标记条 */}
                  {renderDiffGutter(lnWidth)}
                  {showLineNumbers && (
                    <div className="file-ln-gutter" aria-hidden style={{width: lnWidth}}>
                      <div className="file-ln-inner" ref={lnGutterRef} style={{paddingTop: GUTTER_PAD}}>
                        {Array.from({length: activeLines}, (_, i) => (
                          <div key={i} className="file-ln">{i + 1}</div>
                        ))}
                      </div>
                    </div>
                  )}
                  {/* 高亮底层：随内容实时渲染语法颜色 */}
                  <pre
                    ref={editorUnderlayRef}
                    className="file-code-view file-code-underlay"
                    aria-hidden
                    style={{paddingLeft: 12 + lnWidth}}
                    dangerouslySetInnerHTML={{
                      __html: highlightCode(
                        // 末尾换行时补一个空格，保证底层行数与输入层一致
                        activeTab.content.endsWith("\n")
                          ? activeTab.content + " "
                          : activeTab.content,
                        getPrismLang(activeTab.path)
                      ),
                    }}
                  />
                  {renderHitLayer(12 + lnWidth)}
                  {/* 输入层：文字透明，仅显示光标与选区 */}
                  <textarea
                    ref={editorInputRef}
                    className="file-editor-textarea file-editor-input"
                    value={activeTab.content}
                    style={{paddingLeft: 12 + lnWidth}}
                    onChange={(e) => updateActiveContent(e.target.value)}
                    onScroll={(e) => {
                      const el = e.currentTarget;
                      const under = editorUnderlayRef.current;
                      if (under) {
                        under.scrollTop = el.scrollTop;
                        under.scrollLeft = el.scrollLeft;
                      }
                      syncGutter(el.scrollTop);
                    }}
                    wrap="off"
                    spellCheck={false}
                    onKeyDown={(e) => {
                      if ((e.ctrlKey || e.metaKey) && e.key === "s") {
                        e.preventDefault();
                        handleSave();
                      }
                    }}
                  />
                  {renderHistPopover()}
                </div>
              )}
            </>
          ) : error ? (
            <div style={{ padding: 40, textAlign: "center", color: "#ff3b30" }}>
              {error}
            </div>
          ) : (
            <div className="file-editor-empty">
              <File size={40} />
              <div>从左侧文件树选择文件进行预览 / 编辑</div>
            </div>
          )}
          {searchOpen && activeTab?.fileType === "text" && !isMdPreview && (
            <div className="editor-search">
              <input
                ref={searchInputRef}
                className="editor-search-input"
                value={searchQuery}
                onChange={(e) => {
                  setSearchQuery(e.target.value);
                  setMatchIdx(0);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    stepHit(e.shiftKey ? -1 : 1);
                  } else if (e.key === "Escape") {
                    e.stopPropagation();
                    closeSearch();
                  }
                }}
                placeholder="搜索内容…"
                spellCheck={false}
                autoComplete="off"
              />
              <span
                className={`editor-search-count ${searchQuery && !searchHits.length ? "none" : ""}`}
                title={
                  searchTooLarge ? "文件超过 2MB，已禁用内容搜索" : undefined
                }
              >
                {searchQuery
                  ? searchTooLarge
                    ? "文件过大"
                    : searchHits.length
                      ? `${matchIdx + 1}/${searchHits.length}`
                      : "无匹配"
                  : ""}
              </span>
              <button
                className="icon-btn sm"
                onClick={() => stepHit(-1)}
                disabled={!searchHits.length}
                aria-label="上一个匹配"
              >
                <CaretDown size={12} style={{transform: "rotate(180deg)"}} />
              </button>
              <button
                className="icon-btn sm"
                onClick={() => stepHit(1)}
                disabled={!searchHits.length}
                aria-label="下一个匹配"
              >
                <CaretDown size={12} />
              </button>
              <button
                className="icon-btn sm"
                onClick={closeSearch}
                aria-label="关闭搜索"
              >
                <X size={12} />
              </button>
            </div>
          )}
          {saving && (
            <div className="file-saving">
              <Spin size="small" />
            </div>
          )}
        </div>
      </div>

      {/* 集成终端抽屉 */}
      <div
        className={`term-drawer ${termOpen ? "open" : ""}`}
        style={{ height: termOpen ? termHeight : undefined }}
      >
        {termOpen && <div className="term-resizer" onMouseDown={startTermResize} />}
        <button
          className={`term-drawer-bar ${termOpen ? "open" : ""}`}
          onClick={() => setTermOpen((v) => !v)}
          aria-label="切换终端"
        >
          <CaretRight
            size={11}
            style={{
              transform: termOpen ? "rotate(90deg)" : undefined,
              transition: "transform 0.2s var(--ease)",
            }}
          />
          <Terminal size={12} />
          <span>终端</span>
          <span className="term-cwd">{project.root_path}</span>
        </button>
        {termOpen && (
          <div className="term-drawer-content">
            <TerminalView projectId={project.id} />
          </div>
        )}
      </div>

      {/* 快速打开（Ctrl+P）：遮罩点击关闭，弹层置顶 */}
      {quickOpen && (
        <>
          <div className="quick-open-mask" onClick={closeQuickOpen} />
          <div className="quick-open">
            <input
              autoFocus
              className="quick-open-input"
              value={quickQuery}
              onChange={(e) => {
                setQuickQuery(e.target.value);
                setQuickIdx(0);
              }}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setQuickIdx((i) =>
                    quickResults.length ? (i + 1) % quickResults.length : 0
                  );
                } else if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setQuickIdx((i) =>
                    quickResults.length
                      ? (i - 1 + quickResults.length) % quickResults.length
                      : 0
                  );
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  acceptQuick();
                } else if (e.key === "Escape") {
                  e.stopPropagation();
                  closeQuickOpen();
                }
              }}
              placeholder="输入文件名或路径过滤，回车打开…"
              spellCheck={false}
              autoComplete="off"
            />
            <div className="quick-open-list" ref={quickListRef}>
              {!flatFiles ? (
                <div className="quick-open-empty">
                  <Spin size="small" />
                </div>
              ) : quickResults.length === 0 ? (
                <div className="quick-open-empty">无匹配文件</div>
              ) : (
                quickResults.map((h, i) => (
                  <div
                    key={h.path}
                    className={`quick-open-item${i === quickIdx ? " sel" : ""}`}
                    onMouseEnter={() => setQuickIdx(i)}
                    onClick={acceptQuick}
                  >
                    <FileTypeIcon name={h.name} size={13} />
                    <span className="quick-open-name">{h.name}</span>
                    {h.dir && (
                      <span className="quick-open-dir" title={h.dir}>
                        {h.dir}
                      </span>
                    )}
                  </div>
                ))
              )}
            </div>
            <div className="quick-open-hint">
              <span>↑↓ 选择</span>
              <span>Enter 打开</span>
              <span>Esc 关闭</span>
            </div>
          </div>
        </>
      )}

      {/* 文件树右键菜单 */}
      {ctxMenu && (
        <Dropdown
          open={true}
          trigger={["contextMenu"]}
          onOpenChange={(open) => {
            if (!open) setCtxMenu(null);
          }}
          menu={{
            items: [
              {
                key: "rename",
                icon: <Edit size={13} />,
                label: "重命名",
                onClick: () => {
                  const name = ctxMenu.path.split("/").pop() ?? "";
                  setRenaming({ path: ctxMenu.path, name });
                  setCtxMenu(null);
                },
              },
              {
                key: "copy",
                icon: <Copy size={13} />,
                label: "复制",
                onClick: () => {
                  setClipboard({ mode: "copy", path: ctxMenu.path });
                  message.info("已复制，选择目录后粘贴");
                  setCtxMenu(null);
                },
              },
              {
                key: "cut",
                icon: <Scissors size={13} />,
                label: "剪切",
                onClick: () => {
                  setClipboard({ mode: "cut", path: ctxMenu.path });
                  message.info("已剪切，选择目录后粘贴");
                  setCtxMenu(null);
                },
              },
              {
                key: "paste",
                icon: <ClipboardPaste size={13} />,
                label: ctxMenu.isDir ? "粘贴到该目录" : "粘贴到所在目录",
                disabled:
                  !clipboard ||
                  (!ctxMenu.isDir && parentOf(ctxMenu.path) === clipboard.path),
                onClick: () => {
                  void handlePaste(
                    ctxMenu.isDir ? ctxMenu.path : parentOf(ctxMenu.path)
                  );
                  setCtxMenu(null);
                },
              },
              { type: "divider" as const },
              {
                key: "reveal",
                icon: <FolderOpen size={13} />,
                label: "在文件管理器中显示",
                onClick: () => {
                  api.revealInFileManager(project.id, ctxMenu.path).catch((e) =>
                    message.error(`打开文件管理器失败: ${e}`)
                  );
                  setCtxMenu(null);
                },
              },
            ],
          }}
        >
          <div
            style={{
              position: "fixed",
              left: ctxMenu.x,
              top: ctxMenu.y,
              // 锚点必须非 0 尺寸：0x0 的 fixed 元素会被 rc-trigger 判定为
              // 不可见（offsetParent 为 null 且宽高为 0），导致弹层永不对齐
              width: 1,
              height: 1,
              opacity: 0,
              pointerEvents: "none",
            }}
          />
        </Dropdown>
      )}

      {/* 标签页右键菜单 */}
      {tabCtx && (
        <Dropdown
          open={true}
          trigger={["contextMenu"]}
          onOpenChange={(open) => {
            if (!open) setTabCtx(null);
          }}
          menu={{
            items: [
              {
                key: "close",
                icon: <X size={13} />,
                label: "关闭",
                onClick: () => {
                  closeMany([tabCtx.path]);
                  setTabCtx(null);
                },
              },
              {
                key: "closeOthers",
                label: "关闭其他",
                disabled: tabs.length <= 1,
                onClick: () => {
                  closeMany(
                    tabsRef.current
                      .filter((t) => t.path !== tabCtx.path)
                      .map((t) => t.path),
                    tabCtx.path
                  );
                  setTabCtx(null);
                },
              },
              {
                key: "closeRight",
                label: "关闭右侧标签",
                disabled:
                  tabs.findIndex((t) => t.path === tabCtx.path) ===
                  tabs.length - 1,
                onClick: () => {
                  const idx = tabsRef.current.findIndex(
                    (t) => t.path === tabCtx.path
                  );
                  closeMany(
                    tabsRef.current.slice(idx + 1).map((t) => t.path),
                    tabCtx.path
                  );
                  setTabCtx(null);
                },
              },
              {
                key: "closeAll",
                label: "全部关闭",
                onClick: () => {
                  closeMany(tabsRef.current.map((t) => t.path));
                  setTabCtx(null);
                },
              },
              { type: "divider" as const },
              {
                key: "copyPath",
                icon: <Copy size={13} />,
                label: "复制路径",
                onClick: () => {
                  navigator.clipboard
                    .writeText(tabCtx.path)
                    .then(() => message.success("路径已复制"))
                    .catch(() => message.error("复制失败"));
                  setTabCtx(null);
                },
              },
              {
                key: "rename",
                icon: <Edit size={13} />,
                label: "重命名",
                onClick: () => {
                  const name = tabCtx.path.split("/").pop() ?? "";
                  setRenaming({ path: tabCtx.path, name });
                  setTabCtx(null);
                },
              },
              {
                key: "reveal",
                icon: <FolderOpen size={13} />,
                label: "在文件管理器中显示",
                onClick: () => {
                  api.revealInFileManager(project.id, tabCtx.path).catch((e) =>
                    message.error(`打开文件管理器失败: ${e}`)
                  );
                  setTabCtx(null);
                },
              },
            ],
          }}
        >
          <div
            style={{
              position: "fixed",
              left: tabCtx.x,
              top: tabCtx.y,
              width: 1,
              height: 1,
              opacity: 0,
              pointerEvents: "none",
            }}
          />
        </Dropdown>
      )}

      {/* 重命名弹窗 */}
      <Modal
        open={!!renaming}
        title="重命名"
        okText="确定"
        cancelText="取消"
        width={380}
        destroyOnClose
        onOk={() => void doRename()}
        onCancel={() => setRenaming(null)}
      >
        <Input
          value={renaming?.name ?? ""}
          autoFocus
          placeholder="输入新名称"
          onChange={(e) =>
            setRenaming((r) => (r ? { ...r, name: e.target.value } : r))
          }
          onPressEnter={() => void doRename()}
        />
      </Modal>
    </div>
  );
}
