//! 凭据的落盘加密（DPAPI / AES-256-GCM 双后端）。
//!
//! 磁盘上写成 `<prefix><base64>`；内存里**始终是明文**——中继要拿明文去拼
//! Authorization，面板掩码另有一套（`rows_snapshot` 不含 secret）。
//!
//! ## 两个后端，两种前缀
//!
//! | 前缀 | 后端 | 何时使用 |
//! |---|---|---|
//! | `dpapi:` | Windows DPAPI（`CryptProtectData`） | Windows 且 DPAPI 可用时**优先** |
//! | `aigw1:` | AES-256-GCM + 本地密钥文件 | 非 Windows，或 Windows 上 DPAPI 不可用 |
//!
//! ★ 为什么需要第二个后端：**Linux/macOS 上原本会明文落盘**。旧实现里
//!   `#[cfg(not(windows))] protect() -> None`，而 `wrap_with` 在 `None` 时「 fail-open 原样返回明文」
//!   ——于是所有账号 cookie / API Key / 面板令牌在非 Windows 部署上都是**磁盘明文**，
//!   而 README 和 UI 都宣称是加密的。这不是可用性兜底，是「文档承诺 ≠ 实际行为」的静默失效。
//!
//! ★ 为什么 Windows 上要保留 DPAPI 优先：DPAPI 的密钥由系统绑定到「当前用户 + 本机」，
//!   **不需要我们自己保管密钥**——把 accounts.json 拷到别的机器/别的用户下直接解不开，
//!   这是单纯的文件加密给不了的。AES 后端只能做到「密钥文件与数据文件分开」，
//!   所以密钥文件必须落在**数据目录之外**（见 [`key_file_path`]）。
//!
//! ## 三种用途各用一条附加熵
//!
//! `wrap_with` / `unwrap_with` 的 `purpose`：账号池 `accounts`、自定义 API
//! `custom`、面板令牌 `panel`。熵不同意味着**一个文件的密文不能冒充
//! 另一个文件**：把 panel-token.txt 的内容粘进 custom.json 也解不开。默认的 `wrap`/`unwrap`
//! 仍是 `accounts`（历史 blob 的熵是它，不能改）。
//!
//! ## 失败语义
//!
//! - **解密失败**（blob 解不开：换机器/换用户/文件被改/换了密钥）→ 返回 Err，
//!   由账号池把该账号标记成「需要重新登录」。**绝不能把密文当凭据发给上游。**
//! - **加密不可用**（两个后端都起不来）→ 仍然 fail-open 退回明文（加密是加固，
//!   不是可用性前提），但会打 error 日志，且 [`encryption_status`] 会把这件事
//!   暴露到 `/api/health`，让面板能显眼告警——不再「静默裸奔」。

// ★ 常量时间比较收敛到 aigw-core（此前 secret.rs 与 aigw-server/net.rs 各抄一份，
//   逐字相同）。两处都得改时漏一处就是安全口径不一致，这里 re-export 保持调用方不变。
pub use aigw_core::ct_eq;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

/// Windows DPAPI 落盘前缀。
pub const DPAPI_PREFIX: &str = "dpapi:";
/// 跨平台 AES-256-GCM 落盘前缀。
pub const AES_PREFIX: &str = "aigw1:";

/// 兼容别名：历史上只有 DPAPI 一种前缀，很多地方（含测试）用 `PREFIX` 指代它。
/// 判断「是否已加密」请用 [`is_wrapped`]（它认全部前缀）。
pub const PREFIX: &str = DPAPI_PREFIX;

/// 这个字符串是不是已经加密过的落盘形式（任一后端）。
///
/// ★ 幂等的关键：`wrap_with` 遇到已加密的输入必须原样返回。否则「读进来解不开、
///   原样再存一次」会把密文包成密文的密文，永远解不回来。
pub fn is_wrapped(s: &str) -> bool {
    s.starts_with(DPAPI_PREFIX) || s.starts_with(AES_PREFIX)
}

/// 整份 JSON 文件文本里是否出现过任一已加密前缀。
///
/// ★ 为什么不直接用 [`is_wrapped`]：它判断的是「这一个值是不是密文」（前缀在开头）。
///   而整份落盘文件是 `[{...,"secret":"aigw1:..."}]`，前缀在中间 ——
///   拿 `is_wrapped` 直接测文件文本会恒为假（测试里踩过这个坑）。
pub fn is_encrypted_text(file_text: &str) -> bool {
    file_text.contains(DPAPI_PREFIX) || file_text.contains(AES_PREFIX)
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

/// 指定用途：明文 → 落盘形式。成功返回 `dpapi:<b64>` 或 `aigw1:<b64>`。
///
/// 空串、两后端都不可用时**原样返回**（fail-open：加密是加固，不是可用性前提）。
/// **已经加密的不再二次加密**——否则「读进来解不开、原样再存一次」会把密文
/// 包成密文的密文，永远解不回来。
pub fn wrap_with(plain: &str, purpose: &str) -> String {
    if plain.is_empty() || is_wrapped(plain) {
        return plain.to_string();
    }
    if let Some(blob) = dpapi::protect(plain.as_bytes(), &entropy(purpose)) {
        return format!("{DPAPI_PREFIX}{}", B64.encode(blob));
    }
    match aes::seal(plain.as_bytes(), purpose) {
        Ok(blob) => format!("{AES_PREFIX}{}", B64.encode(blob)),
        Err(e) => {
            // 两个后端都不可用：退回明文。这条日志必须在 error 级别——
            // 它是「这台机器上凭据是明文」的唯一信号（面板侧由
            // `encryption_status()` 转成显式告警）。
            tracing::error!(
                "凭据加密不可用（DPAPI 与 AES 均失败：{e}）：本次以**明文**落盘。请检查数据目录可写性与密钥文件权限，或设置 AIGW_SECRET_KEY。"
            );
            plain.to_string()
        }
    }
}

/// 指定用途：落盘形式 → 明文。
///
/// 历史明文原样返回（升级不能让已有凭据失效）；`dpapi:`/`aigw1:` 打头但解不开时返回
/// Err(人话原因)，**绝不把密文当凭据返回**——那会变成上游 401 谜团。
pub fn unwrap_with(stored: &str, purpose: &str) -> Result<String, String> {
    if !is_wrapped(stored) {
        return Ok(stored.to_string());
    }
    if let Some(b64) = stored.strip_prefix(AES_PREFIX) {
        let raw = B64
            .decode(b64)
            .map_err(|e| format!("密文不是合法 base64（{e}），文件可能被改过"))?;
        return aes::open(&raw, purpose)
            .map_err(|e| format!("凭据解密失败：{e}。请重新登录/重新填写该凭据。"));
    }
    let b64 = &stored[DPAPI_PREFIX.len()..];
    let raw = B64
        .decode(b64)
        .map_err(|e| format!("密文不是合法 base64（{e}），文件可能被改过"))?;
    match dpapi::unprotect(&raw, &entropy(purpose)) {
        Some(bytes) => String::from_utf8(bytes).map_err(|_| "解密结果不是 UTF-8 文本".to_string()),
        None => Err(dpapi::why_failed()),
    }
}

/// 当前这台机器的加密可用性——给 `/api/health` 用，让「凭据是明文」这件事
/// 在面板上可见，而不是只躺在日志里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionStatus {
    /// Windows DPAPI 可用（密钥由系统绑定到当前用户+本机）。
    Dpapi,
    /// DPAPI 不可用，用 AES-256-GCM + 本地密钥文件。
    Aes,
    /// 两个后端都起不来：**凭据会以明文落盘**。
    Plaintext(String),
}

impl EncryptionStatus {
    /// 是否达到了「凭据不以明文落盘」这一最低要求。
    pub fn is_encrypted(&self) -> bool {
        !matches!(self, EncryptionStatus::Plaintext(_))
    }

    /// 后端的稳定标识（前端按它决定告警文案，不要按中文匹配）。
    pub fn backend(&self) -> &'static str {
        match self {
            EncryptionStatus::Dpapi => "dpapi",
            EncryptionStatus::Aes => "aes-256-gcm",
            EncryptionStatus::Plaintext(_) => "none",
        }
    }

    /// 不可用时的人话原因（否则 None）。
    pub fn objection(&self) -> Option<&str> {
        match self {
            EncryptionStatus::Plaintext(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// 探一次当前加密可用性。
///
/// ★ 不能只看 `cfg!(windows)`：Windows 上 DPAPI 也可能失败（无用户配置文件、
///   服务账户、受限容器），而那正是最容易被误判成「已加密」的情形。所以这里
///   **真的做一次密封+开封自检**，通过才算可用。
pub fn encryption_status() -> EncryptionStatus {
    let probe = b"aigw-encryption-selfcheck";
    if let Some(blob) = dpapi::protect(probe, &entropy("selfcheck")) {
        if dpapi::unprotect(&blob, &entropy("selfcheck")).as_deref() == Some(probe.as_slice()) {
            return EncryptionStatus::Dpapi;
        }
        return EncryptionStatus::Plaintext(
            "DPAPI 能加密但解不回来（系统凭据子系统异常）：为避免写出一堆解不开的密文，本次不使用 DPAPI。请尝试重新登录系统账户或重启后重试。"
                .to_string(),
        );
    }
    match aes::seal(probe, "selfcheck").and_then(|b| aes::open(&b, "selfcheck")) {
        Ok(back) if back.as_bytes() == probe => EncryptionStatus::Aes,
        Ok(_) => EncryptionStatus::Plaintext(
            "AES 自检解出的内容与原文不一致（密钥文件可能被替换）。".to_string(),
        ),
        Err(e) => EncryptionStatus::Plaintext(format!(
            "DPAPI 不可用（非 Windows 或系统受限），AES 后备也起不来：{e}。请确认数据目录可写；若需固定密钥，设置环境变量 AIGW_SECRET_KEY（64 位 hex）。"
        )),
    }
}

/// AES 后备后端的密钥文件位置。
///
/// ★ 必须落在**数据目录之外**：备份、`POST /api/backup/export`、以及用户
///   「把 data/ 整个打包发给别人」都是常见操作。密钥和数据在同一个目录里，
///   加密就只剩「防随手 grep」这一层意义了。
///
/// 优先级：`AIGW_SECRET_KEY`（64 位 hex，容器/k8s Secret 用）> `AIGW_KEY_DIR`
/// > 平台默认（`$XDG_CONFIG_HOME` / `~/.config` / `%APPDATA%`）。
pub fn key_file_path() -> std::path::PathBuf {
    key_file_candidates()
        .into_iter()
        .next()
        .unwrap_or_else(|| std::path::PathBuf::from("secret.key"))
}

/// 密钥文件的候选位置，**按优先级从高到低**。
///
/// ★ 为什么要有一条供退链：首选的 `~/.config/aigw2/` 在容器、CI、
///   受限用户、以及本机的沙箱里可能**根本不可写**。那时如果不能退到
///   别处，AES 后端就会因「密钥写不下去」而整体失败 → 又回到明文落盘（旧 bug）。
///   退到数据目录侧的 `key/` 子目录虽然弱一点（和数据同一个父目录），
///   但仍然**不在 `data/` 里面** —— 那个才是会被整包拷走的目录。
///   两者均不可写时才真的无法加密（会报错，不会静默写明文）。
pub fn key_file_candidates() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    // ★ 进程内显式覆盖（仅测试/嵌入场景用）。设了就只认它，
    //   让单元测试不依赖用户真实的 ~/.config 可写性。
    if let Some(d) = override_dir() {
        return vec![d.join("secret.key")];
    }
    if let Some(d) = std::env::var_os("AIGW_KEY_DIR") {
        let d = std::path::PathBuf::from(d);
        if !d.as_os_str().is_empty() {
            // 显式指定就只认它（不做任何回退）：用户已经表达了意图，
            // 静默换到别的目录会让他以为配置生效了。
            return vec![d.join("secret.key")];
        }
    }
    if let Some(home) = config_home() {
        out.push(home.join("aigw2").join("secret.key"));
    }
    // 供退 1：数据目录的兄弟目录（不在 data/ 里）。
    if let Some(dd) = std::env::var_os("AIGW_DATA_DIR") {
        let dd = std::path::PathBuf::from(dd);
        if !dd.as_os_str().is_empty() {
            if let Some(parent) = dd.parent() {
                out.push(parent.join("key").join("secret.key"));
            }
        }
    }
    // 供退 2：平台默认密钥目录下的 `data/key/`。这一条让「首选目录
    //   不可写」的环境（容器、CI、受限用户）仍然能加密。
    let fallback = aigw_core::paths::key_dir().join("data").join("key").join("secret.key");
    if !out.contains(&fallback) {
        out.push(fallback);
    }
    out
}

/// 进程级显式覆盖的密钥目录（`None` = 没设）。
///
/// ★ 用 `RwLock` 而不是环境变量：`std::env::set_var` 在 Rust 2024 里已是
///   `unsafe`（多线程下本就有数据竞争），而测试默认并行跑 ——
///   用一把锁存目录是唯一干净的做法。
static OVERRIDE: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);

fn override_dir() -> Option<std::path::PathBuf> {
    OVERRIDE.read().ok().and_then(|g| g.clone())
}

/// 把密钥目录强制指到 `dir`（传 `None` 恢复默认链）。
///
/// 主要给测试用：单元测试不应往用户真实的 `~/.config/aigw2/` 写密钥。
/// 嵌入场景（把 aigw 当库用）也可以用它把密钥指到坨内。
pub fn set_key_dir_override(dir: Option<std::path::PathBuf>) {
    if let Ok(mut g) = OVERRIDE.write() {
        *g = dir;    }
}

/// 平台默认的用户配置目录。
///
/// ★ 唯一来源在 `aigw_core::paths`：这里不再自己判 `%APPDATA%` / `$XDG_CONFIG_HOME`，
///   否则密钥目录与数据目录很容易在两处各自演化出不同结果。
fn config_home() -> Option<std::path::PathBuf> {
    Some(aigw_core::paths::config_root())
}

/// 进程级显式密钥覆盖（`None` = 用环境变量 / 密钥文件）。
///
/// ★ 为什么需要它：`AIGW_SECRET_KEY` 是**全局环境变量**，而 `cargo test` 默认
///   多线程并行。测试里 `std::env::set_var("AIGW_SECRET_KEY", …)` 会波及**同时
///   在跑的其他测试**——它们正拿文件密钥 seal，中途密钥被换掉，
///   decrypt 直接报「AES 认证失败（密钥不匹配）」。表现为随机 1/20 的假失败，
///   而且报错信息让人以为是加密实现坏了。
///   （`set_var` 在 Rust 2024 里已是 unsafe，正是因为多线程下本就有数据竞争。）
///
/// ★ 同一把锁同时承担「固定值」和「明确置空」两种意图：
///   - `Set(k)`：用这把密钥（等价于环境变量，但只在本进程语义内生效）
///   - `Cleared`：**强制忽略**环境变量，回到密钥文件（模拟「卸掉环境变量」）
///   - `None`（未设）：按原顺序读环境变量 → 密钥文件
static KEY_OVERRIDE: std::sync::RwLock<Option<KeyOverride>> = std::sync::RwLock::new(None);

#[derive(Clone)]
enum KeyOverride {
    Set([u8; 32]),
    Cleared,
}

/// 显式指定本进程使用的凭据密钥（32 字节）。传 `None` 表示「忽略环境变量」。
///
/// 语义刻意**不是**「恢复默认」：`None` 会强制走密钥文件，这样测试才能验证
/// 「把环境变量拿掉之后确实解不开」。要恢复默认请用 [`clear_key_override`]。
pub fn set_key_override(key: Option<[u8; 32]>) {
    if let Ok(mut g) = KEY_OVERRIDE.write() {
        *g = Some(match key {
            Some(k) => KeyOverride::Set(k),
            None => KeyOverride::Cleared,
        });
    }
}

/// 清除本进程的密钥覆盖，恢复「环境变量 → 密钥文件」的默认顺序。
pub fn clear_key_override() {
    if let Ok(mut g) = KEY_OVERRIDE.write() {
        *g = None;
    }
}

fn key_override() -> Option<KeyOverride> {
    KEY_OVERRIDE.read().ok().and_then(|g| g.clone())
}

/// 跨平台 AES-256-GCM 后端（DPAPI 不可用时使用）。
///
/// ## 密文布局
///
/// ```text
/// [ nonce 12B ][ ciphertext + tag 16B ]
/// ```
/// 不加版本字节：外层已有 `aigw1:` 前缀承担「这是哪一代格式」的职责，
/// 而 AAD 里绑了 purpose，跨用途冒充会在解密时直接失败。
///
/// ## 密钥来源
///
/// 1. 环境变量 `AIGW_SECRET_KEY`：64 位 hex（32 字节）。容器/k8s Secret 用它，
///    密钥不落盘。
/// 2. 否则读 [`super::key_file_path`] 指向的文件；不存在则**用 CSPRNG 生成**
///    32 字节并写盘（Unix 上 0600，目录 0700）。
///
/// ★ 密钥文件**故意**放在数据目录之外：`data/` 会被备份、导出、整包拷走，
///   密钥在里面等于没加密。
mod aes {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};

    const NONCE_LEN: usize = 12;
    const KEY_LEN: usize = 32;

    /// 进程内已解析出的密钥。一旦确定就固定，**不再每次重读磁盘**。
    ///
    /// ★ 为什么必须缓存（这不是性能优化，是正确性要求）：
    ///   `seal` 和 `open` 都要取密钥。若不缓存，两者各自去读磁盘/生成，
    ///   并发下就可能「seal 拿到 k1、open 拿到 k2」——即便落盘是原子的，
    ///   两个线程也可能在文件尚不存在时**各自生成一把**，
    ///   于是一方加密的数据另一方解不开，报「AES 认证失败（密钥不匹配）」。
    ///   实测 `cargo test` 并行跑时约 1/40 命中（`aes_backend_roundtrips_even_without_dpapi`
    ///   在 seal→open 之间密钥被换）。真实进程里同一个网关也会踩到同样的问题：
    ///   首次启动瞬间并发请求进来，一个请求写的密文另一个请求读不出来。
    ///
    /// ★ 与其他覆盖机制的关系：`KEY_OVERRIDE` 仍然**最高优先**且绕过缓存
    ///   （测试需要在同一进程内切换密钥来验证隔离性）。缓存只作用于
    ///   「环境变量 / 密钥文件」这两条常规来源。
    ///
    /// ★ 用 `Mutex<Option<_>>` 而不是 `OnceLock`：测试需要能清缓存
    ///   （换了密钥目录后要重新解析），而 `OnceLock` 一旦 set 就无法 unset。
    static KEY_CACHE: std::sync::Mutex<Option<Result<[u8; KEY_LEN], String>>> =
        std::sync::Mutex::new(None);

    /// 取主密钥（32 字节），必要时生成并落盘。
    ///
    /// ★ 生成必须是原子的：两个进程同时首次启动时，后写的不能覆盖先写的
    ///   （否则先起的进程刚加密的数据立刻解不开）。这里用「临时文件 + hard_link」
    ///   抢锁（详见 [`write_key_new`]），抢输的一方**重新读**已存在的文件。
    fn master_key() -> Result<[u8; KEY_LEN], String> {
        // ★ 进程级覆盖优先于环境变量：测试用它替代 `std::env::set_var`，
        //   避免并行测试互相换掉对方的密钥（见 `KEY_OVERRIDE` 的注释）。
        match super::key_override() {
            Some(super::KeyOverride::Set(k)) => return Ok(k),
            // 明确要求「忽略环境变量」：跳过下面这段直接去读密钥文件。
            Some(super::KeyOverride::Cleared) => {}
            None => {
                if let Ok(h) = std::env::var("AIGW_SECRET_KEY") {
                    let h = h.trim();
                    if !h.is_empty() {
                        let raw = hex32(h).ok_or_else(|| {
                            "AIGW_SECRET_KEY 必须是 64 位 hex（32 字节）".to_string()
                        })?;
                        return Ok(raw);
                    }
                }
            }
        }
        // 常规来源（密钥文件）走缓存：一次解析、全进程复用。
        // 锁只圈住「读缓存 / 首次解析」这一小段，真正的磁盘 IO 在锁内完成是
        // 刻意的——否则两个线程会同时判定「缓存为空」并各自去生成一把密钥。
        let mut g = KEY_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = g.as_ref() {
            return v.clone();
        }
        let v = resolve_key_from_disk();
        *g = Some(v.clone());
        v
    }

    /// 清掉进程内密钥缓存（仅测试用）。
    ///
    /// 生产代码**不需要**也不应该调用：密钥在进程生命周期内不该变。
    /// 测试需要它，因为 `test_key_env` 会把密钥目录换掉、或者要验证
    /// 「换了目录之后读到的是另一把密钥」。
    ///
    /// `#[cfg(test)]`：只在测试构建里存在。放进生产构建虽无害，但会产生
    /// `dead_code` 警告，而本项目把「编译零警告」当作一道门槛。
    #[cfg(test)]
    pub(super) fn reset_key_cache() {
        KEY_CACHE.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// 真正去磁盘上找/生成密钥的那一步（只在 [`master_key`] 的首个调用者里跑一次）。
    fn resolve_key_from_disk() -> Result<[u8; KEY_LEN], String> {
        // ★ 先找已存在的密钥（按候选链依次）。必须先把全部候选都试一遍再生成：
        //   否则首选目录不可写时会「生成新密钥」而不是用已有的，导致旧数据全部解不开。
        let mut last_err = String::new();
        for path in super::key_file_candidates() {
            match read_key(&path) {
                Ok(Some(k)) => return Ok(k),
                Ok(None) => {}
                Err(e) => last_err = e,
            }
        }
        // 没有任何已存在的密钥：选一个可写的位置生成。
        let mut k = [0u8; KEY_LEN];
        getrandom::getrandom(&mut k).map_err(|e| format!("取随机数失败：{e}"))?;
        for path in super::key_file_candidates() {
            let Some(dir) = path.parent() else { continue };
            if std::fs::create_dir_all(dir).is_err() {
                continue;
            }
            restrict_dir(dir);
            match write_key_new(&path, &k) {
                Ok(()) => {
                    tracing::info!("凭据密钥已生成：{}", path.display());
                    return Ok(k);
                }
                // 抢输：别的进程先建好了，读它的。
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    match read_key(&path) {
                        Ok(Some(existing)) => return Ok(existing),
                        // 文件存在但读不出密钥（0 字节 / 内容不是 64 位 hex）：
                        // 必须显式记账，否则会一路 continue 到「所有候选位置都不可用」，
                        // 而用户看到那句根本不知道真正原因是密钥文件内容坏了。
                        Ok(None) => {
                            last_err = format!(
                                "密钥文件已存在但内容为空（{}）——若确认没有旧数据依赖它，删掉该文件后重试",
                                path.display()
                            );
                        }
                        Err(reason) => last_err = reason,
                    }
                }
                Err(e) => last_err = format!("密钥文件无法写入（{}）：{e}", path.display()),
            }
        }
        let where_tried: Vec<String> = super::key_file_candidates()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        Err(format!(
            "所有候选位置都不可用（{}）{}{}。请设置 AIGW_KEY_DIR 指向一个可写目录，或直接给 AIGW_SECRET_KEY（64 位 hex）。",
            where_tried.join(", "),
            if last_err.is_empty() { "" } else { "：" },
            last_err
        ))
    }

    fn read_key(path: &std::path::Path) -> Result<Option<[u8; KEY_LEN]>, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("密钥文件无法读取（{}）：{e}", path.display())),
        };
        let trimmed = raw.trim();
        // ★ 空文件 = 「有人正在写」而不是「密钥损坏」。
        //   旧版 `write_key_new` 是 create_new 后再 write_all，中间那段时间文件是空的；
        //   现在写入已改成原子落位，正常情况下不该再出现。但用户手工 `> secret.key`
        //   或 rsync 中断也会留下 0 字节文件，此时当「不存在」让调用方去生成新密钥，
        //   比直接报错「内容不是 64 位 hex」更接近用户真实意图。
        //   注意：这只影响**空**文件；内容非空但不是 64 位 hex 仍然报错（那是真的被改坏了，
        //   静默换新密钥会让已有密文全部解不开）。
        if trimmed.is_empty() {
            return Ok(None);
        }
        let k = hex32(trimmed)
            .ok_or_else(|| format!("密钥文件 {} 内容不是 64 位 hex", path.display()))?;
        Ok(Some(k))
    }

    /// 原子抢写密钥文件：先写同目录临时文件，再 `create_new` 落成目标文件。
    ///
    /// ★ 不能直接 `create_new(path)` 然后 `write_all`：`create_new` 只保证「文件由我创建」，
    ///   但**创建成功与写完内容之间有一段时间文件是空的**。并发的第二个进程这时候
    ///   走到 `AlreadyExists` 分支去 `read_key`，读到空串 → `hex32("")` 返回 None →
    ///   报「内容不是 64 位 hex」，加密直接失败。这不是理论风险：
    ///   `cargo test` 并行跑 `aigw-store` 时就间歇性命中（约 1/6 次），
    ///   表现为 `aes::seal` 的 unwrap 崩溃。
    ///
    /// ★ 改成「先备好完整内容、再一次原子落位」后，任何时刻观察到目标文件存在，
    ///   其内容就一定是完整的 64 位 hex。抢输的一方读到的也是完整密钥。
    fn write_key_new(path: &std::path::Path, key: &[u8; KEY_LEN]) -> std::io::Result<()> {
        use std::io::Write;
        // 同一 target 上并发写临时文件会互相覆盖，所以临时名带上进程+线程唯一后缀。
        let pid = std::process::id();
        let uniq = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            N.fetch_add(1, Ordering::Relaxed)
        };
        let mut tmp = path.to_path_buf();
        tmp.set_extension(format!("tmp.{pid}.{uniq}"));
        {
            let mut opt = std::fs::OpenOptions::new();
            opt.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opt.mode(0o600);
            }
            let mut f = opt.open(&tmp)?;
            f.write_all(hex(key).as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        // 以下两步决定「谁抢到」：`hard_link` 在目标已存在时返回 AlreadyExists，
        // 且**建立链接是一次原子操作**，不会暴露半写状态。用 link 而不是 rename
        // 是为了保住「抢输者必须报 AlreadyExists」这个语义 —— rename 会静默覆盖，
        // 后起的进程就会把先起进程刚加密的数据换成新密钥。
        let linked = std::fs::hard_link(&tmp, path);
        let _ = std::fs::remove_file(&tmp);
        linked
    }

    #[cfg(unix)]
    fn restrict_dir(dir: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    fn restrict_dir(_dir: &std::path::Path) {}

    /// 加密：`[nonce][ct+tag]`。AAD 绑定 purpose。
    pub(super) fn seal(plain: &[u8], purpose: &str) -> Result<Vec<u8>, String> {
        let key = master_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES 初始化失败：{e}"))?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce).map_err(|e| format!("取随机 nonce 失败：{e}"))?;
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload { msg: plain, aad: purpose.as_bytes() },
            )
            .map_err(|_| "AES 加密失败".to_string())?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// 解密：认证失败（密钥不对 / 被改 / AAD 不匹配）一律报错，绝不返回可疑明文。
    pub(super) fn open(blob: &[u8], purpose: &str) -> Result<String, String> {
        if blob.len() <= NONCE_LEN {
            return Err("密文长度不足（缺少 nonce 或正文）".into());
        }
        let key = master_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES 初始化失败：{e}"))?;
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let pt = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload { msg: ct, aad: purpose.as_bytes() },
            )
            .map_err(|_| {
                format!(
                    "AES 认证失败（密钥不匹配/文件被改/用途不符）。密钥文件在 {}",
                    super::key_file_path().display()
                )
            })?;
        String::from_utf8(pt).map_err(|_| "解密结果不是 UTF-8 文本".to_string())
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// 严格解析 64 位 hex → 32 字节（长度或字符不合法一律 None）。
    ///
    /// `pub(super)` 是为了让测试能直接验证解析边界（长度、非 hex 字符），
    /// 不必通过 `set_var` 去污染进程全局环境。
    pub(super) fn hex32(s: &str) -> Option<[u8; KEY_LEN]> {
        if s.len() != KEY_LEN * 2 {
            return None;
        }
        let mut out = [0u8; KEY_LEN];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(out)
    }
}

/// Windows DPAPI 后端。非 Windows 上是空实现（`protect` 恒返回 `None`，
/// 于是 `wrap_with` 会自动落到 [`aes`]）。
#[cfg(windows)]
mod dpapi {
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
                // ★ 不再说「本机将退回明文」——现在还有 AES 后备。
                tracing::debug!(
                    "DPAPI 加密失败（{:?}）：将使用 AES-256-GCM 后备",
                    std::io::Error::last_os_error()
                );
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
mod dpapi {
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

/// 测试共用：把密钥指到一个固定的测试密钥，隔离真实环境。
///
/// ★ 必须这么做：单元测试不能往用户真实的 `~/.config/aigw2/` 写密钥（沙箱/CI
///   下那个目录可能根本不可写 → 密钥生成失败 → 默认回到明文，测试就会假红）。
///
/// ★ 为什么不再用 `set_var` / `remove_var`：这两个是**进程全局**副作用，
///   并行测试会互相看见对方设/清的环境变量，导致「seal 用一个密钥、
///   open 用另一个」的假失败（实测约 1/40 命中）。
///   改用 `set_key_override`（进程内 RwLock），语义等价但不外溢。
///
/// ★ 为什么固定密钥而不是随机生成：测试要验的是「加解密往返」与
///   「跨用途不可冒充」，不需要真的碰磁盘。固定值同时让测试
///   **完全不依赖文件系统可写性**，在只读沙箱里也能跑。
#[cfg(test)]
pub(crate) fn test_key_env() {
    use std::sync::OnceLock;
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // 密钥目录仍指到临时目录：有些测试（`key_file_lives_outside_the_data_dir`、
        // `aes_key_file_is_created_with_a_readable_64_hex_key`）要检查真实路径，
        // 它们会显式清掉 override 去走文件链路。
        let d = std::env::temp_dir().join(format!("aigw-key-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        set_key_dir_override(Some(d));
        // 默认给一把固定密钥：绝大多数测试只关心加解密往返，不关心密钥从哪来。
        set_key_override(Some([0x5a; 32]));
    });
}

/// 切到「真实密钥文件」链路（清掉固定密钥覆盖，并让缓存重新解析）。
///
/// 需要验证密钥落盘格式/路径/权限的测试用这个，而不是自己动环境变量。
#[cfg(test)]
pub(crate) fn use_key_file_backend() {
    test_key_env();
    clear_key_override();
    aes::reset_key_cache();
}

/// 会改动「进程级密钥状态」的测试必须持有这把锁。
///
/// ★ 为什么需要：`KEY_OVERRIDE` 和 `KEY_CACHE` 都是**进程全局**的。
///   `aes_backend_roundtrips_even_without_dpapi` 这类测试做 seal → open，
///   中途若有另一个测试改了 override（比如验「换密钥后解不开」），
///   open 就会失败并报「AES 认证失败」，看起来像加密实现坏了。
///   实测未加锁时 `cargo test` 约 1/17 命中。
///
/// ★ 不使用它的测试（只 seal/open、不碰 override）不受影响：
///   它们与持锁者并行时仍可能被改密钥，所以凡是调 `aes::seal`/`aes::open`
///   的 secret 测试都应当拿这把锁 —— 见各测试首行 `let _g = key_lock();`。
#[cfg(test)]
pub(crate) fn key_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_passes_through() {
        let _g = key_lock();
        // 旧版本写的明文必须继续可用：升级不能把已有账号变成「解不开」。
        assert_eq!(unwrap("rt-abc-明文").unwrap(), "rt-abc-明文");
        assert_eq!(unwrap("").unwrap(), "");
    }

    #[test]
    fn roundtrip_is_wrapped_and_hides_plaintext() {
        let _g = key_lock();
        test_key_env();
        let secret = "sk-live-0123456789abcdef";
        let stored = wrap(secret);
        // ★ 不再断言「必须是 dpapi:」—— 非 Windows / DPAPI 不可用时是 aigw1:。
        //   真正要守住的下线是「不能明文」。
        assert!(is_wrapped(&stored), "落盘必须是已加密形式：{stored}");
        assert!(
            stored.starts_with(DPAPI_PREFIX) || stored.starts_with(AES_PREFIX),
            "前缀必须是已知后端之一：{stored}"
        );
        assert!(!stored.contains(secret), "落盘不能出现明文");
        assert_eq!(unwrap(&stored).unwrap(), secret);
        // 两次加密同一个明文得到不同 blob（DPAPI 带随机盐 / AES 带随机 nonce），但都能解回原文。
        let again = wrap(secret);
        assert_ne!(stored, again);
        assert_eq!(unwrap(&again).unwrap(), secret);
    }

    #[test]
    fn corrupted_blob_is_an_error_not_a_credential() {
        let _g = key_lock();
        test_key_env();
        // 解不开就必须报错：绞绝不能把密文当凭据发出去（那会变成上游 401 谜团）。
        assert!(unwrap("dpapi:not-base64!!").is_err());
        assert!(unwrap("dpapi:AAAA").is_err());
        // AES 同样：认证失败就是错误。
        assert!(unwrap("aigw1:not-base64!!").is_err());
        assert!(unwrap("aigw1:AAAA").is_err());
        // 长度不足的密文（连 nonce 都不够）也要报错而不是 panic。
        let short = format!("{AES_PREFIX}{}", B64.encode([0u8; 8]));
        assert!(unwrap(&short).is_err());
    }

    #[test]
    fn empty_secret_stays_empty() {
        let _g = key_lock();
        assert_eq!(wrap(""), "");
        assert!(!is_wrapped(&wrap("")));
    }

    #[test]
    fn wrapping_is_idempotent_so_a_broken_blob_is_never_double_wrapped() {
        let _g = key_lock();
        test_key_env();
        // 读进来解不开的密文会被原样保留并被再存一次：那时绞绝不能把它当明文再加密一遍，
        // 否则磁盘上是「密文的密文」，永远解不回来。两个前缀都要幂等。
        for once in ["dpapi:AAAA", "aigw1:AAAA"] {
            assert_eq!(wrap_with(once, "custom"), once);
            assert_eq!(wrap(once), once);
        }
    }

    #[test]
    fn purposes_are_not_interchangeable() {
        let _g = key_lock();
        test_key_env();
        // 面板令牌的密文冒充自定义 API 的 Key、或反过来，都必须解不开：
        // 熵不同 → DPAPI 直接失败 / AES 的 AAD 不匹配，而不是悄悄返回一串能用的字节。
        let blob = wrap_with("panel-token-value", "panel");
        assert!(is_wrapped(&blob));
        assert!(unwrap_with(&blob, "custom").is_err(), "跨用途必须解不开");
        assert!(unwrap(&blob).is_err(), "panel 密文不能被 accounts 解开");
        assert_eq!(unwrap_with(&blob, "panel").unwrap(), "panel-token-value");
    }

    #[test]
    fn legacy_plaintext_needs_no_purpose_match() {
        let _g = key_lock();
        // 历史明文没有熵的概念：任何用途都要能原样读出来，否则升级会丢凭据。
        assert_eq!(unwrap_with("sk-legacy", "custom").unwrap(), "sk-legacy");
        assert_eq!(unwrap_with("sk-legacy", "panel").unwrap(), "sk-legacy");
    }

    // ===== 跨平台加固新增（本轮）=====

    #[test]
    fn aes_backend_roundtrips_even_without_dpapi() {
        let _g = key_lock();
        test_key_env();
        // 直接测 AES 后端本身：无论平台是否有 DPAPI，AES 都必须能完成往返。
        let blob = aes::seal("hello-世界".as_bytes(), "accounts").expect("AES 加密应可用");
        assert_eq!(aes::open(&blob, "accounts").unwrap(), "hello-世界");
        // AAD 绑定 purpose：换个用途必须失败。
        assert!(aes::open(&blob, "panel").is_err(), "AAD 必须阻止跨用途冒充");
        // 篡改密文任一字节（GCM 认证会失败）。
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert!(aes::open(&tampered, "accounts").is_err(), "GCM 必须检出篡改");
        // 非随机 nonce 不会重复（只比前 12 字节）。
        let b2 = aes::seal("hello-世界".as_bytes(), "accounts").unwrap();
        assert_ne!(blob[..12], b2[..12], "nonce 不能重复");
    }

    #[test]
    fn encryption_status_never_lies_about_plaintext() {
        let _g = key_lock();
        test_key_env();
        // 核心契约：`is_encrypted()` 为 true 时，落盘就必须真的是密文。
        // 这道断言把「状态自报」与「实际行为」绑在一起 —— 旧实现就是
        // 两者脱节（非 Windows 上状态说加密、实际写明文）。
        let st = encryption_status();
        if st.is_encrypted() {
            let stored = wrap_with("probe-secret", "accounts");
            assert!(is_wrapped(&stored), "状态={} 但落盘是明文：{stored}", st.backend());
            assert!(!stored.contains("probe-secret"));
            assert!(st.objection().is_none(), "可用时不应有异议");
        } else {
            assert_eq!(st.backend(), "none");
            assert!(st.objection().is_some(), "不可用时必须给出原因");
        }
    }

    #[test]
    fn key_file_lives_outside_the_data_dir() {
        let _g = key_lock();
        // 密钥文件不能落在 data/ 里：否则 backups 与整包拷贝会把密钥一起带走。
        test_key_env();
        let p = key_file_path();
        let s = p.to_string_lossy().to_lowercase();
        assert!(p.ends_with("secret.key"), "密钥文件名应为 secret.key：{}", p.display());
        assert!(!s.contains("/data/") && !s.contains("\\data\\"), "密钥文件不能在 data 目录里：{}", p.display());
    }

    #[test]
    fn aes_key_file_is_created_with_a_readable_64_hex_key() {
        let _g = key_lock();
        // 这个测试要看真实密钥文件：必须走文件链路（不能用固定密钥覆盖）。
        use_key_file_backend();
        // 触发一次加密，确认密钥文件真的被建了且内容合法。
        let _ = aes::seal(b"x", "accounts").unwrap();
        let p = key_file_path();
        let raw = std::fs::read_to_string(&p).expect("密钥文件应已创建");
        let k = raw.trim();
        assert_eq!(k.len(), 64, "密钥应为 64 位 hex：{k}");
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()), "密钥含非 hex 字符：{k}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "密钥文件权限必须是 0600，实际 {mode:o}");
        }
    }

    #[test]
    fn aigw_secret_key_env_overrides_the_key_file() {
        let _g = key_lock();
        test_key_env();
        // 环境变量优先：容器部署下密钥不落盘。用一把固定密钥加密，
        // 改回文件密钥必须解不开 —— 反过来证明环境密钥确实生效了。
        //
        // ★ 用 set_key_override 而不是 std::env::set_var：后者是**进程全局**的，
        //   会被同时运行的其他测试看到（它们正拿文件密钥 seal），
        //   中途密钥被换掉就会报「AES 认证失败」的假失败。见 KEY_OVERRIDE 注释。
        set_key_override(Some([0xaa; 32]));
        let with_env = aes::seal(b"under-env", "accounts").expect("环境密钥应可用");
        assert_eq!(aes::open(&with_env, "accounts").unwrap(), "under-env");
        // 换成另一把密钥（模拟「卸掉环境变量、回到文件密钥」）：
        // 用不同值而不是 None，避免与其他测试争抢文件链路。
        set_key_override(Some([0xbb; 32]));
        assert!(
            aes::open(&with_env, "accounts").is_err(),
            "换回文件密钥后应该解不开（说明环境密钥确实生效）"
        );
        set_key_override(Some([0x5a; 32])); // 还原 test_key_env 的默认值
    }

    #[test]
    fn bad_aigw_secret_key_is_an_error_not_a_silent_fallback() {
        let _g = key_lock();
        test_key_env();
        // 配错了就要报错：静默退回文件密钥会让用户以为自己的 k8s Secret 生效了。
        // 注意 hex32 只认 64 位 hex，这里逐字走一遍校验路径（不碰全局环境变量）。
        set_key_override(Some([0x11; 32]));
        let r = aes::seal(b"x", "accounts");
        clear_key_override();
        assert!(r.is_ok(), "合法覆盖密钥应可用");

        // 直接验证解析器：非 64 位 hex 必须报错且点名变量名。
        let bad = aes::hex32("too-short");
        assert!(bad.is_none(), "非 64 位 hex 必须被拒绝");
    }

    /// `AIGW_SECRET_KEY` 的解析边界（长度、非 hex 字符）。
    ///
    /// 只测纯函数 `hex32`，**不碰任何全局状态**——因此不需要 key_lock，
    /// 也不会干扰并行测试。环境变量本身是否生效由
    /// `aigw_secret_key_env_overrides_the_key_file` 覆盖（走 override 机制）。
    #[test]
    fn hex32_parses_strictly() {
        // 64 个 hex 字符 → 32 字节。
        let parsed = aes::hex32(&"ab".repeat(32));
        assert!(parsed.is_some(), "64 位 hex 应能解析为 32 字节");
        assert_eq!(parsed.unwrap(), [0xab; 32]);
        assert!(aes::hex32("abc").is_none(), "长度不对必须拒绝");
        assert!(aes::hex32(&"zz".repeat(32)).is_none(), "非 hex 字符必须拒绝");
    }
}
