import {useEffect, useState} from "react";
import {App, AutoComplete, Form, Input, Modal, Typography} from "antd";
import {FolderOpen} from "./Icons";
import {open} from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type {JdkInfo, MavenInfo, Project} from "../types";

const { Text } = Typography;

interface Props {
  project: Project | null;
  onClose: () => void;
  onSaved: () => void;
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
  const [saving, setSaving] = useState(false);
  const isOpen = !!project;

  useEffect(() => {
    if (project) {
      form.setFieldsValue({
        java_home: project.java_home ?? "",
        maven_home: project.maven_home ?? "",
      });
      api.detectJdks().then(setJdks).catch(() => setJdks([]));
      api.detectMavens().then(setMavens).catch(() => setMavens([]));
    } else {
      // 关闭时重置，避免下次打开闪现旧数据
      form.resetFields();
      setJdks([]);
      setMavens([]);
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
        values.maven_home?.trim() || null
      );
      message.success("项目配置已保存");
      onSaved();
      onClose();
    } catch (e: any) {
      if (e?.errorFields) return;
      message.error(`保存失败: ${e}`);
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
      </Form>
    </Modal>
  );
}
