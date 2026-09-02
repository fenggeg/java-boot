//! 敏感环境变量脱敏。
//!
//! 键名匹配 `PASSWORD|SECRET|TOKEN|KEY|CREDENTIAL`（大小写不敏感）的键，
//! 其值在**持久化/上报**前替换为 `«redacted»`。脱敏只在写库与对外展示边界做，
//! 内存中启动进程所需的明文仍由调用方单独持有，二者互不干扰。

/// 敏感键名匹配词。命中任意一个即视为敏感。
const SENSITIVE_SUBSTRINGS: &[&str] = &[
    "PASSWORD",
    "SECRET",
    "TOKEN",
    "KEY",
    "CREDENTIAL",
];

/// 脱敏替换值。
pub const REDACTED: &str = "«redacted»";

/// 判断某个环境变量键是否为敏感键。
pub fn is_sensitive_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SENSITIVE_SUBSTRINGS.iter().any(|s| upper.contains(s))
}

/// 对单个键值对执行脱敏：敏感键返回脱敏值，否则原样返回。
pub fn redact_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) {
        REDACTED.to_string()
    } else {
        value.to_string()
    }
}

/// 对 `(key, value)` 迭代器整体脱敏，返回序列化友好的 JSON 对象。
pub fn redact_map<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert(
            k.to_string(),
            serde_json::Value::String(redact_value(k, v)),
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sensitive_keys_case_insensitive() {
        // 命中任一敏感词：大小写不敏感
        for key in [
            "PASSWORD",
            "DB_PASSWORD",
            "secret_key",
            "MySecretToken",
            "AWS_ACCESS_KEY_ID",
            "api_CREDENTIAL",
            "jwt-token",
        ] {
            assert!(is_sensitive_key(key), "应判为敏感: {key}");
            assert_eq!(redact_value(key, "plaintext"), REDACTED);
        }
    }

    #[test]
    fn keeps_non_sensitive_keys() {
        for key in [
            "JAVA_HOME",
            "MAVEN_HOME",
            "SPRING_PROFILES_ACTIVE",
            "KAFKA_BOOTSTRAP",
            "NAME",
        ] {
            assert!(!is_sensitive_key(key), "不应判为敏感: {key}");
            assert_eq!(redact_value(key, "value"), "value");
        }
    }

    #[test]
    fn redact_map_mixes() {
        let mut m = redact_map([
            ("DB_PASSWORD", "hunter2"),
            ("PORT", "8080"),
            ("API_KEY", "sk-123"),
        ]);
        assert_eq!(m["DB_PASSWORD"], REDACTED);
        assert_eq!(m["API_KEY"], REDACTED);
        assert_eq!(m["PORT"], "8080");
    }
}