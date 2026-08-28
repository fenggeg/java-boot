import {useCallback, useEffect, useRef, useState} from "react";
import {App, Button, Empty, Modal, Segmented, Spin, Tooltip} from "antd";
import {Check, Edit, X} from "./Icons";
import * as api from "../api";
import type {GitChange} from "../types";
import {gitChangeKind} from "../types";
import MonacoDiffEditor, {type MonacoDiffEditorHandle} from "./MonacoDiffEditor";

interface Props {
  projectId: string;
  change: GitChange | null;
  onClose: () => void;
  /** 文件被编辑/暂存后回调，让父级刷新 status */
  onChanged: () => void;
}

export default function GitDiffModal({
  projectId,
  change,
  onClose,
  onChanged,
}: Props) {
  const { message } = App.useApp();
  const [versions, setVersions] = useState<api.DiffVersions | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [staged, setStaged] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState("");
  const [saving, setSaving] = useState(false);
  const diffRef = useRef<MonacoDiffEditorHandle | null>(null);

  const isUntracked = change ? gitChangeKind(change) === "untracked" : false;
  const isDeleted = change ? gitChangeKind(change) === "deleted" : false;

  const loadDiff = useCallback(
    async (target: GitChange, useStaged: boolean) => {
      setLoading(true);
      setError(null);
      try {
        if (gitChangeKind(target) === "untracked") {
          // 未跟踪文件：original=null（左侧空），modified=工作区文件内容
          const c = await api.gitReadFile(projectId, target.path);
          setVersions({ original: null, modified: c });
        } else {
          const v = await api.gitDiffVersions(projectId, target.path, useStaged);
          setVersions(v);
        }
      } catch (e) {
        setError(api.toErrMsg(e));
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
    setVersions(null);
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
      setEditContent(c);
      setEditing(true);
    } catch (e) {
      message.error(`读取文件失败: ${api.toErrMsg(e)}`);
    }
  };

  const saveEdit = async () => {
    if (!change) return;
    setSaving(true);
    try {
      await api.gitWriteFile(projectId, change.path, editContent);
      message.success("已保存到工作区");
      setEditing(false);
      // 保存后重新加载 diff 并通知父级刷新 status
      await loadDiff(change, false);
      setStaged(false);
      onChanged();
    } catch (e) {
      message.error(`保存失败: ${api.toErrMsg(e)}`);
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
      width={960}
      footer={null}
      destroyOnClose
      style={{ top: 40 }}
    >
      {loading && !versions && !error ? (
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
                  { label: "全部改动(相对上次提交)", value: "worktree" },
                  { label: "已暂存改动", value: "staged" },
                ]}
              />
              {editing ? (
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
              ) : (
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
              )}
            </div>
          )}

          {editing ? (
            <div className="git-diff-editor-wrap">
              <div className="git-diff-editor-path">{change?.path}</div>
              <MonacoDiffEditor
                path={change?.path ?? ""}
                original={versions?.original ?? null}
                modified={editContent}
                editable
                onModifiedChange={setEditContent}
                height="58vh"
              />
            </div>
          ) : (
            <div style={{ height: "58vh" }}>
              {versions && (
                <MonacoDiffEditor
                  path={change?.path ?? ""}
                  original={versions.original}
                  modified={versions.modified}
                  editable={false}
                  diffEditorRef={diffRef}
                  height="100%"
                />
              )}
            </div>
          )}
        </>
      )}
    </Modal>
  );
}
