import {useState} from "react";
import {App, Form, Input, Modal} from "antd";
import {Plus} from "./Icons";
import {open} from "@tauri-apps/plugin-dialog";
import * as api from "../api";

interface Props {
  open: boolean;
  onClose: () => void;
  onAdded: () => void;
}

export default function AddServiceModal({
  open: openProp,
  onClose,
  onAdded,
}: Props) {
  const [form] = Form.useForm();
  const [pomPath, setPomPath] = useState("");
  const { message } = App.useApp();

  const handlePick = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Maven POM", extensions: ["xml"] }],
      title: "选择 pom.xml 文件",
    });
    if (selected && typeof selected === "string") {
      setPomPath(selected);
      form.setFieldValue("pomPath", selected);
      // 默认服务名留空，后端解析 artifactId
    }
  };

  const handleAdd = async () => {
    if (!pomPath) {
      message.warning("请选择 pom.xml 文件");
      return;
    }
    const name = form.getFieldValue("name");
    try {
      const service = await api.addService(pomPath, name || undefined);
      message.success(`已添加服务 "${service.name}"`);
      onAdded();
      handleClose();
    } catch (e: any) {
      message.error(`添加失败: ${e}`);
    }
  };

  const handleClose = () => {
    setPomPath("");
    form.resetFields();
    onClose();
  };

  return (
    <Modal
      title={
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Plus size={15} style={{ color: "#0071e3" }} />
          添加服务
        </span>
      }
      open={openProp}
      onCancel={handleClose}
      onOk={handleAdd}
      okText="添加"
      cancelText="取消"
      destroyOnClose
    >
      <Form form={form} layout="vertical">
        <Form.Item label="pom.xml 路径" required>
          <Input
            value={pomPath}
            placeholder="选择服务的 pom.xml"
            readOnly
            onClick={handlePick}
            style={{ cursor: "pointer" }}
            suffix={
              <a onClick={handlePick} style={{ fontSize: 12 }}>
                选择文件
              </a>
            }
          />
        </Form.Item>
        <Form.Item
          label="服务名"
          name="name"
          help="留空则自动取 pom.xml 中的 artifactId"
        >
          <Input placeholder="自定义服务名（可选）" />
        </Form.Item>
      </Form>
    </Modal>
  );
}
