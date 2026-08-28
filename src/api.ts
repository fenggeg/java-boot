import {invoke} from "@tauri-apps/api/core";
import type {
    AppConfig,
    BatchStartResult,
    BatchPackageResult,
    FileContent,
    FileEntry,
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

// ============================ Files（项目文件浏览/编辑） ============================

export const listFiles = (projectId: string, path: string) =>
  invoke<FileEntry[]>("list_files", { projectId, path });
export const readProjectFile = (projectId: string, path: string) =>
  invoke<FileContent>("read_project_file", { projectId, path });
export const writeProjectFile = (
  projectId: string,
  path: string,
  content: string,
) =>
  invoke<void>("write_project_file", { projectId, path, content });

export const getFileAbsPath = (projectId: string, path: string) =>
  invoke<string>("get_file_abs_path", { projectId, path });

/// 重命名文件 / 目录，返回新相对路径
export const fsRename = (projectId: string, path: string, newName: string) =>
  invoke<string>("fs_rename", { projectId, path, newName });

/// 复制文件 / 目录到目标目录，返回新路径
export const fsCopyEntry = (projectId: string, srcPath: string, destDir: string) =>
  invoke<string>("fs_copy_entry", { projectId, srcPath, destDir });

/// 移动文件 / 目录到目标目录，返回新路径
export const fsMoveEntry = (projectId: string, srcPath: string, destDir: string) =>
  invoke<string>("fs_move_entry", { projectId, srcPath, destDir });

/// 在系统文件管理器中定位该条目
export const revealInFileManager = (projectId: string, path: string) =>
  invoke<void>("reveal_in_file_manager", { projectId, path });

/// 在系统文件管理器中定位显示指定绝对路径（不依赖项目根目录）
export const revealPathInFileManager = (path: string) =>
  invoke<void>("reveal_path_in_file_manager", { path });

/** 项目内全量文件扁平条目（快速打开用，已排除依赖/构建目录与符号链接） */
export interface FlatFile {
  path: string;
  name: string;
}

/// 扁平遍历项目内全部文件（快速打开数据源，上限 5 万条）
export const walkFiles = (projectId: string) =>
  invoke<FlatFile[]>("walk_files", { projectId });

// ============================ Config ============================

export const getConfig = () => invoke<AppConfig>("get_config");
export const saveConfig = (config: AppConfig) =>
  invoke<void>("save_config", { config });

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
