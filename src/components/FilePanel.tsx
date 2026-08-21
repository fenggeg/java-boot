import {useMemo, useRef, useState, useCallback, useEffect} from "react";
import {App, Spin, Tooltip} from "antd";
import {CaretDown, CaretRight, ChevronLeft, File, Folder, Save} from "./Icons";
import * as api from "../api";
import type {FileContent, FileEntry, Project} from "../types";

interface Props {
  project: Project;
  onClose: () => void;
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

/** FileEntry → FileTreeNode（目录标记 loaded=false，文件无 children） */
function toNodes(entries: FileEntry[]): FileTreeNode[] {
  // 目录在前，文件在后；同类按名称排序
  const sorted = [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
  return sorted.map((e) => ({
    name: e.name,
    path: e.path,
    isDir: e.is_dir,
    loaded: !e.is_dir,
  }));
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
// 单个树行（递归组件）
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

  return (
    <div
      className={`file-tree-row file ${isSelected ? "active" : ""}`}
      style={{ paddingLeft: 8 + depth * 14 }}
      onClick={() => onSelect(node.path)}
    >
      <span className="file-tree-caret" />
      <span className="file-tree-icon file">
        <File size={14} />
      </span>
      <span className="file-tree-name">{node.name}</span>
    </div>
  );
}

// ================================================================
// 主组件
// ================================================================
export default function FilePanel({ project, onClose }: Props) {
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
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  // 懒加载目录子节点
  const loadChildren = useCallback(async (path: string) => {
    // 标记目标节点 loading
    setTreeData((prev) => markLoading(prev, path));
    try {
      const entries = await api.listFiles(project.id, path);
      setTreeData((prev) => setChildren(prev, path, toNodes(entries)));
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
    try {
      const meta = await api.readProjectFile(project.id, path);
      setDoc({ path, meta });
      setContent(meta.content);
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
          {doc && (
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

        {/* 右侧编辑器 */}
        <div className="file-editor">
          {loading ? (
            <div style={{ padding: 40, textAlign: "center" }}>
              <Spin />
            </div>
          ) : doc ? (
            <>
              <div className="file-editor-toolbar">
                <span className="file-editor-path">{doc.path}</span>
                <span className="file-editor-size">
                  {doc.meta.size.toLocaleString()} B
                </span>
              </div>
              <textarea
                className="file-editor-textarea"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                readOnly={doc.meta.readonly}
                spellCheck={false}
                onKeyDown={(e) => {
                  if ((e.ctrlKey || e.metaKey) && e.key === "s") {
                    e.preventDefault();
                    handleSave();
                  }
                }}
                placeholder={
                  doc.meta.readonly
                    ? "该文件为只读（非 UTF-8 编码或文件过大）"
                    : undefined
                }
              />
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
