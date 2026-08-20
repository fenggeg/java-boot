import {useCallback, useEffect, useMemo, useState} from "react";
import {App, Button, Empty, Modal, Segmented, Spin, Tooltip} from "antd";
import {Check, Edit, X} from "./Icons";
import * as api from "../api";
import type {GitChange} from "../types";
import {gitChangeKind} from "../types";

interface Props {
  projectId: string;
  change: GitChange | null;
  onClose: () => void;
  /** 文件被编辑/暂存后回调，让父级刷新 status */
  onChanged: () => void;
}

/** 简单按行着色的 diff 渲染（+ 绿 / - 红 / 元信息灰 / hunk 蓝） */
function DiffText({ text }: { text: string }) {
  const lines = useMemo(() => text.split("\n"), [text]);
  return (
    <div className="diff-view">
      {lines.map((l, i) => {
        let cls = "diff-line";
        if (l.startsWith("+") && !l.startsWith("+++")) cls += " add";
        else if (l.startsWith("-") && !l.startsWith("---")) cls += " del";
        else if (l.startsWith("@")) cls += " hunk";
        else if (
          /^(diff |index |--- |\+\+\+ )/.test(l) ||
          l.startsWith("new file") ||
          l.startsWith("deleted file")
        )
          cls += " meta";
        return (
          <div key={i} className={cls}>
            <span className="diff-content">{l || " "}</span>
          </div>
        );
      })}
    </div>
  );
}

export default function GitDiffModal({
  projectId,
  change,
  onClose,
  onChanged,
}: Props) {
  const { message } = App.useApp();
  const [diffText, setDiffText] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [staged, setStaged] = useState(false);
  const [editing, setEditing] = useState(false);
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);

  const isUntracked = change ? gitChangeKind(change) === "untracked" : false;
  const isDeleted = change ? gitChangeKind(change) === "deleted" : false;

  const loadDiff = useCallback(
    async (target: GitChange, useStaged: boolean) => {
      setLoading(true);
      setError(null);
      try {
        if (gitChangeKind(target) === "untracked") {
          const c = await api.gitReadFile(projectId, target.path);
          setDiffText(c);
        } else {
          const d = await api.gitDiff(projectId, target.path, useStaged);
          setDiffText(d || "(无差异)");
        }
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [projectId]
  );

  // 打开 / 切换文件时初始化并加载 diff
  useEffect(() => {
    if (!change) return;
    setStaged(change.staged);
    setEditing(false);
    setError(null);
    setDiffText("");
    loadDiff(change, change.staged);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [change, projectId]);

  const toggleStaged = (v: string | number | boolean) => {
    const s = v === "staged";
    setStaged(s);
    if (change) loadDiff(change, s);
  };

  const startEdit = async () => {
    if (!change) return;
    try {
      const c = await api.gitReadFile(projectId, change.path);
      setContent(c);
      setEditing(true);
    } catch (e: any) {
      message.error(`读取文件失败: ${e}`);
    }
  };

  const saveEdit = async () => {
    if (!change) return;
    setSaving(true);
    try {
      await api.gitWriteFile(projectId, change.path, content);
      message.success("已保存到工作区");
      setEditing(false);
      // 保存后可能产生新的 unstaged diff，重取并通知父级刷新 status
      const d = await api.gitDiff(projectId, change.path, false);
      setDiffText(d || "(无差异)");
      onChanged();
    } catch (e: any) {
      message.error(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const kind = change ? gitChangeKind(change) : null;
  const title = change
    ? `${isUntracked ? "新增文件" : kind === "deleted" ? "已删除" : "文件差异"} · ${change.path}`
    : "";

  return (
    <Modal
      open={!!change}
      onCancel={onClose}
      title={title}
      width={860}
      footer={null}
      destroyOnClose
      style={{ top: 40 }}
    >
      {loading && !diffText && !error ? (
        <div style={{ padding: 60, textAlign: "center" }}>
          <Spin />
        </div>
      ) : error ? (
        <Empty description={error} />
      ) : (
        <>
          {change && !isUntracked && !isDeleted && (
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                marginBottom: 10,
              }}
            >
              <Segmented
                size="small"
                value={staged ? "staged" : "worktree"}
                onChange={toggleStaged}
                options={[
                  { label: "工作区改动", value: "worktree" },
                  { label: "已暂存改动", value: "staged" },
                ]}
              />
              <Tooltip title="在编辑器中打开该文件（工作区）">
                <Button
                  size="small"
                  icon={<Edit size={13} />}
                  onClick={startEdit}
                  disabled={saving}
                >
                  编辑文件
                </Button>
              </Tooltip>
            </div>
          )}

          {editing ? (
            <div className="diff-editor">
              <div className="diff-editor-toolbar">
                <span className="diff-editor-path">{change?.path}</span>
                <span style={{ display: "flex", gap: 6 }}>
                  <Button
                    size="small"
                    icon={<X size={12} />}
                    onClick={() => setEditing(false)}
                    disabled={saving}
                  >
                    取消
                  </Button>
                  <Button
                    size="small"
                    type="primary"
                    icon={<Check size={12} />}
                    onClick={saveEdit}
                    loading={saving}
                  >
                    保存
                  </Button>
                </span>
              </div>
              <textarea
                className="diff-editor-textarea"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                spellCheck={false}
                disabled={saving}
              />
            </div>
          ) : (
            <div style={{ maxHeight: "60vh", overflow: "auto" }}>
              <DiffText text={diffText} />
            </div>
          )}
        </>
      )}
    </Modal>
  );
}