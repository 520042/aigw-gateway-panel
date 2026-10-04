# -*- coding: utf-8 -*-
"""
Windows BCrypt(CNG) 原生 AES-GCM 参考实现
==========================================
用途：作为「黄金参考」交叉验证 app/aesgcm.py 的纯 Python 实现。

为什么需要它：
  自研实现不能只用自己加密再自己解密来验证——那样即便起始计数器写错
  也照样自洽。必须有一个独立、权威的实现做基准。BCrypt 是 Windows 自带的
  FIPS 认证实现，离线可用，正是最合适的基准。

仅用于测试，不参与打包（build.spec 未包含）。
"""

import ctypes
from ctypes import wintypes

bcrypt = ctypes.WinDLL("bcrypt")

_BCRYPT_CHAINING_MODE = "ChainingMode"
_BCRYPT_CHAIN_MODE_GCM = "ChainingModeGCM"


class _AUTH_INFO(ctypes.Structure):
    """BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO（64 位下 sizeof == 88）"""
    _fields_ = [
        ("cbSize", wintypes.ULONG),
        ("dwInfoVersion", wintypes.ULONG),
        ("pbNonce", ctypes.c_void_p),
        ("cbNonce", wintypes.ULONG),
        ("pbAuthData", ctypes.c_void_p),
        ("cbAuthData", wintypes.ULONG),
        ("pbTag", ctypes.c_void_p),
        ("cbTag", wintypes.ULONG),
        ("pbMacContext", ctypes.c_void_p),
        ("cbMacContext", wintypes.ULONG),
        ("cbAAD", wintypes.ULONG),
        ("cbData", ctypes.c_ulonglong),
        ("dwFlags", wintypes.ULONG),
    ]


def _w(s):
    return (s + "\0").encode("utf-16-le")


def gcm_encrypt(key, iv, aad, plain, taglen=16):
    """返回 (ciphertext, tag)"""
    h_alg = ctypes.c_void_p()
    st = bcrypt.BCryptOpenAlgorithmProvider(ctypes.byref(h_alg), _w("AES"), None, 0)
    if st:
        raise OSError("BCryptOpenAlgorithmProvider 失败 0x%X" % st)
    mode = _w(_BCRYPT_CHAIN_MODE_GCM)
    st = bcrypt.BCryptSetProperty(h_alg, _w(_BCRYPT_CHAINING_MODE),
                                  mode, len(mode), 0)
    if st:
        raise OSError("BCryptSetProperty 失败 0x%X" % st)

    h_key = ctypes.c_void_p()
    kbuf = ctypes.create_string_buffer(bytes(key), len(key))
    st = bcrypt.BCryptGenerateSymmetricKey(h_alg, ctypes.byref(h_key), None, 0,
                                           kbuf, len(key), 0)
    if st:
        raise OSError("BCryptGenerateSymmetricKey 失败 0x%X" % st)

    ivbuf = ctypes.create_string_buffer(bytes(iv), len(iv))
    aadbuf = ctypes.create_string_buffer(bytes(aad), len(aad)) if aad else None
    tagbuf = ctypes.create_string_buffer(taglen)

    info = _AUTH_INFO()
    info.cbSize = ctypes.sizeof(_AUTH_INFO)
    info.dwInfoVersion = 1
    info.pbNonce = ctypes.cast(ivbuf, ctypes.c_void_p)
    info.cbNonce = len(iv)
    if aadbuf:
        info.pbAuthData = ctypes.cast(aadbuf, ctypes.c_void_p)
        info.cbAuthData = len(aad)
    info.pbTag = ctypes.cast(tagbuf, ctypes.c_void_p)
    info.cbTag = taglen

    out = ctypes.create_string_buffer(max(len(plain), 1))
    nres = wintypes.ULONG()
    pbuf = ctypes.create_string_buffer(bytes(plain), len(plain)) if plain else None
    st = bcrypt.BCryptEncrypt(h_key, pbuf, len(plain), ctypes.byref(info),
                              ivbuf, len(iv), out, len(plain),
                              ctypes.byref(nres), 0)
    bcrypt.BCryptDestroyKey(h_key)
    bcrypt.BCryptCloseAlgorithmProvider(h_alg, 0)
    if st:
        raise OSError("BCryptEncrypt 失败 0x%X" % st)
    return out.raw[:nres.value], tagbuf.raw[:taglen]


if __name__ == "__main__":
    import os
    import sys
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from app.aesgcm import aes_gcm_decrypt

    key = bytes(range(32))
    iv = bytes(range(12))
    cases = [(b"", b""), (b"A", b""), (bytes(range(64)), b""),
             (bytes(range(64)), bytes(range(20))), (b"x" * 100, b"aad")]
    bad = 0
    for pt, aad in cases:
        ct, tag = gcm_encrypt(key, iv, aad, pt)
        try:
            got = aes_gcm_decrypt(key, iv, ct, tag, aad)
            okk = got == pt
        except ValueError as e:
            got, okk = str(e), False
        if not okk:
            bad += 1
        print("pt=%-4d aad=%-3d -> %s %s"
              % (len(pt), len(aad), "OK" if okk else "FAIL",
                 "" if okk else repr(got)[:50]))
    print("失败 %d / %d" % (bad, len(cases)))
