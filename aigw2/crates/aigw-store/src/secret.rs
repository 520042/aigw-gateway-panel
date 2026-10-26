//! 凭据的落盘加密（Windows DPAPI）。
//!
//! 磁盘上写成 `dpapi:<base64(CryptProtectData 的 blob)>`；内存里**始终是明文**
//! ——中继要拿明文去拼 Authorization，面板掩码另有一套（`rows_snapshot` 不含 secret）。
//!
//! 三种用途各用一条附加熵（`wrap_with` / `unwrap_with`）：账号池 `accounts`、
//! 自定义 API `custom`、面板令牌 `panel`。熵不同意味着**一个文件的密文不能冒充
//! 另一个文件**：把 panel-token.txt 的内容粘进 custom.json 也解不开。
//! 默认的 `wrap`/`unwrap` 仍是 `accounts`（历史 blob 的熵是它，不能改）。
//!
//! DPAPI 的密钥由 Windows 绑定到「当前用户 + 本机」，所以把
//! `%LOCALAPPDATA%\aigw2\data\accounts.json` 拷到别的机器 / 别的 Windows 用户下
//! 是解不开的 —— 这正是加固的目标（以前这个文件是纯明文，任何进程都能直接读走
//! 110 个账号的 cookie / API Key）。
//!
//! 两条失败语义刻意分开：
//! - **加密失败**（非 Windows、DPAPI 调用报错）→ fail-open，退回明文落盘。
//!   加密是加固不是可用性前提，不能因为加密坏了就让人连登录都用不了。
//! - **解密失败**（已有 `dpapi:` blob 但解不开：换机器/换用户/文件被改）→ 返回 Err，
//!   由账号池把该账号标记成「需要重新登录」。绝不能把密文当凭据发给上游。

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

/// 落盘前缀。没有这个前缀的就是历史明文（旧版本写的 / legacy 导入的），原样使用。
pub const PREFIX: &str = "dpapi:";

pub fn is_wrapped(s: &str) -> bool {
    s.starts_with(PREFIX)
}

/// 用途 → DPAPI 附加熵。改这里的字符串等于让已有密文全部解不开，别动。
fn entropy(purpose: &str) -> Vec<u8> {
    format!("aigw2/{purpose}/v1").into_bytes()
}

/// 账号 secret 的落盘形式（历史默认用途）。
pub fn wrap(plain: &str) -> String {
    wrap_with(plain, "accounts")
}

/// 账号 secret 的还原（历史默认用途）。
pub fn unwrap(stored: &str) -> Result<String, String> {
    unwrap_with(stored, "accounts")
}

/// 指定用途：明文 → 落盘形式。成功返回 `dpapi:<b64>`。
///
/// 空串、非 Windows、DPAPI 调用失败时**原样返回**（fail-open：加密是加固，不是可用性前提）。
/// **已经是 `dpapi:` 的不再二次加密**——否则「读进来解不开、原样再存一次」会把密文
/// 包成密文的密文，永远解不回来。
pub fn wrap_with(plain: &str, purpose: &str) -> String {
    if plain.is_empty() || is_wrapped(plain) {
        return plain.to_string();
    }
    match platform::protect(plain.as_bytes(), &entropy(purpose)) {
        Some(blob) => format!("{PREFIX}{}", B64.encode(blob)),
        None => plain.to_string(),
    }
}

/// 指定用途：落盘形式 → 明文。
///
/// 历史明文原样返回（升级不能让已有凭据失效）；`dpapi:` 打头但解不开时返回 Err(人话原因)，
/// **绝不把密文当凭据返回**——那会变成上游 401 谜团。
pub fn unwrap_with(stored: &str, purpose: &str) -> Result<String, String> {
    if !is_wrapped(stored) {
        return Ok(stored.to_string());
    }
    let b64 = &stored[PREFIX.len()..];
    let raw = B64
        .decode(b64)
        .map_err(|e| format!("密文不是合法 base64（{e}），文件可能被改过"))?;
    match platform::unprotect(&raw, &entropy(purpose)) {
        Some(bytes) => String::from_utf8(bytes).map_err(|_| "解密结果不是 UTF-8 文本".to_string()),
        None => Err(platform::why_failed()),
    }
}

/// 常数时间字符串比较（长度先比，逐字节异或累积）。
///
/// 为什么不用 `==`：`str` 的相等判断一遇到不同的字节就返回，比较耗时随
/// 「前多少个字符相同」变化。密钥是从网络来的，能测响应时间的攻击者可以
/// 一个字节一个字节地把 key 试出来。这里全程走完再判结果。
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod ct_tests {
    use super::ct_eq;

    #[test]
    fn ct_eq_matches_only_identical_strings() {
        assert!(ct_eq("sk-abc", "sk-abc"));
        assert!(!ct_eq("sk-abc", "sk-abd"));
        assert!(!ct_eq("sk-abc", "sk-ab"), "长度不同必须不等");
        assert!(!ct_eq("", "a"));
        assert!(ct_eq("", ""), "两个空串相等（由调用方保证不拿空串当 key）");
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };

    /// 附加熵：DPAPI 的 entropy 参与密钥派生，别的程序即便也用 DPAPI，
    /// 拿不到这串常量就解不开我们的 blob（防止同用户下的泛化抓取工具直接复用）。
    /// 具体取值由 `super::entropy(purpose)` 生成，按用途区分。
    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            // DPAPI 只读入参，不会改这块内存，但 FFI 签名要求 *mut。
            pbData: data.as_ptr() as *mut u8,
        }
    }

    /// 把 DPAPI 输出的缓冲区拷出来并 LocalFree，避免每次登录都漏一块堆内存。
    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Option<Vec<u8>> {
        if out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as *mut core::ffi::c_void);
        Some(v)
    }

    pub(super) fn protect(data: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let (mut inb, mut ent) = (blob(data), blob(entropy));
            let mut out = CRYPT_INTEGER_BLOB::default();
            let ok = CryptProtectData(
                &mut inb,
                std::ptr::null(),
                &mut ent,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 {
                tracing::warn!("DPAPI 加密失败（{:?}）：本机将退回明文落盘", std::io::Error::last_os_error());
                return None;
            }
            take(out)
        }
    }

    pub(super) fn unprotect(data: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let (mut inb, mut ent) = (blob(data), blob(entropy));
            let mut out = CRYPT_INTEGER_BLOB::default();
            let ok = CryptUnprotectData(
                &mut inb,
                std::ptr::null_mut(),
                &mut ent,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            );
            if ok == 0 {
                return None;
            }
            take(out)
        }
    }

    pub(super) fn why_failed() -> String {
        let e = std::io::Error::last_os_error();
        format!(
            "DPAPI 解密失败（{e}）。凭据是在别的 Windows 用户/别的机器上加密的，本机解不开：请重新登录这个账号（或从原机器导出数据后再覆盖 accounts.json）"
        )
    }
}

#[cfg(not(windows))]
mod platform {
    pub(super) fn protect(_data: &[u8], _entropy: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub(super) fn unprotect(_data: &[u8], _entropy: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub(super) fn why_failed() -> String {
        "当前系统没有 DPAPI（凭据是在 Windows 上加密的），请回到原机器使用或重新登录".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_passes_through() {
        // 旧版本写的明文必须继续可用：升级不能把已有账号变成「解不开」。
        assert_eq!(unwrap("rt-abc-明文").unwrap(), "rt-abc-明文");
        assert_eq!(unwrap("").unwrap(), "");
    }

    #[cfg(windows)]
    #[test]
    fn roundtrip_is_wrapped_and_hides_plaintext() {
        let secret = "sk-live-0123456789abcdef";
        let stored = wrap(secret);
        assert!(is_wrapped(&stored), "落盘必须是 dpapi: 前缀：{stored}");
        assert!(!stored.contains(secret), "落盘不能出现明文");
        assert_eq!(unwrap(&stored).unwrap(), secret);
        // 两次加密同一个明文得到不同 blob（DPAPI 带随机盐），但都能解回原文。
        let again = wrap(secret);
        assert_ne!(stored, again);
        assert_eq!(unwrap(&again).unwrap(), secret);
    }

    #[test]
    fn corrupted_blob_is_an_error_not_a_credential() {
        // 解不开就必须报错：绝不能把密文当凭据发出去（那会变成上游 401 谜团）。
        assert!(unwrap("dpapi:not-base64!!").is_err());
        assert!(unwrap("dpapi:AAAA").is_err());
    }

    #[test]
    fn empty_secret_stays_empty() {
        assert_eq!(wrap(""), "");
        assert!(!is_wrapped(&wrap("")));
    }

    #[test]
    fn wrapping_is_idempotent_so_a_broken_blob_is_never_double_wrapped() {
        // 读进来解不开的密文会被原样保留并被再存一次：那时绝不能把它当明文再加密一遍，
        // 否则磁盘上是「密文的密文」，永远解不回来。
        let once = "dpapi:AAAA";
        assert_eq!(wrap_with(once, "custom"), once);
        assert_eq!(wrap(once), once);
    }

    #[cfg(windows)]
    #[test]
    fn purposes_are_not_interchangeable() {
        // 面板令牌的密文冒充自定义 API 的 Key、或反过来，都必须解不开：
        // 熵不同 → DPAPI 直接失败，而不是悄悄返回一串能用的字节。
        let blob = wrap_with("panel-token-value", "panel");
        assert!(is_wrapped(&blob));
        assert!(unwrap_with(&blob, "custom").is_err(), "跨用途必须解不开");
        assert!(unwrap(&blob).is_err(), "panel 密文不能被 accounts 解开");
        assert_eq!(unwrap_with(&blob, "panel").unwrap(), "panel-token-value");
    }

    #[test]
    fn legacy_plaintext_needs_no_purpose_match() {
        // 历史明文没有熵的概念：任何用途都要能原样读出来，否则升级会丢凭据。
        assert_eq!(unwrap_with("sk-legacy", "custom").unwrap(), "sk-legacy");
        assert_eq!(unwrap_with("sk-legacy", "panel").unwrap(), "sk-legacy");
    }
}
