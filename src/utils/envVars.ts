/**
 * 环境变量 & 覆盖属性：公共解析 / 序列化逻辑
 *
 * ServiceConfigModal 和 ProjectConfigModal 共享同一套逻辑，
 * 提取到此避免重复代码 & ID 碰撞。
 */

/** 带唯一 id 的环境变量条目（id 仅前端使用，不序列化） */
export interface EnvVarEntry {
  id: string;
  key: string;
  value: string;
}

/** 带唯一 id 的覆盖属性条目（id 仅前端使用，不序列化） */
export interface OverrideEntry {
  id: string;
  key: string;
  value: string;
}

// 统一 ID 计数器，避免两个组件各自维护独立计数器导致 ID 碰撞
let idCounter = 0;

function nextId(prefix: string): string {
  idCounter += 1;
  return `${prefix}-${idCounter}`;
}

// ── 环境变量 ──────────────────────────────────

export function parseEnvVars(json: string | null | undefined): EnvVarEntry[] {
  if (!json || !json.trim()) return [];
  try {
    const arr = JSON.parse(json) as unknown;
    if (!Array.isArray(arr)) return [];
    return arr
      .filter(
        (x): x is Record<string, unknown> =>
          !!x && typeof x === "object" && typeof x.key === "string" && (x.key as string).trim().length > 0,
      )
      .map((x) => ({
        id: nextId("env"),
        key: (x.key as string).trim(),
        value: String(x.value ?? ""),
      }));
  } catch {
    return [];
  }
}

export function serializeEnvVars(list: EnvVarEntry[]): string | null {
  const cleaned = list
    .filter((x) => x.key.trim())
    .map(({ key, value }) => ({ key, value }));
  if (cleaned.length === 0) return null;
  return JSON.stringify(cleaned);
}

// ── 覆盖属性 ──────────────────────────────────

export function parseOverrideProperties(
  json: string | null | undefined,
): OverrideEntry[] {
  if (!json || !json.trim()) return [];
  try {
    const arr = JSON.parse(json) as unknown;
    if (!Array.isArray(arr)) return [];
    return arr
      .filter(
        (x): x is Record<string, unknown> =>
          !!x && typeof x === "object" && typeof x.key === "string" && (x.key as string).trim().length > 0,
      )
      .map((x) => ({
        id: nextId("ovr"),
        key: (x.key as string).trim(),
        value: String(x.value ?? ""),
      }));
  } catch {
    return [];
  }
}

export function serializeOverrideProperties(list: OverrideEntry[]): string | null {
  const cleaned = list
    .filter((x) => x.key.trim())
    .map(({ key, value }) => ({ key, value }));
  if (cleaned.length === 0) return null;
  return JSON.stringify(cleaned);
}
