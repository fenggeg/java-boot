import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {App, Segmented, Spin, Tooltip} from "antd";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {convertFileSrc} from "@tauri-apps/api/core";
import {Binary, CaretDown, CaretRight, ChevronLeft, File, Folder, GitBranch, Image as ImageIcon, Save,} from "./Icons";
import {Prism} from "../prism-langs";
import {getPrismLang, isMarkdown} from "../languages";
import * as api from "../api";
import type {FileContent, Project} from "../types";

interface Props {
  project: Project;
  onClose: () => void;
  /** 打开同项目的 Git 工作区面板 */
  onOpenGit: (project: Project) => void;
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

// ================================================================
// 单个树行（递归组件）— 根据文件类型显示不同图标
// ================================================================
function TreeRow({
  node,
  depth,
  selectedPath,
  onSelect,
  onToggle,
}: {
  node: FileTreeNode;
  depth: number;
  selectedPath: string | null;
  onSelect: (path: string) => void;
  onToggle: (path: string) => void;
}) {
  const isSelected = selectedPath === node.path;
  const isExpanded = !!node.expanded;

  if (node.isDir) {
    return (
      <>
        <div
          className={`file-tree-row ${isSelected ? "active" : ""}`}
          style={{ paddingLeft: 8 + depth * 14 }}
          onClick={() => onToggle(node.path)}
        >
          <span className="file-tree-caret">
            {node.loading ? (
              <Spin size="small" style={{ transform: "scale(0.6)" }} />
            ) : isExpanded ? (
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
        {isExpanded && node.children && node.children.length > 0 && (
          <div className="file-tree-children">
            {node.children.map((child) => (
              <TreeRow
                key={child.path}
                node={child}
                depth={depth + 1}
                selectedPath={selectedPath}
                onSelect={onSelect}
                onToggle={onToggle}
              />
            ))}
          </div>
        )}
        {isExpanded && node.children && node.children.length === 0 && (
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

  const fileType = getFileType(node.name);

  return (
    <div
      className={`file-tree-row file ${isSelected ? "active" : ""}`}
      style={{ paddingLeft: 8 + depth * 14 }}
      onClick={() => onSelect(node.path)}
    >
      <span className="file-tree-caret" />
      <span
        className={`file-tree-icon file ${fileType === "image" ? "file-type-image" : fileType === "binary" ? "file-type-binary" : ""}`}
      >
        {fileType === "image" ? (
          <ImageIcon size={14} />
        ) : fileType === "binary" ? (
          <Binary size={14} />
        ) : (
          <File size={14} />
        )}
      </span>
      <span className="file-tree-name">{node.name}</span>
    </div>
  );
}

// ================================================================
// 主组件
// ================================================================
export default function FilePanel({ project, onClose, onOpenGit }: Props) {
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
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [doc, setDoc] = useState<{ path: string; meta: FileContent } | null>(
    null
  );
  const [content, setContent] = useState("");
  const [loading, setLoading] = useState(false);
  // 编辑态语法高亮底层（与输入层滚动同步）
  const editorUnderlayRef = useRef<HTMLPreElement>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 图片预览 URL
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  // 当前文件的类型（已打开后）
  const [currentFileType, setCurrentFileType] = useState<FileType | null>(null);
  // 文本/标记文件的查看模式：view=高亮/预览，edit=编辑
  const [viewMode, setViewMode] = useState<"view" | "edit">("view");

  // 可拖拽宽度：文件树宽度持久化到 localStorage
  const [treeWidth, setTreeWidth] = useState<number>(() => {
    const saved = localStorage.getItem("jb_file_tree_width");
    return saved ? parseInt(saved, 10) || 240 : 240;
  });
  const draggingRef = useRef(false);
  const panelBodyRef = useRef<HTMLDivElement>(null);

  const dirty = useMemo(
    () => doc !== null && content !== doc.meta.content,
    [doc, content]
  );

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
  }, [loadChildren]);

  // 切换项目时重置文件树、选中文件和编辑器
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
    setSelectedPath(null);
    setDoc(null);
    setContent("");
    setError(null);
    setImageUrl(null);
    setCurrentFileType(null);
    setViewMode("view");
    loadChildren("");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project.id]);

  const openFile = async (path: string) => {
    if (dirty) {
      const ok = await new Promise<boolean>((resolve) => {
        modal.confirm({
          title: "放弃未保存的修改？",
          content: `${doc?.path} 有未保存的修改，切换后将丢失。`,
          okText: "放弃并打开",
          cancelText: "取消",
          onOk: () => resolve(true),
          onCancel: () => resolve(false),
        });
      });
      if (!ok) return;
    }
    setLoading(true);
    setError(null);
    setSelectedPath(path);
    setImageUrl(null);
    setDoc(null);
    setCurrentFileType(null);
    setViewMode("view");

    const filename = path.split("/").pop() ?? path;
    const fileType = getFileType(filename);
    setCurrentFileType(fileType);

    try {
      if (fileType === "image") {
        // 图片：获取绝对路径后通过 Tauri asset 协议展示
        const absPath = await api.getFileAbsPath(project.id, path);
        const url = convertFileSrc(absPath);
        setImageUrl(url);
      } else if (fileType === "binary") {
        // 二进制文件：不读取内容，直接显示提示
        const absPath = await api.getFileAbsPath(project.id, path);
        setImageUrl(convertFileSrc(absPath)); // 复用变量名但不会用于显示图片
        // 获取文件大小
        try {
          const meta = await api.readProjectFile(project.id, path);
          setDoc({ path, meta });
        } catch {
          // 二进制文件可能无法读取为文本，这是正常的
          setDoc({ path, meta: { content: "", encoding: "binary", readonly: true, size: 0 } });
        }
      } else {
        // 文本文件：走原有的读取流程
        const meta = await api.readProjectFile(project.id, path);
        setDoc({ path, meta });
        setContent(meta.content);
      }
    } catch (e: any) {
      setError(String(e));
      setDoc(null);
      setContent("");
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    if (!doc || doc.meta.readonly) return;
    setSaving(true);
    try {
      await api.writeProjectFile(project.id, doc.path, content);
      setDoc({
        path: doc.path,
        meta: { ...doc.meta, content, size: new Blob([content]).size },
      });
      message.success("已保存");
    } catch (e: any) {
      message.error(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  // ---- 可拖拽分隔条 ----
  const startDrag = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    draggingRef.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!draggingRef.current || !panelBodyRef.current) return;
      const rect = panelBodyRef.current.getBoundingClientRect();
      const newWidth = Math.max(160, Math.min(600, e.clientX - rect.left));
      setTreeWidth(newWidth);
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
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, []);

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
          {doc && currentFileType === "text" && (
            <>
              <span className="file-enc-badge">{doc.meta.encoding}</span>
              {doc.meta.readonly && (
                <span className="file-enc-badge readonly">只读</span>
              )}
              <Tooltip title="保存 (Ctrl+S)">
                <button
                  className="icon-btn sm accent"
                  onClick={handleSave}
                  disabled={!dirty || doc.meta.readonly}
                  aria-label="保存"
                >
                  <Save size={13} />
                </button>
              </Tooltip>
            </>
          )}
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
        {/* 左侧文件树（自定义，点击整行展开/折叠） */}
        <div className="file-tree" style={{ width: treeWidth, flexShrink: 0 }}>
          {treeData.map((node) => (
            <TreeRow
              key={node.path}
              node={node}
              depth={0}
              selectedPath={selectedPath}
              onSelect={openFile}
              onToggle={handleToggle}
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
          {loading ? (
            <div style={{ padding: 40, textAlign: "center" }}>
              <Spin />
            </div>
          ) : currentFileType === "image" && imageUrl ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{selectedPath}</span>
                <span className="file-type-badge image">图片</span>
              </div>
              <div className="file-image-preview">
                <img
                  src={imageUrl}
                  alt={selectedPath ?? ""}
                  style={{ maxWidth: "100%", maxHeight: "100%", objectFit: "contain" }}
                  onError={() => {
                    setError("图片加载失败");
                    setImageUrl(null);
                  }}
                />
              </div>
            </>
          ) : currentFileType === "binary" ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{selectedPath}</span>
                <span className="file-type-badge binary">二进制</span>
              </div>
              <div className="file-binary-hint">
                <Binary size={40} />
                <div>这是一个二进制文件，不支持在线预览</div>
                <div style={{ fontSize: 11, color: "var(--text-4)" }}>
                  {doc?.meta.size.toLocaleString() ?? ""} B
                </div>
              </div>
            </>
          ) : doc ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{doc.path}</span>
                <div
                  style={{
                    marginLeft: "auto",
                    display: "flex",
                    gap: 10,
                    alignItems: "center",
                  }}
                >
                  {currentFileType === "text" && (
                    <Segmented
                      size="small"
                      value={viewMode}
                      onChange={(v) => setViewMode(v as "view" | "edit")}
                      options={
                        doc.meta.readonly
                          ? [
                              {
                                label: isMarkdown(doc.path) ? "预览" : "查看",
                                value: "view",
                              },
                            ]
                          : isMarkdown(doc.path)
                            ? [
                                { label: "预览", value: "view" },
                                { label: "编辑", value: "edit" },
                              ]
                            : [
                                { label: "查看", value: "view" },
                                { label: "编辑", value: "edit" },
                              ]
                      }
                    />
                  )}
                  <span className="file-editor-size">
                    {doc.meta.size.toLocaleString()} B
                  </span>
                </div>
              </div>
              {doc.meta.readonly || viewMode === "view" ? (
                isMarkdown(doc.path) ? (
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
                        {content}
                      </ReactMarkdown>
                    </div>
                  </div>
                ) : (
                  <pre
                    className="file-code-view"
                    dangerouslySetInnerHTML={{
                      __html: highlightCode(
                        content,
                        getPrismLang(doc.path)
                      ),
                    }}
                  />
                )
              ) : (
                <div className="file-editor-overlay">
                  {/* 高亮底层：随内容实时渲染语法颜色 */}
                  <pre
                    ref={editorUnderlayRef}
                    className="file-code-view file-code-underlay"
                    aria-hidden
                    dangerouslySetInnerHTML={{
                      __html: highlightCode(
                        // 末尾换行时补一个空格，保证底层行数与输入层一致
                        content.endsWith("\n") ? content + " " : content,
                        getPrismLang(doc.path)
                      ),
                    }}
                  />
                  {/* 输入层：文字透明，仅显示光标与选区 */}
                  <textarea
                    className="file-editor-textarea file-editor-input"
                    value={content}
                    onChange={(e) => setContent(e.target.value)}
                    onScroll={(e) => {
                      const el = e.currentTarget;
                      const under = editorUnderlayRef.current;
                      if (under) {
                        under.scrollTop = el.scrollTop;
                        under.scrollLeft = el.scrollLeft;
                      }
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
    </div>
  );
}
