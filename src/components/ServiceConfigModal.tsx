import { useEffect, useState, useMemo } from "react";
import { Modal, Form, Input, App, Segmented, InputNumber, Divider, Switch, Tooltip, Button } from "antd";
import { Settings, Plus, Trash } from "./Icons";
import * as api from "../api";
import type { Service, OverrideProperty } from "../types";

interface Props {
  service: Service | null;
  onClose: () => void;
  onSaved: () => void;
}

// ── JVM 内存预设 ─────────────────────────────────────────────
const MEM_PRESETS: { label: string; xms: number; xmx: number; desc: string }[] = [
  { label: "轻量", xms: 128, xmx: 256, desc: "轻量服务 / 工具类" },
  { label: "标准", xms: 256, xmx: 512, desc: "默认推荐 · 单体应用" },
  { label: "较大", xms: 512, xmx: 1024, desc: "中等流量 / 多模块" },
  { label: "大型", xms: 1024, xmx: 2048, desc: "高负载 / 微服务聚合" },
];
const DEFAULT_PRESET_IDX = 1;

// ── 可视化参数 token 定义 ────────────────────────────────────
// 这些参数会被智能提取/合成，剩余的无法识别的进入"高级参数"框
interface PortConfig { enabled: boolean; port: number | null }
interface DebugConfig { enabled: boolean; port: number | null; suspend: boolean }

// 端口正则：--server.port=8080 或 -Dserver.port=8080
const PORT_RE = /(?:^|\s)(?:--D?server\.port=|-Dserver\.port=)(\d{2,5})(?=\s|$)/;
// debug 正则：-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:5005
const DEBUG_RE = /-agentlib:jdwp=[^\s]+address=(?:\*:)?(\d+)/;
const DEBUG_SUSPEND_RE = /suspend=y/;

// ── 解析 / 合成 ──────────────────────────────────────────────

/** 从 maven_opts 字符串中分离所有可视化参数与剩余参数 */
function parseAll(raw: string | null | undefined) {
  const opts = (raw ?? "").trim();
  if (!opts) {
    return {
      xms: null as number | null, xmx: null as number | null,
      port: { enabled: false, port: null } as PortConfig,
      debug: { enabled: false, port: 5005, suspend: false } as DebugConfig,
      rest: "",
    };
  }

  // 提取端口
  let port: PortConfig = { enabled: false, port: null };
  const portMatch = opts.match(PORT_RE);
  if (portMatch) {
    port = { enabled: true, port: parseInt(portMatch[1]) };
  }

  // 提取 debug
  let debug: DebugConfig = { enabled: false, port: 5005, suspend: false };
  const debugMatch = opts.match(DEBUG_RE);
  if (debugMatch) {
    debug = {
      enabled: true,
      port: parseInt(debugMatch[1]),
      suspend: DEBUG_SUSPEND_RE.test(opts),
    };
  }

  // 提取 Xms/Xmx
  let xms: number | null = null;
  let xmx: number | null = null;
  const restTokens: string[] = [];

  for (const tok of opts.split(/\s+/)) {
    if (!tok) continue;
    const m = tok.match(/^-Xms(\d+)([kmgKMG]?)$/);
    const x = tok.match(/^-Xmx(\d+)([kmgKMG]?)$/);
    if (m) { xms = toMb(parseInt(m[1]), m[2]); continue; }
    if (x) { xmx = toMb(parseInt(x[1]), x[2]); continue; }
    // 端口 / debug token 不进 rest
    if (PORT_RE.test(tok) || tok.startsWith("-agentlib:jdwp")) continue;
    restTokens.push(tok);
  }

  return { xms, xmx, port, debug, rest: restTokens.join(" ") };
}

function toMb(n: number, unit: string): number {
  switch (unit.toLowerCase()) {
    case "k": return Math.round(n / 1024);
    case "g": return n * 1024;
    default: return n;
  }
}

function mbToArg(mb: number, kind: "ms" | "mx"): string {
  return `-${kind === "ms" ? "Xms" : "Xmx"}${mb}m`;
}

function matchPreset(xms: number | null, xmx: number | null): number {
  for (let i = 0; i < MEM_PRESETS.length; i++) {
    if (MEM_PRESETS[i].xms === xms && MEM_PRESETS[i].xmx === xmx) return i;
  }
  return -1;
}

/** 根据各可视化配置合成最终 maven_opts */
function buildMavenOpts(
  xms: number | null, xmx: number | null,
  port: PortConfig, debug: DebugConfig, rest: string
): string {
  const parts: string[] = [];
  // JVM 参数区（-X / -D）
  if (xms && xms > 0) parts.push(mbToArg(xms, "ms"));
  if (xmx && xmx > 0) parts.push(mbToArg(xmx, "mx"));
  if (port.enabled && port.port) {
    parts.push(`-Dserver.port=${port.port}`);
  }
  if (debug.enabled && debug.port) {
    parts.push(
      `-agentlib:jdwp=transport=dt_socket,server=y,suspend=${debug.suspend ? "y" : "n"},address=*:${debug.port}`
    );
  }
  // 应用参数区（-- 开头，放 classpath 之后）
  const restTrim = rest.trim();
  if (restTrim) parts.push(restTrim);
  return parts.join(" ");
}

// ── 覆盖属性 解析 / 序列化 ──────────────────────────────────
function parseOverrideProperties(json: string | null | undefined): OverrideProperty[] {
  if (!json || !json.trim()) return [];
  try {
    const arr = JSON.parse(json);
    if (!Array.isArray(arr)) return [];
    return arr
      .filter((x: any) => x && typeof x.key === "string" && x.key.trim())
      .map((x: any) => ({ key: x.key, value: String(x.value ?? "") }));
  } catch {
    return [];
  }
}

function serializeOverrideProperties(list: OverrideProperty[]): string | null {
  const cleaned = list.filter((x) => x.key.trim());
  if (cleaned.length === 0) return null;
  return JSON.stringify(cleaned);
}

export default function ServiceConfigModal({ service, onClose, onSaved }: Props) {
  const [form] = Form.useForm();
  const { message } = App.useApp();
  const [saving, setSaving] = useState(false);

  const [memMode, setMemMode] = useState<number>(DEFAULT_PRESET_IDX);
  const [customXms, setCustomXms] = useState<number | null>(256);
  const [customXmx, setCustomXmx] = useState<number | null>(512);

  const [portCfg, setPortCfg] = useState<PortConfig>({ enabled: false, port: 8080 });
  const [debugCfg, setDebugCfg] = useState<DebugConfig>({ enabled: false, port: 5005, suspend: false });
  const [restOpts, setRestOpts] = useState<string>("");
  const [devMode, setDevMode] = useState<boolean>(false);
  const [overrides, setOverrides] = useState<OverrideProperty[]>([]);

  useEffect(() => {
    if (service) {
      form.setFieldsValue({
        name: service.name,
        profiles: service.profiles ?? "",
      });
      const p = parseAll(service.maven_opts);
      setRestOpts(p.rest);
      setPortCfg(p.port.enabled ? p.port : { enabled: false, port: 8080 });
      setDebugCfg(p.debug.enabled ? p.debug : { enabled: false, port: 5005, suspend: false });
      setDevMode(!!service.dev_mode);
      setOverrides(parseOverrideProperties(service.override_properties));

      const hit = matchPreset(p.xms, p.xmx);
      if (hit >= 0) {
        setMemMode(hit);
      } else if (p.xms || p.xmx) {
        setMemMode(-1);
      } else {
        setMemMode(DEFAULT_PRESET_IDX);
      }
      setCustomXms(p.xms ?? MEM_PRESETS[DEFAULT_PRESET_IDX].xms);
      setCustomXmx(p.xmx ?? MEM_PRESETS[DEFAULT_PRESET_IDX].xmx);
    }
  }, [service, form]);

  const effectiveMem = useMemo(() => {
    if (memMode >= 0) return { xms: MEM_PRESETS[memMode].xms, xmx: MEM_PRESETS[memMode].xmx };
    return { xms: customXms, xmx: customXmx };
  }, [memMode, customXms, customXmx]);

  const finalOpts = useMemo(
    () => buildMavenOpts(effectiveMem.xms, effectiveMem.xmx, portCfg, debugCfg, restOpts),
    [effectiveMem, portCfg, debugCfg, restOpts]
  );

  const handleSave = async () => {
    if (!service) return;
    try {
      const values = await form.validateFields();
      setSaving(true);
      await api.updateService(
        service.id,
        values.name,
        undefined,
        finalOpts || null,
        values.profiles?.trim() || null,
        devMode,
        undefined,
        serializeOverrideProperties(overrides),
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
      width={600}
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

        <Divider style={{ marginTop: 4, marginBottom: 16 }}>JVM 内存</Divider>

        <Form.Item label="内存档位">
          <Segmented
            block
            value={String(memMode)}
            onChange={(v) => setMemMode(Number(v))}
            options={[
              ...MEM_PRESETS.map((p, i) => ({
                label: (
                  <span style={{ display: "inline-flex", flexDirection: "column", lineHeight: 1.1 }}>
                    <span>{p.label}</span>
                    <span style={{ fontSize: 10, opacity: 0.6 }}>{p.xms}/{p.xmx}m</span>
                  </span>
                ),
                value: String(i),
              })),
              {
                label: (
                  <span style={{ display: "inline-flex", flexDirection: "column", lineHeight: 1.1 }}>
                    <span>自定义</span>
                    <span style={{ fontSize: 10, opacity: 0.6 }}>手动</span>
                  </span>
                ),
                value: "-1",
              },
            ]}
          />
          <div style={{ marginTop: 6, fontFamily: "var(--font-mono)", fontSize: 10, color: "var(--text-3)", letterSpacing: "0.04em" }}>
            {memMode >= 0
              ? `${MEM_PRESETS[memMode].desc} · -Xms${MEM_PRESETS[memMode].xms}m -Xmx${MEM_PRESETS[memMode].xmx}m`
              : "手动指定初始 / 最大堆内存"}
          </div>
        </Form.Item>

        {memMode === -1 && (
          <div style={{ display: "flex", gap: 16, marginBottom: 8 }}>
            <Form.Item label="初始堆 -Xms (MB)" style={{ flex: 1, marginBottom: 0 }}>
              <InputNumber
                min={16} max={32768} step={64}
                value={customXms ?? undefined}
                onChange={(v) => setCustomXms(v as number | null)}
                style={{ width: "100%" }}
                placeholder="如 256"
              />
            </Form.Item>
            <Form.Item label="最大堆 -Xmx (MB)" style={{ flex: 1, marginBottom: 0 }}>
              <InputNumber
                min={16} max={32768} step={64}
                value={customXmx ?? undefined}
                onChange={(v) => setCustomXmx(v as number | null)}
                style={{ width: "100%" }}
                placeholder="如 512"
              />
            </Form.Item>
          </div>
        )}

        <Divider style={{ marginTop: 16, marginBottom: 16 }}>服务配置</Divider>

        {/* 开发快速启动 */}
        <div
          style={{
            display: "flex", alignItems: "center", gap: 12,
            padding: "10px 12px",
            background: "var(--surface-2)",
            border: "1px solid var(--border-2)",
            borderRadius: 2,
            marginBottom: 12,
          }}
        >
          <Switch
            size="small"
            checked={devMode}
            onChange={setDevMode}
          />
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 13, color: "var(--text)" }}>
              开发快速启动模式
              <Tooltip title="启用后：-XX:TieredStopAtLevel=1（只 C1 分层编译）、-XX:+AlwaysPreTouch、关 JMX / devtools 重启 / ANSI，冷启动可提速 20-40%。适合开发环境，不建议生产使用。">
                <span style={{ marginLeft: 6, color: "var(--text-3)", cursor: "help", fontSize: 11 }}>?</span>
              </Tooltip>
            </div>
            <div style={{ fontSize: 10, fontFamily: "var(--font-mono)", color: "var(--text-3)", letterSpacing: "0.04em" }}>
              TieredStopAtLevel=1 · AlwaysPreTouch · jmx=false · devtools.restart=false
            </div>
          </div>
        </div>

        {/* 端口可视化 */}
        <div
          style={{
            display: "flex", alignItems: "center", gap: 12,
            padding: "10px 12px",
            background: "var(--surface-2)",
            border: "1px solid var(--border-2)",
            borderRadius: 2,
            marginBottom: 12,
          }}
        >
          <Switch
            size="small"
            checked={portCfg.enabled}
            onChange={(enabled) => setPortCfg((c) => ({ ...c, enabled }))}
          />
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 13, color: "var(--text)" }}>服务端口</div>
            <div style={{ fontSize: 10, fontFamily: "var(--font-mono)", color: "var(--text-3)", letterSpacing: "0.04em" }}>
              -Dserver.port · 关闭则使用 application.yml 中的默认值
            </div>
          </div>
          <InputNumber
            min={1} max={65535}
            value={portCfg.port ?? undefined}
            disabled={!portCfg.enabled}
            onChange={(v) => setPortCfg((c) => ({ ...c, port: v as number | null }))}
            style={{ width: 100 }}
            placeholder="8080"
          />
        </div>

        {/* Debug 可视化 */}
        <div
          style={{
            display: "flex", alignItems: "center", gap: 12,
            padding: "10px 12px",
            background: "var(--surface-2)",
            border: "1px solid var(--border-2)",
            borderRadius: 2,
            marginBottom: 12,
          }}
        >
          <Switch
            size="small"
            checked={debugCfg.enabled}
            onChange={(enabled) => setDebugCfg((c) => ({ ...c, enabled }))}
          />
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 13, color: "var(--text)" }}>
              远程调试
              <Tooltip title="开启后 JVM 挂起等待调试器连接（suspend=n 则不等待）。IDE 连接 address=*:端口">
                <span style={{ marginLeft: 6, color: "var(--text-3)", cursor: "help", fontSize: 11 }}>?</span>
              </Tooltip>
            </div>
            <div style={{ fontSize: 10, fontFamily: "var(--font-mono)", color: "var(--text-3)", letterSpacing: "0.04em" }}>
              -agentlib:jdwp · {debugCfg.suspend ? "启动时挂起等待" : "不等待，立即启动"}
            </div>
          </div>
          <InputNumber
            min={1} max={65535}
            value={debugCfg.port ?? undefined}
            disabled={!debugCfg.enabled}
            onChange={(v) => setDebugCfg((c) => ({ ...c, port: v as number | null }))}
            style={{ width: 100 }}
            placeholder="5005"
          />
          <Tooltip title={debugCfg.suspend ? "当前：启动时挂起等待调试器" : "当前：不等待，立即启动"}>
            <Switch
              size="small"
              checked={debugCfg.suspend}
              disabled={!debugCfg.enabled}
              onChange={(suspend) => setDebugCfg((c) => ({ ...c, suspend }))}
              checkedChildren="挂起"
              unCheckedChildren="不等待"
            />
          </Tooltip>
        </div>

        <Form.Item
          label="Spring Profiles"
          name="profiles"
          tooltip="spring.profiles.active 值，如 dev、prod。多个用逗号分隔。"
        >
          <Input placeholder="如 dev（留空则不指定）" />
        </Form.Item>

        <Form.Item
          label="高级参数"
          tooltip="无法可视化的其余参数。以 -D/-X 开头的作 JVM 参数，-- 开头的作应用参数。"
        >
          <Input
            placeholder="如 -Dspring.devtools.restart.enabled=false --debug"
            value={restOpts}
            onChange={(e) => setRestOpts(e.target.value)}
          />
        </Form.Item>

        <Divider style={{ marginTop: 16, marginBottom: 12 }}>配置覆盖属性</Divider>

        <div style={{ marginBottom: 8, fontSize: 11, color: "var(--text-3)", lineHeight: 1.6 }}>
          以 <code style={{ color: "var(--lime)" }}>-Dkey=value</code> 注入 JVM 系统属性，
          Spring Boot 优先级高于 application.yml。常用于覆盖注册中心 IP、数据源、日志级别等。
        </div>

        {/* 覆盖属性 key-value 编辑器 */}
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {overrides.map((item, idx) => (
            <div key={idx} style={{ display: "flex", gap: 6, alignItems: "center" }}>
              <Input
                placeholder="key，如 spring.cloud.nacos.discovery.ip"
                value={item.key}
                onChange={(e) => {
                  const v = e.target.value;
                  setOverrides((list) =>
                    list.map((x, i) => (i === idx ? { ...x, key: v } : x))
                  );
                }}
                style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: 12 }}
              />
              <span style={{ color: "var(--text-3)", fontSize: 12 }}>=</span>
              <Input
                placeholder="value，如 192.168.1.100"
                value={item.value}
                onChange={(e) => {
                  const v = e.target.value;
                  setOverrides((list) =>
                    list.map((x, i) => (i === idx ? { ...x, value: v } : x))
                  );
                }}
                style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: 12 }}
              />
              <Button
                size="small"
                type="text"
                danger
                onClick={() =>
                  setOverrides((list) => list.filter((_, i) => i !== idx))
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
            onClick={() => setOverrides((list) => [...list, { key: "", value: "" }])}
            style={{ marginTop: 2 }}
          >
            <Plus size={12} /> 添加属性
          </Button>
        </div>

        {/* 最终合成预览 */}
        <div
          style={{
            marginTop: 4, padding: "8px 10px",
            background: "var(--surface-2)",
            border: "1px solid var(--border-2)",
            borderRadius: 2,
            fontFamily: "var(--font-code)",
            fontSize: 11,
            color: "var(--text-2)",
            lineHeight: 1.6,
            wordBreak: "break-all",
          }}
        >
          <span style={{ color: "var(--text-3)", marginRight: 8 }}>最终 maven_opts:</span>
          <span style={{ color: finalOpts ? "var(--lime)" : "var(--text-3)" }}>
            {finalOpts || "（空）"}
          </span>
        </div>
      </Form>
    </Modal>
  );
}
