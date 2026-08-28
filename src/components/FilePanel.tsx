import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {App, Dropdown, Input, Modal, Segmented, Spin, Tooltip} from "antd";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {convertFileSrc} from "@tauri-apps/api/core";
import {isMarkdown} from "../languages";
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
  Image as ImageIcon,
  Save,
  Scissors,
  Terminal,
  X,
} from "./Icons";
import * as api from "../api";
import type {FileContent, Project} from "../types";
import TerminalView from "./TerminalView";
import MonacoCodeEditor from "./MonacoCodeEditor";
import type {MonacoCodeEditorHandle} from "./MonacoCodeEditor";

interface Props {
  project: Project;
  onClose: () => void;
  /** 面板是否可见（重新可见时刷新文件树） */
  visible?: boolean;
}

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

/** 把磁盘原始内容归一化为 LF 缓冲区 + 记录原 EOL 风格 */
function toBufferEol(raw: string): { content: string; eol: "\n" | "\r\n" } {
  if (raw.includes("\r\n")) {
    return { content: raw.replace(/\r\n?/g, "\n"), eol: "\r\n" };
  }
  return { content: raw.replace(/\r/g, "\n"), eol: "\n" };
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
}: {
  node: FileTreeNode;
  depth: number;
  activePath: string | null;
  onSelect: (path: string) => void;
  onToggle: (path: string) => void;
  onContextMenu: (e: React.MouseEvent, path: string, isDir: boolean) => void;
  dnd: DndHandlers;
}) {
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
          <span className="file-tree-icon folder">
            <Folder size={14} />
          </span>
          <span className="file-tree-name">{node.name}</span>
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
      <span className="file-tree-name">{node.name}</span>
    </div>
  );
}

// ================================================================
// 主组件
// ================================================================
export default function FilePanel({
  project,
  onClose,
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
  // 文本/标记文件的查看模式：仅 Markdown 使用（预览 ↔ 编辑）；
  // 其他文本文件无需切换，直接在单页内编辑（只读文件静态展示）
  const [viewMode, setViewMode] = useState<"view" | "edit">("view");

  // ---- Monaco 编辑器 ref（暴露 getValue/setValue/revealLine/getEditor） ----
  const monacoRef = useRef<MonacoCodeEditorHandle | null>(null);

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

  // 切换激活标签时回到查看模式
  useEffect(() => {
    setViewMode("view");
  }, [activePath]);

  /**
   * 外部修改同步：把无未保存编辑的文本标签页从磁盘静默重读。
   * 聚焦/可见刷新时一并重读，避免缓冲区停留在外部修改前。
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

  // 面板重新可见 / 切换项目时同步外部修改
  useEffect(() => {
    if (visible) {
      void syncCleanTabsFromDisk();
    }
  }, [visible, syncCleanTabsFromDisk]);

  // 窗口重新聚焦时同步外部修改（覆盖在 IDE 等外部工具中编辑后切回的场景）
  useEffect(() => {
    if (!visible) return;
    const onFocus = () => {
      void syncCleanTabsFromDisk();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [visible, syncCleanTabsFromDisk]);

  // 懒加载目录子节点（含紧凑路径合并）
  const loadChildren = useCallback(async (path: string) => {
    // 标记目标节点 loading
    setTreeData((prev) => markLoading(prev, path));
    try {
      const nodes = await loadMergedNodes(project.id, path);
      setTreeData((prev) => setChildren(prev, path, nodes));
    } catch (e) {
      message.error(`加载目录失败: ${api.toErrMsg(e)}`);
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
          setTabs((prev) => {
            // 防重复标签：若已在异步等待期间被添加则跳过
            if (prev.some((t) => t.path === path)) return prev;
            return [
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
            ];
          });
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
          setTabs((prev) => {
            if (prev.some((t) => t.path === path)) return prev;
            return [
              ...prev,
              {
                path,
                fileType,
                content: "",
                assetUrl: convertFileSrc(absPath),
                meta: { content: "", encoding: "binary", readonly: true, size },
              },
            ];
          });
          setActivePath(path);
        } else {
          // 文本文件
          const meta = await api.readProjectFile(project.id, path);
          const buf = toBufferEol(meta.content);
          setTabs((prev) => {
            if (prev.some((t) => t.path === path)) return prev;
            return [
              ...prev,
              {
                path,
                fileType,
                content: buf.content,
                meta: {...meta, content: buf.content},
                eol: buf.eol,
              },
            ];
          });
          setActivePath(path);
        }
      } catch (e) {
        setError(api.toErrMsg(e));
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
    } catch (e) {
      message.error(`保存失败: ${api.toErrMsg(e)}`);
    } finally {
      setSaving(false);
    }
  }, [activeTab, project.id, message]);

  const dirty = useMemo(
    () => !!activeTab && activeTab.content !== activeTab.meta.content,
    [activeTab]
  );


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
      message.success("已重命名");
      setRenaming(null);
    } catch (e) {
      message.error(`重命名失败: ${api.toErrMsg(e)}`);
    }
  }, [renaming, project.id, refreshDir, remapTabPaths, message]);

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
        message.success(isCut ? "已移动" : "已粘贴");
      } catch (e) {
        message.error(`${isCut ? "移动" : "粘贴"}失败: ${api.toErrMsg(e)}`);
      }
    },
    [clipboard, project.id, refreshDir, remapTabPaths, message]
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
        message.success("已移动");
} catch (e) {
      message.error(`移动失败: ${api.toErrMsg(e)}`);
      }
    },
    [draggingPath, project.id, refreshDir, remapTabPaths, message]
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
                    <span className="file-tab-name">
                      {t.path.split("/").pop()}
                    </span>
                    {tabDirty ? <span className="file-tab-dirty" /> : null}
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
                          return (
                            <code className={`${className ?? ""} file-md-code`} {...props}>
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
              ) : (
                <div className="file-code-wrap" style={{position: "relative", height: "100%", overflow: "hidden"}}>
                  <MonacoCodeEditor
                    path={activeTab.path}
                    value={activeTab.content}
                    readonly={activeTab.meta.readonly}
                    eol={activeTab.eol ?? "\n"}
                    onChange={updateActiveContent}
                    onSave={handleSave}
                    editorRef={monacoRef}
                  />
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
