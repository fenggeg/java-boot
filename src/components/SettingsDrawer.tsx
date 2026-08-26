import {App, Divider, Drawer, Form, InputNumber, Switch, Typography} from "antd";
import {Settings} from "./Icons";
import {useStore} from "../store";
import type {AppConfig} from "../types";
import {useEffect, useRef, useState} from "react";

const { Text } = Typography;

interface Props {
  open: boolean;
  onClose: () => void;
}

export default function SettingsDrawer({ open, onClose }: Props) {
  const config = useStore((s) => s.config);
  const updateConfig = useStore((s) => s.updateConfig);
  const { message } = App.useApp();
  const [local, setLocal] = useState<AppConfig>(config);

  // 最近一次渲染的 local，供 save 在连续输入时合并增量，避免闭包陈旧值
  const localRef = useRef(local);
  localRef.current = local;
  // 防抖：InputNumber 每次击键都触发 onChange，改为 500ms 后统一落盘，减少 DB 写
  const timerRef = useRef<number | null>(null);
  const pendingRef = useRef<AppConfig | null>(null);

  useEffect(() => {
    setLocal(config);
  }, [config]);

  useEffect(
    () => () => {
      if (timerRef.current) window.clearTimeout(timerRef.current);
    },
    []
  );

  const scheduleSave = (next: AppConfig) => {
    pendingRef.current = next;
    if (timerRef.current) window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(async () => {
      timerRef.current = null;
      pendingRef.current = null;
      try {
        await updateConfig(next);
      } catch (e: any) {
        message.error(`保存配置失败: ${e}`);
      }
    }, 500);
  };

  const save = (patch: Partial<AppConfig>) => {
    const next = { ...localRef.current, ...patch };
    setLocal(next);
    scheduleSave(next);
  };

  const handleClose = () => {
    // 关闭前落盘防抖窗口内未保存的修改
    if (timerRef.current) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
      if (pendingRef.current) {
        const pending = pendingRef.current;
        pendingRef.current = null;
        updateConfig(pending).catch((e: any) =>
          message.error(`保存配置失败: ${e}`)
        );
      }
    }
    onClose();
  };

  return (
    <Drawer
      title={
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Settings size={14} style={{ color: "#0071e3" }} />
          设置
        </span>
      }
      open={open}
      onClose={handleClose}
      width={420}
      destroyOnClose
    >
      <Form layout="vertical">
        <Divider orientation="left">端口检测</Divider>
        <Form.Item
          label="端口刷新间隔（秒）"
          tooltip="多久重新查询一次服务的监听端口"
        >
          <InputNumber
            min={1}
            max={30}
            value={local.port_refresh_interval_secs}
            onChange={(v) =>
              save({ port_refresh_interval_secs: v ?? 2 })
            }
            style={{ width: "100%" }}
          />
        </Form.Item>

        <Divider orientation="left">自动重启</Divider>
        <Form.Item
          label="防抖时间（秒）"
          tooltip="文件变更后等待多久再触发重启，避免频繁保存导致反复重启"
        >
          <InputNumber
            min={1}
            max={30}
            value={local.auto_restart_debounce_secs}
            onChange={(v) =>
              save({ auto_restart_debounce_secs: v ?? 3 })
            }
            style={{ width: "100%" }}
          />
        </Form.Item>
        <Form.Item
          label="编译失败时停止旧进程"
          tooltip="关闭则编译失败时保留旧进程继续运行（默认）；开启则编译失败即下线服务"
        >
          <Switch
            checked={local.stop_on_compile_fail}
            onChange={(v) => save({ stop_on_compile_fail: v })}
          />
          <Text type="secondary" style={{ marginLeft: 12 }}>
            {local.stop_on_compile_fail
              ? "编译失败即停止旧进程，端口释放"
              : "编译失败时保留旧进程，服务不中断"}
          </Text>
        </Form.Item>
        <Form.Item
          label="开发模式懒加载（实验性）"
          tooltip="dev_mode 启动时注入 -Dspring.main.lazy-initialization=true，Bean 按需初始化，显著缩短 Spring 上下文启动时间；依赖 @PostConstruct 时序的应用可能出现初始化顺序问题"
        >
          <Switch
            checked={local.dev_lazy_init}
            onChange={(v) => save({ dev_lazy_init: v })}
          />
          <Text type="secondary" style={{ marginLeft: 12 }}>
            {local.dev_lazy_init
              ? "dev_mode 启动时启用 Spring 懒加载"
              : "关闭（默认），完整初始化所有 Bean"}
          </Text>
        </Form.Item>

        <Divider orientation="left">日志</Divider>
        <Form.Item
          label="单服务日志缓冲行数"
          tooltip="每个服务在内存中保留的最近日志行数，超出后丢弃最旧的"
        >
          <InputNumber
            min={1000}
            max={50000}
            step={1000}
            value={local.log_buffer_lines}
            onChange={(v) =>
              save({ log_buffer_lines: v ?? 10000 })
            }
            style={{ width: "100%" }}
          />
        </Form.Item>

        <Divider orientation="left">退出行为</Divider>
        <Form.Item
          label="关闭应用时停止所有服务"
          tooltip="开启则退出时自动停止所有运行中的服务；关闭则服务继续运行"
        >
          <Switch
            checked={local.stop_all_on_exit}
            onChange={(v) => save({ stop_all_on_exit: v })}
          />
        </Form.Item>
      </Form>
    </Drawer>
  );
}
