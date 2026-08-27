/**
 * 应用更新服务层
 *
 * - 检查更新：请求后端接口（GitHub Releases 格式），经 Tauri http 插件发出
 * - 下载：invoke download_update，Rust reqwest 流式落盘，进度经
 *   update://progress 事件上报
 * - 安装：invoke install_update 启动 NSIS 静默安装器（/S /R）后退出当前进程
 */
import {invoke} from "@tauri-apps/api/core";
import {listen, type UnlistenFn} from "@tauri-apps/api/event";
import {fetch as tauriFetch} from "@tauri-apps/plugin-http";

export interface UpdateInfo {
  /** 是否有可用更新 */
  available: boolean;
  /** 当前版本 */
  current_version: string;
  /** 新版本号（available 时有效） */
  version: string;
  /** 发布日期（available 时有效） */
  pub_date: string;
  /** 安装包大小（展示用） */
  download_size: string;
  /** 更新日志（markdown 格式） */
  notes: string;
  /** 安装包下载地址（供后端下载使用） */
  download_url: string;
  /** 发布页地址 */
  release_url: string;
}

const UPDATE_API = "https://node-red.gyfwork.cc.cd/api/get/java-boot";

const FALLBACK_VERSION = "0.1.0";

async function currentVersion(): Promise<string> {
  try {
    const { getVersion } = await import("@tauri-apps/api/app");
    return await getVersion();
  } catch {
    return FALLBACK_VERSION;
  }
}

// ---- GitHub Releases 响应结构（仅取所需字段） ----

interface GithubReleaseAsset {
  name: string;
  size: number;
  browser_download_url: string;
}

interface GithubRelease {
  tag_name: string;
  name: string;
  html_url: string;
  published_at: string;
  body: string | null;
  draft: boolean;
  prerelease: boolean;
  assets: GithubReleaseAsset[];
}

// ---- 工具函数 ----

function parseVersion(v: string): number[] {
  return v
    .replace(/^v/i, "")
    .split(".")
    .map((x) => parseInt(x, 10) || 0);
}

/** 语义化版本比较：remote 是否比 current 更新 */
function isNewerVersion(remote: string, current: string): boolean {
  const a = parseVersion(remote);
  const b = parseVersion(current);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const diff = (a[i] ?? 0) - (b[i] ?? 0);
    if (diff !== 0) return diff > 0;
  }
  return false;
}

/** 从 assets 中挑选安装包（排除签名/清单等辅助文件） */
function pickInstallerAsset(
  assets: GithubReleaseAsset[]
): GithubReleaseAsset | null {
  const candidates = assets.filter(
    (a) =>
      a.name !== "latest.json" &&
      !a.name.endsWith(".sig") &&
      !a.name.endsWith(".blockmap")
  );
  return (
    candidates.find((a) => /\.exe$/i.test(a.name)) ??
    candidates.find((a) => /\.msi$/i.test(a.name)) ??
    candidates[0] ??
    null
  );
}

function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}

export {formatSize};

/** 后端 download_update 推送的下载进度（update://progress 事件） */
export interface UpdateDownloadProgress {
  /** 百分比 0-100（total 未知时为 0） */
  percent: number;
  /** 已下载字节数 */
  downloaded: number;
  /** 总字节数（未知为 0） */
  total: number;
  /** 实时速度（字节/秒，后端 EMA 平滑） */
  speed: number;
}

function formatDate(iso: string): string {
  return iso ? iso.slice(0, 10) : "";
}

/**
 * 检查更新：请求后端 latest 接口（GitHub Releases 格式），
 * 与当前版本比较后映射为 UpdateInfo
 */
export async function checkForUpdate(): Promise<UpdateInfo> {
  const current = await currentVersion();
  // 使用 Tauri http 插件：请求走 Rust 侧，不受 webview CORS 约束
  const res = await tauriFetch(UPDATE_API, {
    headers: { Accept: "application/json" },
  });
  if (!res.ok) {
    throw new Error(`服务器返回 ${res.status}`);
  }
  const release: GithubRelease = await res.json();
  if (release.draft || release.prerelease) {
    // 预发布/草稿不作为正式更新推送
    return {
      available: false,
      current_version: current,
      version: release.tag_name,
      pub_date: formatDate(release.published_at),
      download_size: "",
      notes: "",
      download_url: "",
      release_url: release.html_url,
    };
  }
  const asset = pickInstallerAsset(release.assets ?? []);
  return {
    available: isNewerVersion(release.tag_name, current),
    current_version: current,
    version: release.tag_name.replace(/^v/i, ""),
    pub_date: formatDate(release.published_at),
    download_size: asset ? formatSize(asset.size) : "",
    notes: release.body ?? "",
    download_url: asset?.browser_download_url ?? "",
    release_url: release.html_url,
  };
}

/**
 * 下载更新包：后端流式下载，进度经 update://progress 事件上报
 * （百分比 / 已下载字节 / 总大小 / 实时速度），
 * 返回安装包落盘路径（供 install_update 使用）
 */
export async function downloadAndInstall(
  downloadUrl: string,
  onProgress: (p: UpdateDownloadProgress) => void
): Promise<string> {
  if (!downloadUrl) {
    throw new Error("更新包下载地址为空");
  }
  const unlisten: UnlistenFn = await listen<UpdateDownloadProgress>(
    "update://progress",
    (event) => onProgress(event.payload)
  );
  try {
    return await invoke<string>("download_update", {url: downloadUrl});
  } finally {
    unlisten();
  }
}

/**
 * 取消正在进行的下载
 *
 * 后端触发取消令牌，`download_update` 检测到后删除半成品文件
 * 并返回"下载已取消"错误，`downloadAndInstall` 的 Promise 随之 reject。
 * 无下载任务时为空操作。
 */
export async function cancelUpdate(): Promise<void> {
  await invoke("cancel_update");
}

/**
 * 重启应用并完成安装：
 * 启动 NSIS 静默安装器（/S /R），当前进程随即退出，
 * 安装器覆盖文件后自动拉起新版本
 */
export async function relaunchAndInstall(installerPath: string): Promise<void> {
  if (!installerPath) {
    throw new Error("尚未下载更新包");
  }
  await invoke("install_update", {path: installerPath});
}
