// handlers.go GUI 后端接口实现。
package main

import (
	"context"
	"log"
	"net/http"
	"path/filepath"
	"strings"
	"time"

	"lobsterai2api/internal/auth"
	"lobsterai2api/internal/server"
)

// handleState 返回账号池、配置、代理运行状态。
func (a *App) handleState(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, map[string]any{
		"accounts": a.pool.List(),
		"proxy": map[string]any{
			"running": a.proxyRun,
			"listen":  a.cfg.Listen,
			"api_key": a.cfg.APIKey,
		},
		"config": map[string]any{
			"upstream_base": a.cfg.UpstreamBase,
			"login_portal":  a.cfg.LoginPortal,
			"auth_dir":      a.cfg.AuthDir,
		},
		"login": a.loginStatusPayload(),
	})
}

// handleRefreshAccounts 重新扫描 auths 目录并刷新所有额度。
func (a *App) handleRefreshAccounts(w http.ResponseWriter, r *http.Request) {
	auths, err := auth.LoadDir(a.cfg.AuthDir)
	if err != nil {
		writeJSON(w, map[string]any{"ok": false, "error": err.Error()})
		return
	}
	a.pool.SyncToDir(auths)
	a.refreshAllCredits()
	writeJSON(w, map[string]any{"ok": true, "count": len(auths)})
}

// handleDeleteAccount 删除指定账号的 auth 文件和池条目。
func (a *App) handleDeleteAccount(w http.ResponseWriter, r *http.Request) {
	uid := r.URL.Query().Get("uid")
	if uid == "" {
		writeJSON(w, map[string]any{"ok": false, "error": "uid required"})
		return
	}
	acct := a.pool.AuthByUID(uid)
	if acct != nil && acct.FilePath != "" {
		_ = removeFile(acct.FilePath)
	}
	a.reloadPool()
	writeJSON(w, map[string]any{"ok": true})
}

// handleConfig 读取或更新配置。
func (a *App) handleConfig(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodGet {
		writeJSON(w, a.cfg)
		return
	}
	var in struct {
		APIKey       string `json:"api_key"`
		Listen       string `json:"listen"`
		UpstreamBase string `json:"upstream_base"`
		LoginPortal  string `json:"login_portal"`
	}
	if err := decodeJSON(r, &in); err != nil {
		writeJSON(w, map[string]any{"ok": false, "error": err.Error()})
		return
	}
	if strings.TrimSpace(in.APIKey) != "" {
		a.cfg.APIKey = trim(in.APIKey)
	}
	if strings.TrimSpace(in.Listen) != "" {
		a.cfg.Listen = trim(in.Listen)
	}
	if strings.TrimSpace(in.UpstreamBase) != "" {
		a.cfg.UpstreamBase = trim(in.UpstreamBase)
	}
	if strings.TrimSpace(in.LoginPortal) != "" {
		a.cfg.LoginPortal = trim(in.LoginPortal)
	}
	_ = saveConfig(filepath.Join(a.root, "config.json"), a.cfg)
	writeJSON(w, map[string]any{"ok": true})
}

// handleProxyStart 启动 OpenAI 兼容代理。
func (a *App) handleProxyStart(w http.ResponseWriter, r *http.Request) {
	if a.proxyRun {
		writeJSON(w, map[string]any{"ok": true, "already": true, "listen": a.cfg.Listen})
		return
	}
	h := server.NewHandler(server.Config{
		Pool:         a.pool,
		Upstream:     a.up,
		APIKey:       a.cfg.APIKey,
		HardCooldown: a.cfg.HardCreditDur,
		SoftCooldown: a.cfg.SoftRateDur,
		ErrThreshold: a.cfg.Cooldown.ErrThresh,
		ErrCooldown:  a.cfg.ErrCooldownDur,
	})
	srv := &http.Server{Addr: a.cfg.Listen, Handler: h, ReadHeaderTimeout: 30 * time.Second}
	ready := make(chan error, 1)
	go func() {
		time.Sleep(200 * time.Millisecond)
		err := srv.ListenAndServe()
		if err != nil && err != http.ErrServerClosed {
			ready <- err
		}
	}()
	select {
	case err := <-ready:
		writeJSON(w, map[string]any{"ok": false, "error": err.Error()})
		return
	case <-time.After(400 * time.Millisecond):
	}
	a.proxySrv = srv
	a.proxyRun = true
	// 启动调度器
	ctx, cancel := context.WithCancel(context.Background())
	a.schedCancel = cancel
	go schedulerRun(ctx, a)
	log.Printf("proxy started on %s", a.cfg.Listen)
	writeJSON(w, map[string]any{"ok": true, "listen": a.cfg.Listen, "api_key": a.cfg.APIKey})
}

// handleProxyStop 停止代理。
func (a *App) handleProxyStop(w http.ResponseWriter, r *http.Request) {
	a.stopProxy()
	writeJSON(w, map[string]any{"ok": true})
}

func (a *App) stopProxy() {
	if a.schedCancel != nil {
		a.schedCancel()
		a.schedCancel = nil
	}
	if a.proxySrv != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = a.proxySrv.Shutdown(ctx)
		a.proxySrv = nil
	}
	a.proxyRun = false
}

// reloadPool 重新扫描 auths 目录并同步池。
func (a *App) reloadPool() {
	auths, err := auth.LoadDir(a.cfg.AuthDir)
	if err != nil {
		return
	}
	a.pool.SyncToDir(auths)
}

// refreshAllCredits 遍历池内账号刷新积分。
func (a *App) refreshAllCredits() {
	for _, st := range a.pool.List() {
		acct := a.pool.AuthByUID(st.UID)
		if acct == nil {
			continue
		}
		remain, _, err := a.up.QuotaUsage(acct)
		if err != nil {
			log.Printf("quota uid=%s: %v", st.UID, err)
			continue
		}
		a.pool.SetCredits(st.UID, remain)
	}
}


