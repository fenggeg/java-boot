import { useState } from "react";
import { Spin } from "antd";
import {
  Binary,
  CaretDown,
  CaretRight,
  File,
  Folder,
  Image as ImageIcon,
} from "./Icons";
import type { DndHandlers, FileTreeNode } from "./filePanelUtils";
import { getFileType } from "./filePanelUtils";

/** 按文件名渲染类型图标（文件树 / 标签栏 / 快速打开共用） */
export function FileTypeIcon({name, size}: {name: string; size: number}) {
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

/** 单个树行（递归组件）— 根据文件类型显示不同图标；支持右键菜单与拖拽移动 */
export function TreeRow({
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
