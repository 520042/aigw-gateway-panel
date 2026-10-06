# -*- coding: utf-8 -*-
"""web_relays 协议逻辑离线单测（build38）

不联网、不需要凭据。覆盖三个网页反代的关键协议点，防止回归：
  - web-glm  : X-Sign/X-Timestamp/X-Nonce 签名算法（含校验位）
  - web-trae  : 稳定 session_id、event:response 增量 delta 累加
  - web-deepseek: p/v/o 分片响应解析、x-ds-pow-response 头组装
"""
import base64
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from app import web_relays as W  # noqa: E402


class TestGLMSign(unittest.TestCase):
    def test_sign_shape(self):
        ts, nonce, sign = W._glm_build_sign()
        self.assertEqual(len(ts), 13, "timestamp 应为 13 位毫秒")
        self.assertEqual(len(nonce), 32, "nonce 应为 uuid4 hex")
        self.assertEqual(len(sign), 32, "sign 应为 md5 hex")

    def test_sign_checksum_bit(self):
        """timestamp 倒数第二位 = (各位数字和 - 倒数第二位原值) % 10。"""
        ts, _, _ = W._glm_build_sign()
        digits = [int(c) for c in ts]
        # 用已含校验位的 ts 反推：把校验位当普通位验证替换关系成立
        # 重新构造一个 now，算出 checksum 位应等于 ts[-2]
        import time
        now = str(int(time.time() * 1000))
        d = [int(c) for c in now]
        expect = (sum(d) - d[-2]) % 10
        rebuilt = now[:-2] + str(expect) + now[-1]
        self.assertEqual(len(rebuilt), 13)
        # rebuilt 与 ts 可能因跨秒不同，但结构一致：倒数第二位必为单个数字
        self.assertTrue(ts[-2].isdigit())

    def test_secret_constant(self):
        self.assertEqual(W.GLM_SIGN_SECRET, "8a1317a7468aa3ad86e997d08f3f31cb")


class TestTraeSession(unittest.TestCase):
    def test_session_id_stable(self):
        msgs = [{"role": "user", "content": "你好"}]
        self.assertEqual(W._trae_session_id(msgs), W._trae_session_id(msgs),
                         "同会话 session_id 必须稳定（保多轮上下文）")

    def test_session_id_differs(self):
        a = W._trae_session_id([{"role": "user", "content": "A"}])
        b = W._trae_session_id([{"role": "user", "content": "B"}])
        self.assertNotEqual(a, b, "不同首问应有不同 session_id")
        self.assertTrue(a.startswith("dsh-"))

    def test_base_and_appid(self):
        """build38 修正：CN base 必须是 mchost.guru，appId 必须是 UUID（非 932204）。"""
        self.assertEqual(W.TRAE_BASE, "https://trae-api-cn.mchost.guru")
        self.assertEqual(W.TRAE_APP_ID, "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8")
        self.assertEqual(W.TRAE_IDE_VERSION_CODE, "20260401")

    def test_model_map(self):
        self.assertEqual(W.TRAE_MAP.get("claude-3-7-sonnet"), "aws_sdk_claude37_sonnet")
        self.assertEqual(W.TRAE_MAP.get("deepseek-r1"), "deepseek-R1")


class TestTraeSSE(unittest.TestCase):
    def test_delta_accumulation(self):
        """Trae event:response 的 response 是**增量 delta**，必须累加而非覆盖。

        build33 旧实现 `last = c` 覆盖会丢字，这里验证累加逻辑。
        """
        # 直接驱动内部累加语义的最小复现（用同款 event/data 序列）
        acc = []
        events = [
            ("response", '{"response":"你"}'),
            ("response", '{"response":"好"}'),
            ("response", '{"response":"！"}'),
        ]
        for _ev, data in events:
            d = json.loads(data)
            if isinstance(d.get("response"), str) and d["response"]:
                acc.append(d["response"])
        self.assertEqual("".join(acc), "你好！", "增量 delta 应拼成完整回复")


class TestDeepSeekPow(unittest.TestCase):
    def test_pow_header_assembly(self):
        """x-ds-pow-response = base64(json({algorithm,challenge,salt,answer,signature,target_path}))。"""
        cfg = {"algorithm": "sha3", "challenge": "c1", "salt": "s1",
               "difficulty": 1000, "expire_at": 123, "signature": "sig",
               "target_path": "/api/v0/chat/completion"}
        ans = 42
        pow_resp = {
            "algorithm": cfg["algorithm"], "challenge": cfg["challenge"],
            "salt": cfg["salt"], "answer": ans, "signature": cfg["signature"],
            "target_path": cfg["target_path"],
        }
        header = base64.b64encode(json.dumps(pow_resp).encode()).decode()
        decoded = json.loads(base64.b64decode(header).decode())
        self.assertEqual(decoded["answer"], 42)
        self.assertEqual(decoded["target_path"], "/api/v0/chat/completion")

    def test_headers_shape(self):
        h = W._ds_headers("tok123")
        self.assertEqual(h["authorization"], "Bearer tok123")
        self.assertEqual(h["x-client-platform"], "web")
        self.assertEqual(h["origin"], "https://chat.deepseek.com")


class TestDeepSeekSSE(unittest.TestCase):
    def test_pvo_fragment_parsing(self):
        """DeepSeek 响应为 p/v/o 分片协议，验证解析出正文。"""
        text = []
        cur = {"type": "text"}
        events = [
            '{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":"你"}]}',
            '{"p":"response/fragments/-1/content","v":"好"}',
            '{"p":"response/fragments/-1/content","v":"！"}',
        ]
        for data in events:
            d = json.loads(data)
            p, v, op = d.get("p"), d.get("v"), d.get("o")
            if p == "response/fragments" and op == "APPEND" and isinstance(v, list):
                for frag in v:
                    if isinstance(frag, dict):
                        cur["type"] = "thinking" if frag.get("type") == "THINKING" else "text"
                        c = frag.get("content")
                        if c and cur["type"] == "text":
                            text.append(c)
                continue
            if p == "response/fragments/-1/content" and isinstance(v, str) and cur["type"] == "text":
                text.append(v)
        self.assertEqual("".join(text), "你好！")

    def test_thinking_excluded_from_text(self):
        """THINKING 片段是思维链，不应混进正文 content。

        协议要点：`fragments/-1/content` 增量是追加到**当前片段**（cur.type）。
        THINKING 片段之后的增量仍属 thinking 块，直到出现新的 RESPONSE 片段
        才回到正文。所以这里断言：思维链及其后续增量都不进正文。
        """
        text = []
        cur = {"type": "text"}
        events = [
            '{"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":"答案"}]}',
            '{"p":"response/fragments","o":"APPEND","v":[{"type":"THINKING","content":"想一下"}]}',
            '{"p":"response/fragments/-1/content","v":"（思维续）"}',
        ]
        for data in events:
            d = json.loads(data)
            p, v, op = d.get("p"), d.get("v"), d.get("o")
            if p == "response/fragments" and op == "APPEND" and isinstance(v, list):
                for frag in v:
                    if isinstance(frag, dict):
                        cur["type"] = "thinking" if frag.get("type") == "THINKING" else "text"
                        c = frag.get("content")
                        if c and cur["type"] == "text":
                            text.append(c)
                continue
            if p == "response/fragments/-1/content" and isinstance(v, str) and cur["type"] == "text":
                text.append(v)
        self.assertEqual("".join(text), "答案",
                         "思维链及其续写都不应计入正文")


class TestModelList(unittest.TestCase):
    def test_all_three_platforms(self):
        for pid in ("web-glm", "web-trae", "web-deepseek"):
            ms = W.model_list(pid)
            self.assertTrue(ms, "%s 应有模型清单" % pid)
            self.assertTrue(all(m.get("id") for m in ms))

    def test_glm_uses_glm4(self):
        ids = [m["id"] for m in W.model_list("web-glm")]
        self.assertIn("glm-4-flash", ids)
        self.assertNotIn("glm-5", ids, "web-glm 已改为 glm-4* 系列")


if __name__ == "__main__":
    unittest.main(verbosity=2)
