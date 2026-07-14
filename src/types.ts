// 前端类型定义 — 与 Rust 端 models 对应

export interface Project {
  id: string;
  name: string;
  root_path: string;
  git_available: boolean;
  java_home: string | null;
  maven_home: string | null;
  created_at: string;
}

export interface Service {
  id: string;
  name: string;
  pom_path: string;
  working_dir: string;
  project_id: string | null;
  auto_restart: boolean;
  maven_opts: string | null;
  profiles: string | null;
  created_at: string;
}

export type ServiceStatus =
  | "stopped"
  | "starting"
  | "running"
  | "recompiling"
  | "pulling"
  | "error"
  | "stopping";

export interface ServiceRuntime {
  service_id: string;
  status: ServiceStatus;
  pid: number | null;
  ports: number[];
  started_at: string | null;
  port_conflict: boolean;
  conflict_with: string[];
  cpu_usage: number | null;
  memory_mb: number | null;
}

export interface AppConfig {
  port_refresh_interval_secs: number;
  stop_on_compile_fail: boolean;
  auto_restart_debounce_secs: number;
  log_buffer_lines: number;
  stop_all_on_exit: boolean;
}

export interface ScannedModule {
  artifact_id: string;
  pom_path: string;
  relative_path: string;
  packaging: string;
  is_service: boolean;
  already_added: boolean;
  children: ScannedModule[];
}

export interface PullResult {
  project_id: string;
  success: boolean;
  up_to_date: boolean;
  message: string;
}

export interface LogLine {
  service_id: string;
  source: string; // [app] [mvn] [git]
  line: string;
  ts: string;
}

export interface JdkInfo {
  path: string;
  version: string;
  vendor: string;
}

export interface MavenInfo {
  path: string;
  version: string;
}

export const STATUS_META: Record<
  ServiceStatus,
  { label: string; color: string; dot: string; live?: boolean }
> = {
  stopped: { label: "已停止", color: "default", dot: "#626771" },
  starting: { label: "启动中", color: "processing", dot: "#22d3ee", live: true },
  running: { label: "运行中", color: "success", dot: "#a3e635", live: true },
  recompiling: { label: "重新编译中", color: "processing", dot: "#fbbf24", live: true },
  pulling: { label: "拉取中", color: "processing", dot: "#c084fc", live: true },
  error: { label: "异常", color: "error", dot: "#f87171", live: true },
  stopping: { label: "停止中", color: "processing", dot: "#f97316", live: true },
};
