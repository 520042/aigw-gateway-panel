# -*- coding: utf-8 -*-
"""
纯标准库 AES-GCM（仅实现解密所需方向）
=====================================
用途：读取 Chrome / Edge 在本机磁盘上加密保存的 Cookie（v10 / v11）。

为什么自己写：
  1. 面板坚持「纯标准库」——引入 cryptography 会让单文件 EXE 体积翻倍，
     且 PyInstaller 需要额外 hook。
  2. GCM 解密只需要 AES 的**加密**方向（CTR 模式生成 keystream），
     不需要实现 InvSubBytes / InvMixColumns，代码量减半。

实现范围：
  AES-128 / AES-192 / AES-256 单块加密
  GHASH（GF(2^128) 乘法，多项式 x^128+x^7+x^2+x+1）
  GCM 解密 + Tag 校验（支持任意长度 IV，实际只用到 96bit）

自检：文件末尾 AES_GCM_SELFTEST 用 NIST SP 800-38D 测试向量验证。
"""

# ------------------------------------------------------------------ 常量

def _build_tables():
    """程序化生成 AES S-box，避免手写 256 个魔数出错"""
    sbox = [0] * 256
    p = 1
    q = 1
    while True:
        # p *= 3（GF(2^8) 下）
        p = p ^ ((p << 1) & 0xFF) ^ (0x1B if p & 0x80 else 0)
        # q /= 3
        q ^= (q << 1) & 0xFF
        q ^= (q << 2) & 0xFF
        q ^= (q << 4) & 0xFF
        if q & 0x80:
            q ^= 0x09
        x = q ^ ((q << 1) | (q >> 7)) ^ ((q << 2) | (q >> 6)) \
              ^ ((q << 3) | (q >> 5)) ^ ((q << 4) | (q >> 4))
        sbox[p] = (x ^ 0x63) & 0xFF
        if p == 1:
            break
    sbox[0] = 0x63
    return sbox


_SBOX = _build_tables()
_RCON = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1B, 0x36, 0x6C, 0xD8, 0xAB, 0x4D]

# 断言：S-box 生成正确（标准 AES S-box 的三个采样点）
assert _SBOX[0x00] == 0x63, "S-box 生成错误"
assert _SBOX[0x01] == 0x7C, "S-box 生成错误"
assert _SBOX[0x53] == 0xED, "S-box 生成错误"

# GF(2^128) 的约化多项式常量在下方 GCM 段定义（_PLOW / _MASK128）


# ------------------------------------------------------------------ AES

def _xtime(a):
    a <<= 1
    if a & 0x100:
        a = (a ^ 0x1B) & 0xFF
    return a


def _mul(a, b):
    """GF(2^8) 乘法，用于 MixColumns"""
    res = 0
    a &= 0xFF
    b &= 0xFF
    for _ in range(8):
        if b & 1:
            res ^= a
        b >>= 1
        a = _xtime(a)
    return res & 0xFF


def _key_expansion(key, nk, nr):
    """密钥扩展，返回 4*(nr+1) 个 32bit 字"""
    w = [0] * (4 * (nr + 1))
    for i in range(nk):
        w[i] = int.from_bytes(key[4 * i:4 * i + 4], "big")
    for i in range(nk, 4 * (nr + 1)):
        t = w[i - 1]
        if i % nk == 0:
            t = ((t << 8) | (t >> 24)) & 0xFFFFFFFF
            t = ((_SBOX[(t >> 24) & 0xFF] << 24) | (_SBOX[(t >> 16) & 0xFF] << 16)
                 | (_SBOX[(t >> 8) & 0xFF] << 8) | _SBOX[t & 0xFF])
            t ^= _RCON[i // nk - 1] << 24
        elif nk > 6 and i % nk == 4:
            t = ((_SBOX[(t >> 24) & 0xFF] << 24) | (_SBOX[(t >> 16) & 0xFF] << 16)
                 | (_SBOX[(t >> 8) & 0xFF] << 8) | _SBOX[t & 0xFF])
        w[i] = w[i - nk] ^ t
    return w


def aes_encrypt_block(key, block):
    """AES 单块加密（128bit）。block 为 16 字节"""
    nk = len(key) // 4
    nr = nk + 6
    w = _key_expansion(key, nk, nr)

    # state 按列填充：s[r][c]
    s = [list(block[0::4]), list(block[1::4]),
         list(block[2::4]), list(block[3::4])]

    def add_round_key(rnd):
        for c in range(4):
            word = w[rnd * 4 + c]
            s[0][c] ^= (word >> 24) & 0xFF
            s[1][c] ^= (word >> 16) & 0xFF
            s[2][c] ^= (word >> 8) & 0xFF
            s[3][c] ^= word & 0xFF

    add_round_key(0)
    for rnd in range(1, nr):
        # SubBytes
        for r in range(4):
            for c in range(4):
                s[r][c] = _SBOX[s[r][c]]
        # ShiftRows（第 r 行左移 r）
        for r in range(1, 4):
            s[r] = s[r][r:] + s[r][:r]
        # MixColumns
        for c in range(4):
            a0, a1, a2, a3 = s[0][c], s[1][c], s[2][c], s[3][c]
            s[0][c] = _mul(a0, 2) ^ _mul(a1, 3) ^ a2 ^ a3
            s[1][c] = a0 ^ _mul(a1, 2) ^ _mul(a2, 3) ^ a3
            s[2][c] = a0 ^ a1 ^ _mul(a2, 2) ^ _mul(a3, 3)
            s[3][c] = _mul(a0, 3) ^ a1 ^ a2 ^ _mul(a3, 2)
        add_round_key(rnd)

    for r in range(4):
        for c in range(4):
            s[r][c] = _SBOX[s[r][c]]
    for r in range(1, 4):
        s[r] = s[r][r:] + s[r][:r]
    add_round_key(nr)

    out = bytearray(16)
    for c in range(4):
        for r in range(4):
            out[4 * c + r] = s[r][c]
    return bytes(out)


# ------------------------------------------------------------------ GCM

# GF(2^128) 约化多项式 P(x)=x^128+x^7+x^2+x+1 的低次部分。
# 位序约定：int 的 bit j ↔ x^j（bit127 ↔ x^127），与 int.from_bytes(blk,"big") 一致。
# a 的最高次项 x^127 左移后溢出为 x^128，需 XOR (x^7+x^2+x+1) = bit7|bit2|bit1|bit0 = 0x87。
_PLOW = 0x87
_MASK128 = (1 << 128) - 1
# 高 96 位掩码（inc32 保留用）
_MASK_HIGH96 = 0xFFFFFFFFFFFFFFFFFFFFFFFF00000000
# 右移版 GHASH 用的约化常量（R = 11100001 || 0^120）
_R128 = 0xE1000000000000000000000000000000


def _gmul(a, b):
    """
    GF(2^128) 乘法，多项式 x^128+x^7+x^2+x+1。

    ⚠️ 这里是 **右移** 版本，配合 int.from_bytes(blk, "big") 使用 —— 这个组合
    已用 Windows BCrypt(CNG) 原生 AES-GCM 做过逐字节交叉验证（见 bcrypt_ref.py）。

    不要用「左移 + R=0x87」去"修正"它：那等价于把块做了比特反转，
    算出来的 GHASH 是错的。GCM 规范里块的 bit0 是最高次项，而本实现的
    int 表示中 bit0 承载的是判约用的最低位，两者通过右移天然对齐。
    """
    res = 0
    for i in range(128):
        if (b >> (127 - i)) & 1:
            res ^= a
        if a & 1:
            a = (a >> 1) ^ _R128
        else:
            a >>= 1
    return res


def _ghash(h, data):
    y = 0
    n = len(data)
    for i in range(0, n, 16):
        blk = data[i:i + 16]
        if len(blk) < 16:
            blk = blk + b"\x00" * (16 - len(blk))
        y = _gmul(y ^ int.from_bytes(blk, "big"), h)
    return y


def _inc32(counter):
    """
    GCM 的 inc32：只递增低 32 位，高 **96** 位原样保留。
    掩码必须是 96 位宽 —— 写成 0xFFFFFFFF00000000 会把 128 位计数器
    的高 64 位清零，导致从第二个数据块起 keystream 全部错位。
    """
    return (counter & _MASK_HIGH96) | (((counter & 0xFFFFFFFF) + 1) & 0xFFFFFFFF)


def _j0(h, iv):
    """由 IV 派生 J0（96bit 直接拼接，其它长度走 GHASH）"""
    if len(iv) == 12:
        return (int.from_bytes(iv, "big") << 32) | 1
    # 非 96bit IV：s = 128·ceil(len(IV)/128) − len(IV)
    #              J0 = GHASH_H(IV || 0^(s+64) || [len(IV)]_64)
    blocks = (len(iv) + 127) // 128
    s = 128 * blocks - len(iv)
    data = iv + b"\x00" * ((s + 64) // 8) + (len(iv) * 8).to_bytes(8, "big")
    return _ghash(h, data)


def aes_gcm_encrypt(key, iv, plaintext, aad=b"", taglen=16):
    """AES-GCM 加密，返回 (ciphertext, tag)。与 aes_gcm_decrypt 对称"""
    h = int.from_bytes(aes_encrypt_block(key, b"\x00" * 16), "big")
    j0 = _j0(h, iv)

    ct = bytearray()
    ctr = _inc32(j0)
    for i in range(0, len(plaintext), 16):
        ks = aes_encrypt_block(key, ctr.to_bytes(16, "big"))
        chunk = plaintext[i:i + 16]
        ct.extend(bytes(a ^ b for a, b in zip(chunk, ks)))
        ctr = _inc32(ctr)
    ct = bytes(ct)

    buf = aad + b"\x00" * ((16 - len(aad) % 16) % 16)
    buf += ct + b"\x00" * ((16 - len(ct) % 16) % 16)
    buf += (len(aad) * 8).to_bytes(8, "big") + (len(ct) * 8).to_bytes(8, "big")
    t = (int.from_bytes(aes_encrypt_block(key, j0.to_bytes(16, "big")), "big")
         ^ _ghash(h, buf)).to_bytes(16, "big")[:taglen]
    return ct, t


def aes_gcm_decrypt(key, iv, ciphertext, tag, aad=b""):
    """
    AES-GCM 解密并校验 Tag。
    成功返回明文 bytes；Tag 不匹配抛出 ValueError。
    """
    h = int.from_bytes(aes_encrypt_block(key, b"\x00" * 16), "big")
    j0 = _j0(h, iv)

    # CTR 生成 keystream。
    # 注意：GCM 的 GCTR 用于加解密明文时，起始计数器是 **inc32(J0)** 而非 J0
    # （J0 只用于计算认证标签 T = MSB_t(AES_K(J0) XOR S)）。
    # 空明文时两者无差别，非空时会整段错位 —— 这是个只在实际数据上暴露的坑。
    plain = bytearray()
    ctr = _inc32(j0)
    for i in range(0, len(ciphertext), 16):
        ks = aes_encrypt_block(key, ctr.to_bytes(16, "big"))
        chunk = ciphertext[i:i + 16]
        plain.extend(bytes(a ^ b for a, b in zip(chunk, ks)))
        ctr = _inc32(ctr)

    # 校验 Tag：S = GHASH(A || pad || C || pad || len(A)||len(C))
    buf = aad + b"\x00" * ((16 - len(aad) % 16) % 16)
    buf += ciphertext + b"\x00" * ((16 - len(ciphertext) % 16) % 16)
    buf += (len(aad) * 8).to_bytes(8, "big") + (len(ciphertext) * 8).to_bytes(8, "big")
    s = _ghash(h, buf)
    t = (int.from_bytes(aes_encrypt_block(key, j0.to_bytes(16, "big")), "big") ^ s)
    t = t.to_bytes(16, "big")[:len(tag)]

    if t != tag:
        raise ValueError("AES-GCM Tag 校验失败")
    return bytes(plain)


# ------------------------------------------------------------------ 自检

def selftest():
    """NIST SP 800-38D 测试向量 + AES 已知向量"""
    checks = []

    # AES-128 FIPS-197 附录 B：明文 00112233445566778899aabbccddeeff
    k128 = bytes.fromhex("000102030405060708090a0b0c0d0e0f")
    p = bytes.fromhex("00112233445566778899aabbccddeeff")
    want = bytes.fromhex("69c4e0d86a7b0430d8cdb78070b4c55a")
    checks.append(("AES-128 加密向量", aes_encrypt_block(k128, p) == want))

    # AES-256 FIPS-197 附录 C.3
    k256 = bytes.fromhex("000102030405060708090a0b0c0d0e0f"
                         "101112131415161718191a1b1c1d1e1f")
    want256 = bytes.fromhex("8ea2b7ca516745bfeafc49904b496089")
    checks.append(("AES-256 加密向量", aes_encrypt_block(k256, p) == want256))

    # ---- GCM：NIST SP 800-38D 测试向量 ----

    # TC1：AES-128 全零密钥，IV=12 零字节，明文/AAD 均空
    got = aes_gcm_decrypt(bytes(16), bytes(12), b"", bytes.fromhex(
        "58e2fccefa7e3061367f1d57a4e7455a"), b"")
    checks.append(("GCM TC1 空明文（AES-128）", got == b""))

    # TC13：AES-256 全零密钥，明文/AAD 均空
    got = aes_gcm_decrypt(bytes(32), bytes(12), b"", bytes.fromhex(
        "530f8afbc74536b9a963b4f1c4cb738b"), b"")
    checks.append(("GCM TC13 空明文（AES-256）", got == b""))

    # 说明：非空明文的权威验证不放在这里 —— 用自己加密再自己解密是自洽的，
    # 无法发现"起始计数器写错"这类规范性错误。真正的基准在
    # bcrypt_ref.py（Windows BCrypt/CNG 原生实现交叉验证），由 test_login.py 调用。

    # ---- 往返自测：覆盖多块 / 非 16 倍数长度 / AAD ----
    rk, riv = bytes(range(32)), bytes(range(12))
    for L, AL in ((0, 0), (1, 0), (15, 0), (16, 0), (17, 1), (64, 0),
                  (64, 20), (100, 33), (1000, 7)):
        pt = bytes((i * 7 + 11) & 0xFF for i in range(L))
        aad = bytes((i * 3 + 5) & 0xFF for i in range(AL))
        ct, tg = aes_gcm_encrypt(rk, riv, pt, aad)
        got = aes_gcm_decrypt(rk, riv, ct, tg, aad)
        checks.append(("往返 %dB+AD%d" % (L, AL), got == pt))

    # ---- 篡改必须被拒绝 ----
    pt = bytes(range(48))
    aad = bytes(range(20))
    ct, tg = aes_gcm_encrypt(rk, riv, pt, aad)
    try:
        aes_gcm_decrypt(rk, riv, bytes([ct[0] ^ 0xFF]) + ct[1:], tg, aad)
        checks.append(("篡改密文被拒绝", False))
    except ValueError:
        checks.append(("篡改密文被拒绝", True))
    try:
        aes_gcm_decrypt(rk, riv, ct, tg, bytes([aad[0] ^ 0xFF]) + aad[1:])
        checks.append(("篡改 AAD 被拒绝", False))
    except ValueError:
        checks.append(("篡改 AAD 被拒绝", True))
    try:
        aes_gcm_decrypt(rk, riv, ct, bytes([tg[0] ^ 0xFF]) + tg[1:], aad)
        checks.append(("篡改 Tag 被拒绝", False))
    except ValueError:
        checks.append(("篡改 Tag 被拒绝", True))
    try:
        aes_gcm_decrypt(rk, bytes([riv[0] ^ 0xFF]) + riv[1:], ct, tg, aad)
        checks.append(("篡改 IV 被拒绝", False))
    except ValueError:
        checks.append(("篡改 IV 被拒绝", True))

    # ---- 多块时 keystream 必须推进（inc32 高 96 位保留的回归测试）----
    long_pt = bytes(range(96))          # 6 个块，跨过 inc32 边界
    ct, tg = aes_gcm_encrypt(rk, riv, long_pt, b"")
    checks.append(("6 块往返（inc32 高位保留）",
                   aes_gcm_decrypt(rk, riv, ct, tg, b"") == long_pt))
    # IV 高 96 位非零，才能暴露 inc32 掩码写窄的问题
    iv2 = bytes(range(1, 13))
    ct, tg = aes_gcm_encrypt(rk, iv2, long_pt, b"")
    checks.append(("非零高位 IV 多块往返",
                   aes_gcm_decrypt(rk, iv2, ct, tg, b"") == long_pt))

    # ---- 非 96bit IV 往返 ----
    for ivlen in (8, 16, 20):
        ivl = bytes(range(ivlen))
        ct, tg = aes_gcm_encrypt(rk, ivl, pt, aad)
        checks.append(("非 96bit IV(%dB) 往返" % ivlen,
                       aes_gcm_decrypt(rk, ivl, ct, tg, aad) == pt))

    return checks


AES_GCM_SELFTEST = selftest

if __name__ == "__main__":
    n = 0
    for name, okk in selftest():
        print(("OK  " if okk else "FAIL") + "  " + name)
        n += 0 if okk else 1
    print("\n%d/%d 通过" % (len(selftest()) - n, len(selftest())))
    raise SystemExit(1 if n else 0)
