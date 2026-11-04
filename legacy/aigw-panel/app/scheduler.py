# -*- coding: utf-8 -*-
"""
调度器
======
后台线程，负责：
  1. 网关进程守护（探测 → 未运行则拉起）
  2. 每日定时签到（本地账号池 + 外部中转站）
  3. 每日成长任务（抽奖 / 补签 / 旅行 / 奖励）
  4. 定时刷新用量与额度快照
  5. 签到/额度变化通知

默认 09:00 签到、10:00 成长任务（与 workbuddy-gateway 自身一致），
外部站点签到统一在 09:05 起错峰执行，避免同时打满。
"""

import os
import subprocess
import threading
import time
from datetime import datetime, timedelta

from .store import now_ts, now_str
from .gwclient import GatewayClient, GatewayError
from . import sitecheck
from .notify import Notifier

CST = timedelta(hours=8)


def local_now():
    return datetime.now(CST.__class__(CST)) if False else datetime.utcnow() + CST


class Scheduler(threading.Thread):
    daemon = True

    def __init__(self, store, gateway_path, log):
        super().__init__(name="aigw-scheduler")
        self.store = store
        self.gateway_path = gateway_path
        self.log = log
        self.stop_flag = threading.Event()
        self.gw_proc = None
        self.gw_lock = threading.RLock()
        self.last_checkin_date = None
        self.last_growth_date = None
        self.notifier = Notifier(store)
        self.ticks = 0
        self.last_tick = None

    # ------------------------------------------------------------ 日志
    def _log(self, msg):
        try:
            self.log(msg)
        except Exception:
            pass

    # ------------------------------------------------------------ 网关守护
    def ensure_gateway(self, client):
        """探测网关，未运行则尝试拉起（只在本进程托管时才拉起）"""
        st = client.ping()
        if st.get("alive"):
            return True, "网关已在运行"
        s = self.store.get("settings")
        if not s.get("auto_start_gateway"):
            return False, "网关未运行（已关闭自动启动）"
        if not self.gateway_path or not os.path.exists(self.gateway_path):
            return False, "未找到网关可执行文件"
        with self.gw_lock:
            # 拉起前先登录控制台，避免 setup_required 挡住
            if client.needs_setup() and s.get("gateway_admin_password"):
                try:
                    client.setup(s["gateway_admin_user"], s["gateway_admin_password"])
                except GatewayError as e:
                    self._log("setup 失败：%s" % e.message)
            args = [self.gateway_path, "serve",
                    "-addr", s.get("gateway_addr", "127.0.0.1"),
                    "-port", str(s.get("gateway_port", 8317)),
                    "-api-key", s.get("gateway_api_key", "admin")]
            if s.get("gateway_admin_password"):
                args += ["-admin-password", s["gateway_admin_password"]]
            try:
                logf = open(os.path.join(os.path.dirname(self.gateway_path) or ".",
                                         "aigw-gateway.log"), "a", encoding="utf-8")
                self.gw_proc = subprocess.Popen(
                    args, stdout=logf, stderr=subprocess.STDOUT,
                    cwd=os.path.dirname(self.gateway_path) or ".",
                    creationflags=0x08000000 if os.name == "nt" else 0)  # CREATE_NO_WINDOW
            except Exception as e:
                return False, "启动失败：%s" % e
            self._log("已拉起网关进程 PID=%s" % self.gw_proc.pid)
        for _ in range(20):
            time.sleep(1)
            if client.ping().get("alive"):
                return True, "网关已拉起并就绪"
        return False, "网关启动超时"

    def stop_gateway(self):
        with self.gw_lock:
            if self.gw_proc and self.gw_proc.poll() is None:
                self.gw_proc.terminate()
                try:
                    self.gw_proc.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    self.gw_proc.kill()
                self._log("已停止网关进程")
                return True, "已停止"
            return False, "本进程未托管网关，或网关已退出"

    # ------------------------------------------------------------ 签到
    def run_all_checkins(self, client, force=False):
        """网关账号池签到 + 外部站点签到"""
        s = self.store.get("settings")
        results = []
        gw_note = "未启用"
        if client and client.ping().get("alive"):
            if force:
                try:
                    r = client.do_checkin()
                    gw_note = "触发成功：%s" % (r.get("message") or "已提交")
                    self.store.add_checkin({"site": "wb-gateway", "name": "本地网关账号池",
                                            "ok": True, "message": gw_note, "at": now_str()})
                except GatewayError as e:
                    gw_note = "触发失败：%s" % e.message
                    self.store.add_checkin({"site": "wb-gateway", "name": "本地网关账号池",
                                            "ok": False, "message": gw_note, "at": now_str()})
            else:
                try:
                    st = client.checkins()
                    gw_note = "网关今日已自动签到，统计：%s 条" % (st.get("total") or 0)
                except GatewayError as e:
                    gw_note = "查询失败：%s" % e.message
        results.append({"site": "wb-gateway", "name": "本地网关账号池",
                        "ok": force, "message": gw_note, "kind": "gateway"})

        sites = [x for x in self.store.get("sites") if x.get("checkin")]
        for site in sites:
            r = sitecheck.checkin_site(dict(site, force=force))
            r["kind"] = "site"
            results.append(r)
            self.store.add_checkin({
                "site": site.get("id"), "name": site.get("name"),
                "ok": bool(r.get("ok")), "message": r.get("message"),
                "reward": r.get("reward"), "streak": r.get("streak"),
                "at": now_str(),
            })

        ok_n = len([r for r in results if r.get("ok")])
        skip_n = len([r for r in results if r.get("skipped")])
        fail_n = len([r for r in results if not r.get("ok") and not r.get("skipped")])
        summary = "签到完成：成功 %d / 跳过 %d / 失败 %d（共 %d）" % (ok_n, skip_n, fail_n, len(results))
        detail = "\n".join("· %s：%s" % (r.get("name"), r.get("message")) for r in results)
        self.notifier.notify("checkin", "每日签到 · %s" % s.get("checkin_hour", 9), summary + "\n" + detail)
        return {"ok": fail_n == 0, "summary": summary,
                "success": ok_n, "skipped": skip_n, "failed": fail_n, "results": results}

    def run_growth_tasks(self, client):
        """成长任务：领奖励 / 补签 / 抽奖 / 旅行"""
        if not client or not client.ping().get("alive"):
            return {"ok": False, "summary": "网关未运行，跳过成长任务", "steps": []}
        steps = []
        for label, fn in (("领取成长奖励", client.bonus), ("补签", client.makeup),
                          ("抽奖", client.lottery), ("旅行", client.travel)):
            try:
                r = fn()
                msg = r.get("message") or (r.get("data") or {}).get("message") or "已提交"
                steps.append({"step": label, "ok": True, "message": str(msg)[:200]})
            except GatewayError as e:
                steps.append({"step": label, "ok": False, "message": e.message})
        ok_n = len([x for x in steps if x["ok"]])
        summary = "成长任务完成：%d/%d 成功" % (ok_n, len(steps))
        self.notifier.notify("growth", "成长任务", summary + "\n" +
                             "\n".join("· %s：%s" % (x["step"], x["message"]) for x in steps))
        return {"ok": ok_n == len(steps), "summary": summary, "steps": steps}

    # ------------------------------------------------------------ 主循环
    def run(self):
        self._log("调度器已启动（仅在启用「自动拉起网关」时才会启动）")
        client = self._make_client()
        # 启动后先静默观察一段时间，避免刚开面板就补跑当天的签到
        self.stop_flag.wait(45)
        # 注意：APP 平台自动签到已移到 main.py 独立启动（AutoCheckin 线程），
        # 不再挂在这里 —— 否则「不自动拉起网关」时定时签到会跟着停。
        while not self.stop_flag.is_set():
            try:
                self.ticks += 1
                self.last_tick = now_str()
                s = self.store.get("settings")
                if self.ticks == 1 or self.ticks % 30 == 0:
                    ok, msg = self.ensure_gateway(client)
                    if not ok:
                        self._log("网关状态：%s" % msg)
                dt = datetime.utcnow() + CST
                today = dt.strftime("%Y-%m-%d")
                hh = dt.hour + dt.minute / 60.0
                ch = int(s.get("checkin_hour", 9))
                gh = int(s.get("growth_hour", 10))

                if (s.get("auto_checkin") and hh >= ch + 0.08
                        and self.last_checkin_date != today):
                    self.last_checkin_date = today
                    self._log("触发每日签到")
                    r = self.run_all_checkins(client)
                    self._log(r["summary"])

                if (s.get("auto_growth") and hh >= gh + 0.08
                        and self.last_growth_date != today):
                    self.last_growth_date = today
                    self._log("触发成长任务")
                    r = self.run_growth_tasks(client)
                    self._log(r["summary"])
            except Exception as e:
                self._log("调度循环异常：%s" % e)
            self.stop_flag.wait(20)
        self._log("调度器已退出")

    def _make_client(self):
        s = self.store.get("settings")
        c = GatewayClient(
            addr=s.get("gateway_addr", "127.0.0.1"),
            port=s.get("gateway_port", 8317),
            api_key=s.get("gateway_api_key", "admin"),
            admin_user=s.get("gateway_admin_user", "admin"),
            admin_password=s.get("gateway_admin_password", ""))
        if c.admin_password:
            try:
                c.login()
            except Exception:
                pass
        return c
