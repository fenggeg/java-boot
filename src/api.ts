import {invoke} from "@tauri-apps/api/core";
import type {
    AppConfig,
    BatchStartResult,
    BatchPackageResult,
    DaemonHello,
    DaemonProcessInfo,
    JdkInfo,
    MavenInfo,
    PackageResult,
    Project,
    ScannedModule,
    Service,
    ServiceRuntime,
} from "./types";

// ============================ Project ============================

export const listProjects = () => invoke<Project[]>("list_projects");
export const listServices = () => invoke<Service[]>("list_services");
export const scanProject = (path: string) =>
  invoke<ScannedModule[]>("scan_project", { path });
export const addProject = (path: string, selectedModules: ScannedModule[]) =>
  invoke<Project>("add_project", { path, selectedModules });
export const rescanProject = (projectId: string) =>
  invoke<ScannedModule[]>("rescan_project", { projectId });
export const deleteProject = (projectId: string) =>
  invoke<void>("delete_project", { projectId });

// ============================ Service ============================

export const addService = (pomPath: string, name?: string) =>
  invoke<Service>("add_service", { pomPath, name: name ?? null });
export const updateService = (
  id: string,
  name?: string,
  autoRestart?: boolean,
  mavenOpts?: string | null,
  profiles?: string | null,
  devMode?: boolean,
  mainClass?: string | null,
  overrideProperties?: string | null,
  envVars?: string | null,
) =>
  invoke<void>("update_service", {
    id,
    name: name ?? null,
    autoRestart: autoRestart ?? null,
    mavenOpts: mavenOpts ?? null,
    profiles: profiles ?? null,
    devMode: devMode ?? null,
    mainClass: mainClass ?? null,
    overrideProperties: overrideProperties ?? null,
    envVars: envVars ?? null,
  });

/// 更新项目级 JDK / Maven / 环境变量配置
export const updateProjectEnv = (
  projectId: string,
  javaHome?: string | null,
  mavenHome?: string | null,
  envVars?: string | null,
) =>
  invoke<void>("update_project_env", {
    projectId,
    javaHome: javaHome ?? null,
    mavenHome: mavenHome ?? null,
    envVars: envVars ?? null,
  });
export const deleteService = (id: string) =>
  invoke<void>("delete_service", { id });
export const toggleAutoRestart = (id: string, enabled: boolean) =>
  invoke<void>("toggle_auto_restart", { id, enabled });

// ============================ Process ============================

export const startService = (id: string) => invoke<void>("start_service", { id });
export const stopService = (id: string) => invoke<void>("stop_service", { id });
export const restartService = (id: string) =>
  invoke<void>("restart_service", { id });
export const compileAndStart = (id: string) =>
  invoke<void>("compile_and_start", { id });
export const recompileAndStart = (id: string) =>
  invoke<void>("recompile_and_start", { id });
export const cleanService = (id: string) =>
  invoke<void>("clean_service", { id });
export const stopAll = () => invoke<void>("stop_all");

/// 打包单个服务（mvn clean package -DskipTests），生成可执行 jar
export const packageService = (id: string) =>
  invoke<PackageResult>("package_service", { id });

/// 批量打包项目下所有已添加的服务（逐个打包）
export const packageProjectServices = (ids: string[]) =>
  invoke<BatchPackageResult>("package_project_services", { ids });

// 带依赖启动
export const startServiceWithDependencies = (id: string) =>
  invoke<void>("start_service_with_dependencies", { id });

// 批量启动（一键启动项目下所有服务）
export const startServicesBatch = (ids: string[]) =>
  invoke<BatchStartResult>("start_services_batch", { ids });

// 服务依赖编排
export const getServiceDependencies = (id: string) =>
  invoke<string[]>("get_service_dependencies", { id });

export const setServiceDependencies = (id: string, dependsOnIds: string[]) =>
  invoke<void>("set_service_dependencies", { id, dependsOnIds });
export const getRuntime = (id: string) =>
  invoke<ServiceRuntime>("get_runtime", { id });
export const getAllRuntimes = () =>
  invoke<ServiceRuntime[]>("get_all_runtimes");
export const refreshPortConflicts = () => invoke<void>("refresh_port_conflicts");

// ============================ Daemon（P3 监控闭环） ============================

/** daemon 是否已连接（Tauri 命令层拉取） */
export const getDaemonConnected = () =>
  invoke<boolean>("daemon_connected");
/** daemon 握手信息（版本协商 / 摘要） */
export const getDaemonHello = () =>
  invoke<DaemonHello | null>("daemon_hello");
/** 对账：拉取 daemon 托管进程实时事实（含 CPU/内存指标） */
export const reconcileDaemon = () =>
  invoke<DaemonProcessInfo[]>("daemon_reconcile");

// ============================ Util（文件系统） ============================

/// 在系统文件管理器中定位显示指定绝对路径（打包产物 / target 目录）
export const revealPathInFileManager = (path: string) =>
  invoke<void>("reveal_path_in_file_manager", { path });

// ============================ Config ============================

export const getConfig = () => invoke<AppConfig>("get_config");
export const saveConfig = (config: AppConfig) =>
  invoke<void>("save_config", { config });

// ============================ 运行数据（软件自身数据目录） ============================

export interface RunDataResult {
  path: string;
  fileCount: number;
  totalBytes: number;
}

/** 查询软件自身运行期数据占用的总大小（日志镜像 / spec 快照） */
export const getRunDataUsage = () =>
  invoke<RunDataResult>("get_run_data_usage");
/** 一键清除软件自身运行期数据，返回清除后的剩余占用 */
export const clearRunData = () =>
  invoke<RunDataResult>("clear_run_data");

// ============================ Util ============================

export const openInBrowser = (port: number) =>
  invoke<void>("open_in_browser", { port });

export const detectJdks = () => invoke<JdkInfo[]>("detect_jdks");
export const detectMavens = () => invoke<MavenInfo[]>("detect_mavens");

// ============================ Error ============================

/**
 * 归一化错误信息：统一 TauriError / Error / string 的格式化输出
 */
export function toErrMsg(e: unknown): string {
  if (e === null || e === undefined) return "未知错误";
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  // Tauri 的 invoke 错误对象通常含 message 或直接是字符串
  if (typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

/** 后端主动取消的操作（用户点了停止）不算错误，避免弹出误导性的失败提示 */
export function isCancelledError(e: unknown): boolean {
  if (typeof e === "object" && e !== null && "kind" in e) {
    if ((e as { kind?: unknown }).kind === "cancelled") return true;
  }
  return toErrMsg(e).includes("操作已取消");
}
