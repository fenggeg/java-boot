import { useState, useMemo } from "react";
import {
  Modal,
  Input,
  Tree,
  Spin,
  Alert,
  App,
  Typography,
  Button,
  Space,
  Checkbox,
} from "antd";
import type { TreeDataNode } from "antd";
import {
  Folder,
  File,
  FolderOpen,
  CheckSquare,
} from "./Icons";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type { ScannedModule } from "../types";

const { Text } = Typography;

interface Props {
  open: boolean;
  onClose: () => void;
  onAdded: () => void;
}

/** 扁平化扫描树为勾选列表 */
function flatten(modules: ScannedModule[]): ScannedModule[] {
  const out: ScannedModule[] = [];
  for (const m of modules) {
    out.push(m);
    out.push(...flatten(m.children));
  }
  return out;
}

/** 扫描树 → antd Tree 节点（不再副作用修改 checkedSet） */
function toTreeData(modules: ScannedModule[]): TreeDataNode[] {
  return modules.map((m) => {
    const key = m.pom_path;
    const isService = m.is_service;
    const disabled = !isService || m.already_added;

    const title = (
      <span
        style={{
          color: m.already_added
            ? "#626771"
            : isService
            ? "#e6e8eb"
            : "#9ba1a8",
        }}
      >
        {m.artifact_id}
        <Text type="secondary" style={{ fontSize: 11, marginLeft: 8 }}>
          ({m.packaging})
        </Text>
        {m.already_added && (
          <Text type="secondary" style={{ fontSize: 11, marginLeft: 8 }}>
            已添加
          </Text>
        )}
        {!isService && (
          <Text type="secondary" style={{ fontSize: 11, marginLeft: 8 }}>
            聚合模块
          </Text>
        )}
      </span>
    );

    return {
      key,
      title,
      disabled,
      icon: isService ? (
        <File size={14} />
      ) : (
        <Folder size={14} style={{ color: "#fbbf24" }} />
      ),
      children:
        m.children.length > 0
          ? toTreeData(m.children)
          : undefined,
    };
  });
}

export default function AddProjectModal({
  open: openProp,
  onClose,
  onAdded,
}: Props) {
  const [path, setPath] = useState("");
  const [scanning, setScanning] = useState(false);
  const [modules, setModules] = useState<ScannedModule[]>([]);
  const [checkedKeys, setCheckedKeys] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const { message } = App.useApp();

  // 所有可选（可启动且未添加）服务的 pom_path 集合
  const selectableKeys = useMemo(() => {
    return flatten(modules)
      .filter((m) => m.is_service && !m.already_added)
      .map((m) => m.pom_path);
  }, [modules]);

  const allChecked =
    selectableKeys.length > 0 &&
    selectableKeys.every((k) => checkedKeys.includes(k));
  const someChecked =
    !allChecked && selectableKeys.some((k) => checkedKeys.includes(k));

  const flatModules = flatten(modules);
  const serviceCount = selectableKeys.length;
  const checkedServiceCount = selectableKeys.filter((k) =>
    checkedKeys.includes(k)
  ).length;

  const handlePickDir = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择项目根目录",
    });
    if (selected && typeof selected === "string") {
      setPath(selected);
      setError(null);
      setModules([]);
      setCheckedKeys([]);
      await doScan(selected);
    }
  };

  const doScan = async (dir: string) => {
    setScanning(true);
    setError(null);
    try {
      const result = await api.scanProject(dir);
      setModules(result);
      // 默认全选所有可启动且未添加的服务
      setCheckedKeys(
        flatten(result)
          .filter((m) => m.is_service && !m.already_added)
          .map((m) => m.pom_path)
      );
      if (result.length === 0) {
        setError("未扫描到任何 module");
      }
    } catch (e: any) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  };

  // 全选 / 取消全选
  const handleSelectAll = () => {
    if (allChecked) {
      // 取消全选
      setCheckedKeys([]);
    } else {
      // 全选
      setCheckedKeys([...selectableKeys]);
    }
  };

  // 反选
  const handleInvert = () => {
    setCheckedKeys(
      selectableKeys.filter((k) => !checkedKeys.includes(k))
    );
  };

  const handleAdd = async () => {
    if (!path) {
      message.warning("请先选择项目目录");
      return;
    }
    const selectedModules = flatModules.filter((m) =>
      checkedKeys.includes(m.pom_path)
    );
    if (selectedModules.length === 0) {
      message.warning("请至少勾选一个服务");
      return;
    }
    try {
      const project = await api.addProject(path, selectedModules);
      message.success(
        `已添加项目 "${project.name}"，包含 ${selectedModules.length} 个服务`
      );
      onAdded();
      handleClose();
    } catch (e: any) {
      message.error(`添加失败: ${e}`);
    }
  };

  const handleClose = () => {
    setPath("");
    setModules([]);
    setCheckedKeys([]);
    setError(null);
    onClose();
  };

  // 全选项的 Checkbox 状态
  const checkAllIndeterminate = someChecked && !allChecked;
  const checkAllChecked = allChecked;

  return (
    <Modal
      title={
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <FolderOpen size={15} style={{ color: "#a3e635" }} />
          添加项目
        </span>
      }
      open={openProp}
      onCancel={handleClose}
      onOk={handleAdd}
      okText={`添加 ${checkedServiceCount} 个服务`}
      cancelText="取消"
      width={680}
      okButtonProps={{ disabled: checkedServiceCount === 0 }}
      destroyOnClose
    >
      {/* 目录选择 */}
      <Space.Compact style={{ width: "100%", marginBottom: 12 }}>
        <Input
          value={path}
          placeholder="选择项目根目录（含 pom.xml）"
          readOnly
          onClick={handlePickDir}
          style={{ cursor: "pointer" }}
        />
        <Button
          type="primary"
          icon={<FolderOpen size={13} />}
          onClick={handlePickDir}
        >
          选择目录
        </Button>
      </Space.Compact>

      {error && (
        <Alert
          type="error"
          message={error}
          style={{ marginBottom: 12 }}
          showIcon
        />
      )}

      {scanning && (
        <div style={{ textAlign: "center", padding: 40 }}>
          <Spin tip="正在扫描 modules..." />
        </div>
      )}

      {!scanning && modules.length > 0 && (
        <>
          {/* 工具栏：统计 + 全选/反选 */}
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              marginBottom: 8,
              padding: "6px 10px",
              background: "#fafafa",
              borderRadius: 6,
              border: "1px solid #f0f0f0",
            }}
          >
            <span style={{ fontSize: 12, color: "#666" }}>
              共 {serviceCount} 个可启动服务，已选 {checkedServiceCount} 个
            </span>
            <Space size={4}>
              <Checkbox
                indeterminate={checkAllIndeterminate}
                checked={checkAllChecked}
                onChange={handleSelectAll}
                disabled={serviceCount === 0}
                style={{ fontSize: 12 }}
              >
                {allChecked ? "取消全选" : "全选"}
              </Checkbox>
              <Button
                type="text"
                size="small"
                icon={<CheckSquare size={13} />}
                onClick={handleInvert}
                disabled={serviceCount === 0}
                style={{ fontSize: 12 }}
              >
                反选
              </Button>
            </Space>
          </div>

          {/* 模块树 */}
          <div
            style={{
              maxHeight: 340,
              overflow: "auto",
              border: "1px solid #f0f0f0",
              borderRadius: 6,
              padding: 8,
            }}
          >
            <Tree
              checkable
              checkedKeys={checkedKeys}
              onCheck={(keys) => {
                setCheckedKeys(
                  (Array.isArray(keys) ? keys : keys.checked) as string[]
                );
              }}
              treeData={toTreeData(modules)}
              defaultExpandAll
              selectable={false}
            />
          </div>
        </>
      )}
    </Modal>
  );
}
