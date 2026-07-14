import { useEffect, useState } from "react";
import { Modal, Form, Input, App } from "antd";
import { Settings } from "./Icons";
import * as api from "../api";
import type { Service } from "../types";

interface Props {
  service: Service | null;
  onClose: () => void;
  onSaved: () => void;
}

export default function ServiceConfigModal({
  service,
  onClose,
  onSaved,
}: Props) {
  const [form] = Form.useForm();
  const { message } = App.useApp();
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (service) {
      form.setFieldsValue({
        name: service.name,
        maven_opts: service.maven_opts ?? "",
        profiles: service.profiles ?? "",
      });
    }
  }, [service, form]);

  const handleSave = async () => {
    if (!service) return;
    try {
      const values = await form.validateFields();
      setSaving(true);
      await api.updateService(
        service.id,
        values.name,
        undefined,
        values.maven_opts?.trim() || null,
        values.profiles?.trim() || null
      );
      message.success("配置已保存");
      onSaved();
      onClose();
    } catch (e: any) {
      if (e?.errorFields) return;
      message.error(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      title={
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Settings size={14} style={{ color: "#a3e635" }} />
          服务配置 — {service?.name}
        </span>
      }
      open={!!service}
      onCancel={onClose}
      onOk={handleSave}
      okText="保存"
      cancelText="取消"
      confirmLoading={saving}
      width={520}
      destroyOnClose
    >
      <Form form={form} layout="vertical">
        <Form.Item
          label="服务名"
          name="name"
          rules={[{ required: true, message: "请输入服务名" }]}
        >
          <Input placeholder="服务显示名" />
        </Form.Item>

        <Form.Item
          label="Spring Profiles"
          name="profiles"
          tooltip="spring.profiles.active 值，如 dev、prod。多个用逗号分隔。"
        >
          <Input placeholder="如 dev（留空则不指定）" />
        </Form.Item>

        <Form.Item
          label="额外 Maven 参数"
          name="maven_opts"
          tooltip="追加到 spring-boot:run 后的参数，如 -DskipTests -Xmx512m"
        >
          <Input placeholder="如 -DskipTests（留空则无）" />
        </Form.Item>
      </Form>
    </Modal>
  );
}
