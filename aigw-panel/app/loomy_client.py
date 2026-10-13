# -*- coding: utf-8 -*-
"""
Loomy（讯飞）账号服务原生客户端
================================
参照 Patrick130306/loomy2api 的逆向协议（docs/PROTOCOL.md + constants.py），
端点/签名/常量逐条对齐官方桌面客户端（Electron 0.9.38）。

为什么有这个模块：Loomy 的会话绑定设备指纹，面板服务器侧拿浏览器 Cookie
调 API 会被风控判为新设备（device rebind requires explicit user action）。
而正确姿势是走**账号服务的签名 API 登录**——设备标识（devid=web 等常量）
在登录 envelope 里由客户端声明并由服务端绑定，根本不需要浏览器。

组成：
  · 签名：HMAC-SHA1 九行 stringToSign（对齐官方客户端）
  · 短信登录：sendMsgCode → checkCode（msgid 回传）
  · 密码登录：getPuKey(RSA) → byPwd（rcode 是服务器 nonce，可完全脚本化）
  · 模型网关：Authorization Bearer <session> + token + traceparent + loomy-version

会话 14 天有效、无 refreshToken —— 过期重登（密码登录可全自动）。
"""

import base64
import hashlib
import hmac
import json
import os
import re
import ssl
import urllib.error
import urllib.parse
import urllib.request
import uuid
import email.utils

# ---------------------------------------------------------------- 常量
ACCOUNT_BASE = "https://account.xfinfr.com"
MODEL_BASE = "https://loomyad.xunfei.cn/api/v1"
AK_ID = "2thryby66wxi53sk"
AK_SECRET = "zsak6eadrbawz683wf5r3m2snrwj868r"
APPID = "GM3LOOMY"
MODELID = "Web"
CLIENT_VERSION = "1.0.0"
DEVICE_ID = "web"
CLIENT_UA = "Loomy|Desktop|Electron|macOS"
LOOMY_VERSION = "0.9.38"
SESSION_EXPIRE = 14 * 24 * 3600

_SSL = ssl.create_default_context()
_SSL.check_hostname = False
_SSL.verify_mode = ssl.CERT_NONE


def norm_phone(phone):
    """归一化手机号 → (ok, 号码或错误说明)。

    ★ 2026-10-13（用户实拍「无法获取验证码」）：账号服务对**格式非法**的
      手机号不返回业务错误，而是甩一个笼统的
      {"code":"010003","desc":"请求参数错误"} —— 用户完全看不出是手机号
      写错了（22644212509 这种号段不存在的号实测就这个错；合法号段
      13800138000 同一套签名直接返回 msgid）。前端/后端都先过这道归一化：
      剥离 +86 / 86 前缀、空格、横线、括号，再用 ^1[3-9]\\d{9}$ 校验。
    """
    s = re.sub(r"[\s\-()（）]+", "", str(phone or ""))
    if s.startswith("+86"):
        s = s[3:]
    elif s.startswith("0086"):
        s = s[4:]
    elif s.startswith("86") and len(s) == 13:
        s = s[2:]
    if not re.fullmatch(r"1[3-9]\d{9}", s):
        return False, ("手机号格式不正确（%s）：请填 11 位大陆手机号，"
                       "支持带 +86/86 前缀，不要带空格或横线" % (str(phone)[:24]))
    return True, s


def _traceparent():
    """00-<32hex>-<16hex>-01（模型网关必需，缺了请求会挂起到超时）。"""
    return "00-%s-%s-01" % (uuid.uuid4().hex, uuid.uuid4().hex[:16])


def _envelope(param):
    """账号服务请求信封：{"base": {...}, "param": {...}}。"""
    return {
        "base": {
            "appid": APPID, "modelid": MODELID, "version": CLIENT_VERSION,
            "devid": DEVICE_ID, "ua": CLIENT_UA,
            "traceid": uuid.uuid4().hex,
        },
        "param": param or {},
    }


def _esc(segment):
    """RFC3986 per-segment escape（encodeURIComponent 再补 !'()*）。"""
    return urllib.parse.quote(str(segment), safe="-_.~")


def _signed_headers_text(x_headers):
    """返回 (SignedHeaders, CanonicalizedHeaders)。x-* 头按名排序；
    没有时两者都是空串——但**不能省略**（签名串固定九行）。"""
    names = sorted(k.lower() for k in (x_headers or {}) if k.lower().startswith("x-"))
    signed = ";".join(names)
    canon = "".join("%s:%s\n" % (n, (x_headers or {}).get(n) or
                                 (x_headers or {}).get(n.lower()) or "")
                    for n in names)
    return signed, canon


def sign(method, path, query="", body_text="", x_headers=None):
    """HMAC-SHA1 签名 → Authorization 头值（account <akid>:<sig>）。
    返回 (auth_value, date, nonce)。"""
    date = email.utils.formatdate(usegmt=True)
    nonce = str(uuid.uuid4())
    esc_path = "/".join(_esc(seg) for seg in path.split("/"))
    esc_query = _esc(query) if query else ""
    md5 = base64.b64encode(hashlib.md5(body_text.encode("utf-8")).digest()).decode() \
        if body_text else ""
    signed, canon = _signed_headers_text(x_headers)
    sts = "\n".join([method.upper(), esc_path, esc_query, md5,
                     "application/json", date, nonce, signed, canon])
    sig = base64.b64encode(hmac.new(AK_SECRET.encode("utf-8"),
                                    sts.encode("utf-8"),
                                    hashlib.sha1).digest()).decode()
    return "account %s:%s" % (AK_ID, sig), date, nonce


def account_post(path, param=None, query="", timeout=20):
    """调账号服务（自动签名 + 信封）。返回解析后的 JSON dict。

    ★ 实测签名要点（文档没写全的三处）：
      · Content-MD5 = base64(md5(body))，**必须同时出现在请求头和签名串**
        （只放签名串会报 "HMAC signature body does not match"）；
      · 签名串固定九行，MD5 行不能省；
      · Date/Nonce 头要与签名串里的值一致。"""
    body_obj = _envelope(param)
    body_text = json.dumps(body_obj, ensure_ascii=False, separators=(",", ":"))
    body_md5 = base64.b64encode(hashlib.md5(body_text.encode("utf-8")).digest()).decode()
    # ★ 注意：sign() 内部会对 body_text 再算一次 MD5 —— 这里传原始 body，
    #   不能把 body_md5 传进去（二次哈希会导致签名不匹配）。
    auth, date, nonce = sign("POST", path, query, body_text)
    url = ACCOUNT_BASE + path + (("?" + query) if query else "")
    h = {
        "Authorization": auth,
        "Content-Type": "application/json",
        "Content-MD5": body_md5,
        "Accept": "application/json, text/plain, */*",
        "Date": date,
        "Nonce": nonce,
        "User-Agent": CLIENT_UA,
        "Origin": "https://loomy.xunfei.cn",
        "Referer": "https://loomy.xunfei.cn/",
    }
    req = urllib.request.Request(url, data=body_text.encode("utf-8"),
                                 headers=h, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_SSL) as x:
            return json.loads(x.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return json.loads(raw)
        except Exception:
            return {"_http": e.code, "_raw": raw[:300]}
    except Exception as e:
        return {"_http": 0, "_err": str(e)[:180]}


# ---------------------------------------------------------------- 登录
def send_sms(phone):
    """发送短信验证码 → msgid（verify 时必须带回）。"""
    ok, ph = norm_phone(phone)
    if not ok:
        return False, ph
    d = account_post("/login/phone/sendMsgCode",
                     {"ccode": "86", "phone": ph, "expire": 300})
    data = d.get("data") if isinstance(d, dict) else None
    if isinstance(data, dict) and data.get("msgid"):
        return True, data["msgid"]
    # 010003「请求参数错误」在号段非法时出现（实测），给人能看懂的说明
    if isinstance(d, dict) and str(d.get("code")) == "010003":
        return False, ("账号服务返回「请求参数错误」——多数情况是手机号号段不存在"
                       "（请确认号码无误），或该号码未注册讯飞账号")
    return False, str(d)[:200]


def verify_sms(phone, code, msgid):
    """短信验证码校验 → session（32 字符）/userid。"""
    ok, ph = norm_phone(phone)
    if not ok:
        return False, ph
    d = account_post("/login/phone/checkCode",
                     {"ccode": "86", "phone": ph, "mcode": str(code),
                      "msgid": str(msgid), "expire": SESSION_EXPIRE})
    data = d.get("data") if isinstance(d, dict) else None
    if isinstance(data, dict) and data.get("session"):
        return True, {"session": data["session"], "userid": data.get("userid") or "",
                      "phone": data.get("phone") or ph}
    return False, str(d)[:220]


def pwd_login(phone, password):
    """密码登录（getPuKey → RSA PKCS1v15 → byPwd；rcode 是服务器 nonce）。"""
    ok, ph = norm_phone(phone)
    if not ok:
        return False, ph
    d = account_post("/login/account/getPuKey", {})
    data = d.get("data") if isinstance(d, dict) else None
    if not isinstance(data, dict) or not data.get("pukey") or not data.get("rcode"):
        return False, "getPuKey 失败：" + str(d)[:200]
    enc = _rsa_encrypt(data["pukey"], str(password))
    d2 = account_post("/login/account/byPwd",
                      {"loginid": ph, "password": enc,
                       "rcode": data["rcode"], "type": 1,
                       "expire": SESSION_EXPIRE})
    data2 = d2.get("data") if isinstance(d2, dict) else None
    if isinstance(data2, dict) and data2.get("session"):
        return True, {"session": data2["session"],
                      "userid": data2.get("userid") or "",
                      "phone": data2.get("phone") or ph}
    return False, str(d2)[:220]


# ------------------------------------------------------- RSA PKCS1v15（纯标准库）
def _der_read_len(data, i):
    b = data[i]; i += 1
    if b & 0x80:
        n = b & 0x7F
        val = int.from_bytes(data[i:i + n], "big"); i += n
    else:
        val = b
    return val, i


def _rsa_encrypt(pubkey_b64, plaintext):
    """RSA PKCS1 v1.5 加密。公钥是 **SPKI** 格式：
    SEQUENCE{ SEQUENCE{OID,NULL}, BITSTRING{ SEQUENCE{INTEGER n, INTEGER e} } }"""
    der = base64.b64decode(pubkey_b64)
    i = 0
    if der[i] != 0x30:
        raise ValueError("公钥 DER 形状不对")
    _ln, i = _der_read_len(der, i + 1)
    if der[i] != 0x30:                      # AlgorithmIdentifier
        raise ValueError("公钥缺算法块")
    _la, i = _der_read_len(der, i + 1)
    i += _la                                # 跳过 OID+NULL
    if der[i] != 0x03:                      # BIT STRING
        raise ValueError("公钥缺 BIT STRING")
    _lb, i = _der_read_len(der, i + 1)
    i += 1                                  # BIT STRING 的未用位数占位字节
    if der[i] != 0x30:
        raise ValueError("公钥缺 RSAPublicKey")
    _lr, i = _der_read_len(der, i + 1)
    if der[i] != 0x02:
        raise ValueError("DER 缺 modulus")
    i += 1
    ln, i = _der_read_len(der, i)
    n = int.from_bytes(der[i:i + ln], "big"); i += ln
    if der[i] != 0x02:
        raise ValueError("DER 缺 exponent")
    i += 1
    le, i = _der_read_len(der, i)
    e = int.from_bytes(der[i:i + le], "big")
    k = (n.bit_length() + 7) // 8
    m = plaintext.encode("utf-8")
    if len(m) > k - 11:
        raise ValueError("密码过长")
    ps_len = k - 3 - len(m)
    ps = bytes(x for x in os.urandom(ps_len + 16) if x != 0)[:ps_len]
    em = b"\x00\x02" + ps + b"\x00" + m
    c = pow(int.from_bytes(em, "big"), e, n)
    return base64.b64encode(c.to_bytes(k, "big")).decode()


# ------------------------------------------------------- 模型网关
def gateway_headers(session):
    """模型网关请求头（每次调用 traceparent 都要新的）。"""
    return {
        "Authorization": "Bearer " + session,
        "token": session,                      # 两种拼写服务端都收
        "traceparent": _traceparent(),         # 缺了请求会挂起到超时
        "loomy-version": LOOMY_VERSION,
    }


def models(session):
    """GET /api/v1/models → 实时模型目录（含倍率/上下文/能力）。"""
    url = MODEL_BASE + "/models"
    req = urllib.request.Request(url, headers=dict(
        gateway_headers(session), Accept="application/json"))
    try:
        with urllib.request.urlopen(req, timeout=20, context=_SSL) as x:
            return json.loads(x.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return json.loads(raw)
        except Exception:
            return {"_http": e.code, "_raw": raw[:300]}
    except Exception as e:
        return {"_http": 0, "_err": str(e)[:180]}
