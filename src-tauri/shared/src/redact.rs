//! 敏感环境变量脱敏。
//!
//! 键名匹配敏感词列表（大小写不敏感）的键，其值在**持久化/上报**前替换为
//! `«redacted»`。此外，对 JDBC URL、连接串等已知含明文密码的**值格式**，
//! 无论键名是否命中，都会对值内嵌的密码部分做脱敏。
//! 脱敏只在写库与对外展示边界做，内存中启动进程所需的明文仍由调用方单独持有，
//! 二者互不干扰。

/// 敏感键名匹配词。命中任意一个即视为敏感。
const SENSITIVE_SUBSTRINGS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "TOKEN",
    "KEY",
    "CREDENTIAL",
    "PRIVATE",
    "CERT",
    "AUTH",
    "BEARER",
    "ACCESS",
    "APPSECRET",
    "APIKEY",
    "API_KEY",
];

/// 非敏感键名白名单：虽含敏感词但实际为路径/配置等非敏感字段。
/// 命中此处且**仅命中此处**（不含其他敏感词）时不脱敏。
const NON_SENSITIVE_KEYS: &[&str] = &[
    "KEYSTORE_PATH",
    "KEYSTORE_TYPE",
    "TRUSTSTORE_PATH",
    "TRUSTSTORE_TYPE",
    "KEY_PATH",
    "CERT_PATH",
    "ACCESS_LOG",
    "ACCESS_LOG_PATH",
    "AUTH_SERVER_URL",
    "AUTH_SERVER_HOST",
    "TOKEN_ENDPOINT",
    "TOKEN_URL",
    "TOKEN_ISSUER",
    "SECRET_KEY_REF",
];

/// 脱敏替换值。
pub const REDACTED: &str = "«redacted»";

/// 判断某个环境变量键是否为敏感键。
pub fn is_sensitive_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();

    // 白名单优先：若键完全匹配白名单且不含其他敏感词组合，则不脱敏。
    // 白名单条目本身含敏感词（如 KEYSTORE_PATH 含 KEY），需显式排除。
    if NON_SENSITIVE_KEYS.iter().any(|s| upper == *s) {
        return false;
    }

    SENSITIVE_SUBSTRINGS.iter().any(|s| upper.contains(s))
}

/// 对值内容中已知含明文密码的格式做脱敏。
///
/// 覆盖场景：
/// - JDBC URL：`jdbc:mysql://user:password@host` → `jdbc:mysql://user:«redacted»@host`
/// - 通用连接串：`protocol://user:password@host` → `protocol://user:«redacted»@host`
/// - Spring 属性内嵌：`spring.datasource.password=xxx`（值中含 `password=`）
///
/// 纯字符串匹配实现，不引入正则依赖，保持 shared crate 零额外依赖。
pub fn redact_value_content(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }

    let mut result = value.to_string();

    // 1) URL 内嵌凭证：scheme://user:password@host
    //    查找 `://` 后第一个 `:` 到 `@` 之间的内容，替换为脱敏值。
    if let Some(scheme_end) = result.find("://") {
        let after_scheme = &result[scheme_end + 3..];
        if let Some(at_pos) = after_scheme.find('@') {
            let cred_section = &after_scheme[..at_pos];
            if let Some(colon_pos) = cred_section.find(':') {
                let abs_colon = scheme_end + 3 + colon_pos;
                let abs_at = scheme_end + 3 + at_pos;
                let password_part = &result[abs_colon + 1..abs_at];
                if !password_part.is_empty() {
                    result = format!(
                        "{}{}{}",
                        &result[..abs_colon + 1],
                        REDACTED,
                        &result[abs_at..]
                    );
                }
            }
        }
    }

    // 2) 值中内嵌 `password=xxx` / `secret=xxx` / `token=xxx` 等键值对
    //    如 `spring.datasource.password=xxx` 作为整体值出现时
    let inline_keys = [
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "credential",
        "apikey",
        "api_key",
    ];
    loop {
        let lower = result.to_ascii_lowercase();
        let mut found = None;
        for ik in inline_keys {
            let pattern = format!("{}=", ik);
            if let Some(idx) = lower.find(&pattern) {
                found = Some((idx, pattern.len()));
                break;
            }
        }
        match found {
            None => break,
            Some((idx, pat_len)) => {
                let value_start = idx + pat_len;
                let rest = &result[value_start..];
                let end = rest
                    .find(|c: char| c == '&' || c == ';' || c == ',' || c.is_whitespace())
                    .unwrap_or(rest.len());
                if end == 0 {
                    break;
                }
                result = format!(
                    "{}{}{}",
                    &result[..value_start],
                    REDACTED,
                    &result[value_start + end..]
                );
            }
        }
    }

    result
}

/// 对单个键值对执行脱敏：敏感键返回脱敏值，否则检查值内容。
pub fn redact_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) {
        REDACTED.to_string()
    } else {
        redact_value_content(value)
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
            "PRIVATE_KEY",
            "CLIENT_SECRET",
            "AUTH_TOKEN",
            "BEARER_TOKEN",
            "ACCESS_KEY",
            "APPSECRET",
            "APIKEY",
            "PASSPHRASE",
            "CERT_PASSWORD",
            "DB_PASSWD",
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
            "KEYSTORE_PATH",
            "KEYSTORE_TYPE",
            "TRUSTSTORE_PATH",
            "CERT_PATH",
            "ACCESS_LOG",
            "ACCESS_LOG_PATH",
            "AUTH_SERVER_URL",
            "TOKEN_ENDPOINT",
            "TOKEN_ISSUER",
        ] {
            assert!(!is_sensitive_key(key), "不应判为敏感: {key}");
            assert_eq!(redact_value(key, "value"), "value");
        }
    }

    #[test]
    fn redact_map_mixes() {
        let m = redact_map([
            ("DB_PASSWORD", "hunter2"),
            ("PORT", "8080"),
            ("API_KEY", "sk-123"),
        ]);
        assert_eq!(m["DB_PASSWORD"], REDACTED);
        assert_eq!(m["API_KEY"], REDACTED);
        assert_eq!(m["PORT"], "8080");
    }

    #[test]
    fn redacts_jdbc_url_embedded_password() {
        // 非敏感键但值内嵌明文密码
        let v = redact_value(
            "SPRING_DATASOURCE_URL",
            "jdbc:mysql://root:hunter2@localhost:3306/mydb",
        );
        assert!(
            v.contains(REDACTED),
            "JDBC URL 内嵌密码应被脱敏: {v}"
        );
        assert!(
            !v.contains("hunter2"),
            "明文密码不应残留: {v}"
        );
        assert!(
            v.contains("localhost:3306"),
            "主机端口信息应保留: {v}"
        );
    }

    #[test]
    fn redacts_generic_connection_string_password() {
        let v = redact_value(
            "CUSTOM_URL",
            "redis://admin:s3cr3t@redis-host:6379/0",
        );
        assert!(v.contains(REDACTED), "连接串密码应被脱敏: {v}");
        assert!(!v.contains("s3cr3t"), "明文密码不应残留: {v}");
    }

    #[test]
    fn redacts_inline_password_kv_in_value() {
        // 值中内嵌 password=xxx 形式
        let v = redact_value(
            "SPRING_OPTS",
            "spring.datasource.password=mySecret123",
        );
        assert!(v.contains(REDACTED), "内嵌 password= 应被脱敏: {v}");
        assert!(!v.contains("mySecret123"), "明文不应残留: {v}");
    }

    #[test]
    fn redacts_inline_token_kv_in_value() {
        let v = redact_value(
            "EXTRA_OPTS",
            "token=abc-123-xyz&other=keep",
        );
        assert!(v.contains(REDACTED), "内嵌 token= 应被脱敏: {v}");
        assert!(v.contains("other=keep"), "非敏感部分应保留: {v}");
    }

    #[test]
    fn keeps_plain_url_without_credentials() {
        let v = redact_value(
            "SPRING_DATASOURCE_URL",
            "jdbc:mysql://localhost:3306/mydb",
        );
        assert_eq!(
            v, "jdbc:mysql://localhost:3306/mydb",
            "无凭证的 URL 不应被修改"
        );
    }

    #[test]
    fn keeps_empty_value() {
        assert_eq!(redact_value("SOME_KEY", ""), "");
    }
}