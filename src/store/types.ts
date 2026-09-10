import type {
  AppConfig,
  DaemonHello,
  DaemonProcessInfo,
  LogLine,
  Project,
  Service,
  ServiceRuntime,
} from "../types";

export interface LogBuffer {
  lines: LogLine[];
  hasUnread: boolean;
}

// ---- 各域切片状态 ----

export interface ServicesSlice {
  projects: Project[];
  services: Service[];
  runtimes: Record<string, ServiceRuntime>;
  config: AppConfig;
  refreshServices: () => Promise<void>;
  updateConfig: (cfg: AppConfig) => Promise<void>;
  removeProject: (projectId: string) => void;
  removeService: (serviceId: string) => void;
}

export interface LogsSlice {
  logs: Record<string, LogBuffer>;
  /** 按服务记录是否暂停日志显示（暂停期间日志仍缓存，但不触发 UI 更新） */
  paused: Record<string, boolean>;
  /** 日志批量 flush 的版本号，暂停恢复时递增以强制刷新订阅 */
  logFlushTick: number;
  appendLog: (log: LogLine) => void;
  clearLog: (serviceId: string) => void;
  markRead: (serviceId: string) => void;
  togglePause: (serviceId: string) => void;
}

export interface DaemonSlice {
  /** daemon 连接状态 */
  daemonConnected: boolean;
  /** daemon 握手信息 */
  daemonHello: DaemonHello | null;
  /** daemon 托管进程实时事实（run_id 键） */
  daemonProcesses: DaemonProcessInfo[];
  /** 最近一次指标刷新时间戳，UI 判断数据新鲜度 */
  daemonMetricsAt: number | null;
  setDaemonConnected: (connected: boolean) => void;
  setDaemonHello: (hello: DaemonHello | null) => void;
  setDaemonProcesses: (list: DaemonProcessInfo[]) => void;
  updateDaemonMetrics: (
    runId: number,
    cpu: number | null,
    mem: number | null,
  ) => void;
  refreshDaemon: () => Promise<void>;
}

export interface UiSlice {
  selectedServiceId: string | null;
  /** 已打开的日志 Tab (IDE-like)，按打开顺序 */
  openedTabs: string[];
  loading: boolean;
  /** 初始化失败时的错误信息，UI 据此展示提示 */
  initError: string | null;
  selectService: (id: string | null) => void;
  closeTab: (id: string) => void;
}

export type Store = ServicesSlice &
  LogsSlice &
  DaemonSlice &
  UiSlice & {
    init: () => Promise<void>;
    setRuntime: (rt: ServiceRuntime) => void;
  };
