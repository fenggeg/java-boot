import {useEffect, useState} from "react";
import {App, AutoComplete, Button, Divider, Form, Input, Modal, Typography} from "antd";
import {FolderOpen, Plus, Trash} from "./Icons";
import {open} from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type {JdkInfo, MavenInfo, Project} from "../types";

const { Text } = Typography;

interface Props {
  project: Project | null;
  onClose: () => void;
  onSaved: () => void;
}

// ── 环境变量 解析 / 序列化 ──────────────────────────────────

/** 带唯一 id 的环境变量条目（id 仅前端使用，不序列化） */
interface EnvVarEntry {
  id: string;
  key: string;
  value: string;
}

let envVarIdCounter = 0;
function newEnvVarId(): string {
  envVarIdCounter += 1;
  return `env-${envVarIdCounter}`;
}

function parseEnvVars(json: string | null | undefined): EnvVarEntry[] {
  if (!json || !json.trim()) return [];
  try {
    const arr = JSON.parse(json) as unknown;
    if (!Array.isArray(arr)) return [];
    return arr
      .filter((x): x is Record<string, unknown> => !!x && typeof x === "object" && typeof x.key === "string" && (x.key as string).trim().length > 0)
      .map((x) => ({
        id: newEnvVarId(),
        key: (x.key as string).trim(),
        value: String(x.value ?? ""),
      }));
  } catch {
    return [];
  }
}

function serializeEnvVars(list: EnvVarEntry[]): string | null {
  const cleaned = list
    .filter((x) => x.key.trim())
    .map(({ key, value }) => ({ key, value }));
  if (cleaned.length === 0) return null;
  return JSON.stringify(cleaned);
}

export default function ProjectConfigModal({
  project,
  onClose,
  onSaved,
}: Props) {
  const [form] = Form.useForm();
  const { message } = App.useApp();
  const [jdks, setJdks] = useState<JdkInfo[]>([]);
  const [mavens, setMavens] = useState<MavenInfo[]>([]);
  const [envVars, setEnvVars] = useState<EnvVarEntry[]>([]);
  const [saving, setSaving] = useState(false);
  const isOpen = !!project;

  useEffect(() => {
    if (project) {
      form.setFieldsValue({
        java_home: project.java_home ?? "",
        maven_home: project.maven_home ?? "",
      });
      setEnvVars(parseEnvVars(project.env_vars));
      api.detectJdks().then(setJdks).catch(() => setJdks([]));
      api.detectMavens().then(setMavens).catch(() => setMavens([]));
    } else {
      // 关闭时重置，避免下次打开闪现旧数据
      form.resetFields();
      setJdks([]);
      setMavens([]);
      setEnvVars([]);
    }
  }, [project, form]);

  const handlePickJdk = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择 JDK 根目录（含 bin/java.exe）",
    });
    if (selected && typeof selected === "string") {
      form.setFieldValue("java_home", selected);
    }
  };

  const handlePickMaven = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择 Maven 根目录（含 bin/mvn.cmd）",
    });
    if (selected && typeof selected === "string") {
      form.setFieldValue("maven_home", selected);
    }
  };

  const handleSave = async () => {
    if (!project) return;
    try {
      const values = await form.validateFields();
      setSaving(true);
      await api.updateProjectEnv(
        project.id,
        values.java_home?.trim() || null,
        values.maven_home?.trim() || null,
        serializeEnvVars(envVars),
      );
      message.success("项目配置已保存");
      onSaved();
      onClose();
    } catch (e) {
      if (e && typeof e === "object" && "errorFields" in e) return;
      message.error(`保存失败: ${api.toErrMsg(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const jdkOptions = [
    { value: "", label: "使用系统默认 (JAVA_HOME)" },
    ...jdks.map((j) => ({
      value: j.path,
      label: `${j.vendor} ${j.version} — ${j.path}`,
    })),
  ];

  const mavenOptions = [
    { value: "", label: "使用项目 mvnw 或系统 PATH" },
    ...mavens.map((m) => ({
      value: m.path,
      label: `${m.version} — ${m.path}`,
    })),
  ];

  return (
    <Modal
      title={
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <FolderOpen size={15} style={{ color: "#0071e3" }} />
          项目环境配置 — {project?.name}
        </span>
      }
      open={isOpen}
      onCancel={onClose}
      onOk={handleSave}
      okText="保存"
      cancelText="取消"
      confirmLoading={saving}
      width={560}
      destroyOnClose
      maskClosable={false}
    >
      <Text type="secondary" style={{ display: "block", marginBottom: 16, fontSize: 12 }}>
        JDK 和 Maven 路径为项目级配置，对该项目下所有服务生效。
      </Text>
      <Form form={form} layout="vertical">
        <Form.Item
          label="JDK 路径"
          name="java_home"
          tooltip="该项目下所有服务使用的 JDK，留空则用系统 JAVA_HOME"
          extra={
            jdks.length > 0 ? (
              <Text type="secondary" style={{ fontSize: 11 }}>
                检测到 {jdks.length} 个已安装 JDK
              </Text>
            ) : (
              <Text type="warning" style={{ fontSize: 11 }}>
                未检测到已安装 JDK，请手动指定
              </Text>
            )
          }
        >
          <AutoComplete
            options={jdkOptions}
            placeholder="留空使用系统 JAVA_HOME"
            filterOption={(input, option) =>
              (option?.label ?? "").toLowerCase().includes(input.toLowerCase())
            }
          >
            <Input
              suffix={
                <FolderOpen
                  onClick={handlePickJdk}
                  style={{ cursor: "pointer", color: "#0071e3" }}
                />
              }
            />
          </AutoComplete>
        </Form.Item>

        <Form.Item
          label="Maven 路径"
          name="maven_home"
          tooltip="该项目下所有服务使用的 Maven，留空则优先用项目自带 mvnw.cmd，再退回系统 PATH"
          extra={
            mavens.length > 0 ? (
              <Text type="secondary" style={{ fontSize: 11 }}>
                检测到 {mavens.length} 个已安装 Maven
              </Text>
            ) : (
              <Text type="warning" style={{ fontSize: 11 }}>
                未检测到 Maven，将依赖项目自带 mvnw
              </Text>
            )
          }
        >
          <AutoComplete
            options={mavenOptions}
            placeholder="留空使用 mvnw 或系统 PATH"
            filterOption={(input, option) =>
              (option?.label ?? "").toLowerCase().includes(input.toLowerCase())
            }
          >
            <Input
              suffix={
                <FolderOpen
                  onClick={handlePickMaven}
                  style={{ cursor: "pointer", color: "#0071e3" }}
                />
              }
            />
          </AutoComplete>
        </Form.Item>

        <Divider style={{ marginTop: 16, marginBottom: 12 }}>环境变量</Divider>

        <div style={{ marginBottom: 8, fontSize: 11, color: "var(--text-3)", lineHeight: 1.6 }}>
          以 <code style={{ color: "var(--blue)" }}>KEY=VALUE</code> 注入子进程环境变量，
          对该项目下所有服务生效（mvn 编译 + java 运行）。服务级同名变量会覆盖此处配置。
        </div>

        {/* 环境变量 key-value 编辑器 */}
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {envVars.map((item) => (
            <div key={item.id} style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <Input
                placeholder="key，如 FOO"
                value={item.key}
                onChange={(e) => {
                  const v = e.target.value;
                  setEnvVars((list) =>
                    list.map((x) => (x.id === item.id ? { ...x, key: v } : x))
                  );
                }}
                style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: 12 }}
              />
              <span style={{ color: "var(--text-3)", fontSize: 12 }}>=</span>
              <Input
                placeholder="value，如 bar"
                value={item.value}
                onChange={(e) => {
                  const v = e.target.value;
                  setEnvVars((list) =>
                    list.map((x) => (x.id === item.id ? { ...x, value: v } : x))
                  );
                }}
                style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: 12 }}
              />
              <Button
                size="small"
                type="text"
                danger
                onClick={() =>
                  setEnvVars((list) => list.filter((x) => x.id !== item.id))
                }
                style={{ flexShrink: 0, padding: "0 6px" }}
              >
                <Trash size={13} />
              </Button>
            </div>
          ))}
          <Button
            size="small"
            type="dashed"
            block
            onClick={() => setEnvVars((list) => [...list, { id: newEnvVarId(), key: "", value: "" }])}
            style={{ marginTop: 2 }}
          >
            <Plus size={12} /> 添加环境变量
          </Button>
        </div>
      </Form>
    </Modal>
  );
}
