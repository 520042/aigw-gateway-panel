const $ = (s) => document.querySelector(s);
let loginTimer = null;

async function api(path, opts) {
  const r = await fetch(path, opts);
  return r.json();
}

function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"]/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

async function loadState() {
  const st = await api("/api/state");
  renderProxy(st.proxy);
  renderAccounts(st.accounts);
  renderConfig(st.config);
  renderLogin(st.login);
}

function renderProxy(p) {
  const badge = $("#proxy-badge");
  if (p.running) {
    badge.textContent = "运行中";
    badge.className = "badge on";
  } else {
    badge.textContent = "未运行";
    badge.className = "badge off";
  }
  $("#proxy-addr").textContent = p.listen || "-";
  $("#proxy-key").textContent = p.api_key || "(无)";
  const host = "http://127.0.0.1" + (p.listen || ":8367");
  $("#proxy-base").textContent = host + "/v1";
  $("#btn-start").disabled = p.running;
  $("#btn-stop").disabled = !p.running;
}

function renderAccounts(list) {
  const box = $("#accounts");
  if (!list || list.length === 0) {
    box.innerHTML = '<div class="hint">还没有账号，请先登录或导入 Cookie。</div>';
    return;
  }
  box.innerHTML = list.map((a) => {
    let st = "ok", stText = "正常";
    if (a.disabled) { st = "dis"; stText = "已禁用"; }
    else if (a.cooling) { st = "cool"; stText = "冷却中"; }
    const creditCls = a.credits > 0 ? "good" : "empty";
    return `<div class="acct">
      <div class="info">
        <div class="nick">${esc(a.nickname || a.uid)} <span class="state ${st}">${stText}</span></div>
        <div class="meta">UID ${esc(a.uid)}${a.reason ? " · " + esc(a.reason) : ""}</div>
      </div>
      <div class="row">
        <div class="credit ${creditCls}">${a.credits ?? 0} 积分</div>
        <button class="del" data-uid="${esc(a.uid)}">删除</button>
      </div>
    </div>`;
  }).join("");
  box.querySelectorAll(".del").forEach((b) => {
    b.onclick = async () => {
      if (!confirm("确定删除账号 " + b.dataset.uid + " ?")) return;
      await api("/api/accounts/delete?uid=" + encodeURIComponent(b.dataset.uid), { method: "POST" });
      loadState();
    };
  });
}

function renderConfig(c) {
  if (!c) return;
  $("#cfg-key").value = c.api_key || "";
  $("#cfg-listen").value = c.listen || "";
  $("#cfg-upstream").value = c.upstream_base || "";
  $("#cfg-portal").value = c.login_portal || "";
  $("#cfg-listen").placeholder = ":8367";
}

function renderLogin(l) {
  if (l && l.active && l.done) {
    $("#login-status").textContent = "登录成功：" + (l.account || "");
    $("#login-status").className = "msg ok";
  } else if (l && l.active && l.error) {
    $("#login-status").textContent = "登录失败：" + l.error;
    $("#login-status").className = "msg err";
  }
}

// --- 事件绑定 ---

$("#btn-start").onclick = async () => {
  $("#btn-start").disabled = true;
  const r = await api("/api/proxy/start", { method: "POST" });
  if (!r.ok) alert("启动失败：" + r.error);
  loadState();
};
$("#btn-stop").onclick = async () => {
  await api("/api/proxy/stop", { method: "POST" });
  loadState();
};
$("#btn-refresh").onclick = async () => {
  $("#btn-refresh").disabled = true;
  $("#btn-refresh").textContent = "刷新中...";
  await api("/api/accounts/refresh", { method: "POST" });
  await loadState();
  $("#btn-refresh").disabled = false;
  $("#btn-refresh").textContent = "刷新额度";
};

$("#btn-login").onclick = async () => {
  const r = await api("/api/login/start", { method: "POST" });
  if (!r.ok) { alert("启动登录失败：" + r.error); return; }
  $("#login-box").classList.remove("hidden");
  $("#login-url").href = r.url;
  $("#login-url").textContent = r.url;
  $("#login-status").textContent = "等待登录...";
  $("#login-status").className = "kv";
  window.open(r.url, "_blank");
  if (loginTimer) clearInterval(loginTimer);
  loginTimer = setInterval(async () => {
    const st = await api("/api/state");
    renderLogin(st.login);
    if (st.login && (st.login.done || st.login.error)) {
      clearInterval(loginTimer);
      loadState();
    }
  }, 2000);
};

$("#btn-import").onclick = async () => {
  const raw = $("#cookie-input").value.trim();
  const m = $("#cookie-msg");
  const box = $("#cookie-results");
  box.innerHTML = "";
  if (!raw) { m.textContent = "请先粘贴 Cookie"; m.className = "msg err"; return; }
  const items = raw.split(/\r?\n/).map((x) => x.trim()).filter(Boolean);
  $("#btn-import").disabled = true;
  const r = await api("/api/cookie/import", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ items }),
  });
  $("#btn-import").disabled = false;
  if (r.error && !r.results) {
    m.textContent = "导入失败：" + r.error;
    m.className = "msg err";
    return;
  }
  m.textContent = `导入完成：成功 ${r.success} 条，失败 ${r.failed} 条`;
  m.className = "msg " + (r.success > 0 ? "ok" : "err");
  box.innerHTML = (r.results || []).map((x) => {
    const ok = x.ok === true;
    return `<div class="res ${ok ? "ok" : "err"}">${ok ? "✓" : "✗"} ${esc(x.nickname || x.uid || x.input || "")}${ok ? "" : " - " + esc(x.error || "")}</div>`;
  }).join("");
  $("#cookie-input").value = "";
  loadState();
};

$("#btn-client-import").onclick = async () => {
  const m = $("#client-msg");
  const box = $("#client-results");
  box.innerHTML = "";
  $("#btn-client-import").disabled = true;
  const r = await api("/api/client/import", { method: "POST" });
  $("#btn-client-import").disabled = false;
  if (r.error && !r.results) {
    m.textContent = "导入失败：" + r.error;
    m.className = "msg err";
    return;
  }
  m.textContent = `导入完成：成功 ${r.success} 条，失败 ${r.failed} 条`;
  m.className = "msg " + (r.success > 0 ? "ok" : "err");
  box.innerHTML = (r.results || []).map((x) => {
    const ok = x.ok === true;
    return `<div class="res ${ok ? "ok" : "err"}">${ok ? "✓" : "✗"} ${esc(x.nickname || x.uid || x.input || "")}${ok ? "" : " - " + esc(x.error || "")}</div>`;
  }).join("");
  loadState();
};

$("#cfg-toggle").onclick = () => {
  $("#cfg-body").classList.toggle("hidden");
  document.querySelector(".chev").classList.toggle("open");
};
$("#btn-save-cfg").onclick = async () => {
  const body = {
    api_key: $("#cfg-key").value,
    listen: $("#cfg-listen").value,
    upstream_base: $("#cfg-upstream").value,
    login_portal: $("#cfg-portal").value,
  };
  const r = await api("/api/config", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const m = $("#cfg-msg");
  m.textContent = r.ok ? "已保存（代理需重启生效）" : "保存失败";
  m.className = "msg " + (r.ok ? "ok" : "err");
};

// tabs
document.querySelectorAll(".tab").forEach((t) => {
  t.onclick = () => {
    document.querySelectorAll(".tab").forEach((x) => x.classList.remove("active"));
    t.classList.add("active");
    document.querySelectorAll(".tabpane").forEach((p) => p.classList.add("hidden"));
    $("#tab-" + t.dataset.tab).classList.remove("hidden");
  };
});

loadState();
setInterval(loadState, 5000);
