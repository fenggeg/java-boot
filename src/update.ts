/**
 * 应用更新服务层
 *
 * - 检查更新：已接入后端（GitHub Releases 格式）
 * - 下载 / 安装：暂为前端模拟，后端接入后替换（见函数 TODO）
 */

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

function formatDate(iso: string): string {
  return iso ? iso.slice(0, 10) : "";
}

/**
 * 检查更新：请求后端 latest 接口（GitHub Releases 格式），
 * 与当前版本比较后映射为 UpdateInfo
 */
export async function checkForUpdate(): Promise<UpdateInfo> {
  const current = await currentVersion();
  const res = await fetch(UPDATE_API, {
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
 * 下载更新包
 * TODO(backend): 用 download_url（UpdateInfo 中已返回）实现真实下载，
 * 进度通过 onProgress 回调上报（0-100），例如 invoke + 事件流
 */
export async function downloadAndInstall(
  onProgress: (percent: number) => void
): Promise<void> {
  let p = 0;
  while (p < 100) {
    p = Math.min(100, p + 3 + Math.round(Math.random() * 9));
    onProgress(p);
    await new Promise((r) => setTimeout(r, 90 + Math.random() * 130));
  }
}

/**
 * 重启应用并安装更新
 * TODO(backend): 替换为 Tauri relaunch（tauri-plugin-process）
 */
export async function relaunchAndInstall(): Promise<void> {
  await new Promise((r) => setTimeout(r, 200));
}
