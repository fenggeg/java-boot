import {useMemo, useState} from "react";
import type {TreeDataNode} from "antd";
import {App, Spin, Tooltip, Tree} from "antd";
import type {DataNode, EventDataNode} from "antd/es/tree";
import {ChevronLeft, File, Folder, Save} from "./Icons";
import * as api from "../api";
import type {FileContent, Project} from "../types";

interface Props {
  project: Project;
  onClose: () => void;
}

/** 单层目录 → antd Tree 节点（懒加载，目录 isLeaf=false） */
function toTreeNode(entries: Awaited<ReturnType<typeof api.listFiles>>): TreeDataNode[] {
  return entries.map((e) => ({
    title: e.name,
    key: e.path,
    isLeaf: !e.is_dir,
    icon: e.is_dir ? (
      <Folder size={14} style={{ color: "#ff9500" }} />
    ) : (
      <File size={14} />
    ),
  }));
}

/** 不可变更新树：把 key 对应节点的 children 替换为加载结果（antd loadData 官方推荐模式） */
function updateTreeData(
  list: TreeDataNode[],
  key: string,
  children: TreeDataNode[]
): TreeDataNode[] {
  return list.map((node) => {
    if (node.key === key) {
      return { ...node, children };
    }
    if (node.children) {
      return { ...node, children: updateTreeData(node.children, key, children) };
    }
    return node;
  });
}

export default function FilePanel({ project, onClose }: Props) {
  const { message, modal } = App.useApp();
  const [treeData, setTreeData] = useState<TreeDataNode[]>(() => [
    {
      title: project.name,
      key: "",
      isLeaf: false,
      icon: <Folder size={14} style={{ color: "#0071e3" }} />,
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

  const dirty = useMemo(
    () => doc !== null && content !== doc.meta.content,
    [doc, content]
  );

  // 懒加载目录（不可变更新，避免闭包/引用问题）
  const loadData = async (node: EventDataNode<DataNode>) => {
    try {
      const entries = await api.listFiles(project.id, String(node.key));
      setTreeData((prev) => updateTreeData(prev, String(node.key), toTreeNode(entries)));
    } catch (e: any) {
      message.error(`加载目录失败: ${e}`);
    }
  };

  const openFile = async (path: string) => {
    // 有未保存修改时先确认
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

      <div className="file-panel-body">
        {/* 左侧文件树 */}
        <div className="file-tree">
          <Tree
            treeData={treeData}
            loadData={loadData}
            defaultExpandedKeys={[""]}
            selectedKeys={selectedPath ? [selectedPath] : []}
            onSelect={(_, info) => {
              const node = info.node as TreeDataNode;
              if (node.isLeaf) openFile(String(node.key));
            }}
            selectable
            showIcon
            blockNode
          />
        </div>

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