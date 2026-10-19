// login.go GUI 内的 OAuth 登录与 Cookie 导入。
package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"lobsterai2api/internal/auth"
	"lobsterai2api/internal/scheduler"
)

const loginCallbackHost = "127.0.0.1"

// loginSession 一次登录会话的状态。
type loginSession struct {
	mu       sync.Mutex
	active   bool
	port     int
	state    string
	uuid     string
	firstKF  string
	url      string
	done     bool
	err      string
	account  string
	srv      *http.Server
	createdA time.Time
}

func (a *App) loginStatusPayload() map[string]any {
	a.loginStateMu.Lock()
	defer a.loginStateMu.Unlock()
	if a.loginState == nil {
		return map[string]any{"active": false}
	}
	ls := a.loginState
	ls.mu.Lock()
	defer ls.mu.Unlock()
	return map[string]any{
		"active":  ls.active,
		"url":     ls.url,
		"done":    ls.done,
		"error":   ls.err,
		"account": ls.account,
	}
}

// handleLoginStatus 返回当前登录会话状态（供前端轮询）。
func (a *App) handleLoginStatus(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, a.loginStatusPayload())
}

// handleLoginStart 启动本地回调服务器并返回登录 URL。
func (a *App) handleLoginStart(w http.ResponseWriter, r *http.Request) {
	a.loginStateMu.Lock()
	// 关闭上一个会话
	if a.loginState != nil && a.loginState.srv != nil {
		_ = a.loginState.srv.Close()
	}
	state := randomHex(16)
	uuid := newUUID()
	firstKF := nowMillis()

	ln, err := net.Listen("tcp", loginCallbackHost+":0")
	if err != nil {
		a.loginStateMu.Unlock()
		writeJSON(w, map[string]any{"ok": false, "error": "listen: " + err.Error()})
		return
	}
	port := ln.Addr().(*net.TCPAddr).Port
	ls := &loginSession{active: true, port: port, state: state, uuid: uuid, firstKF: firstKF, createdA: time.Now()}

	callbackPath := "/auth/callback"
	mux := http.NewServeMux()
	mux.HandleFunc(callbackPath, func(cw http.ResponseWriter, cr *http.Request) {
		q := cr.URL.Query()
		code := q.Get("code")
		gotState := q.Get("state")
		if code == "" || gotState != state {
			cw.Header().Set("Content-Type", "text/plain; charset=utf-8")
			cw.WriteHeader(http.StatusBadRequest)
			io.WriteString(cw, "登录回调参数无效")
			return
		}
		uid, nickname, exErr := a.exchangeCode(code, uuid, firstKF)
		ls.mu.Lock()
		if exErr != nil {
			ls.err = exErr.Error()
		} else {
			ls.done = true
			ls.account = nickname + " (" + uid + ")"
		}
		ls.mu.Unlock()
		a.reloadPool()
		a.refreshAllCredits()
		cw.Header().Set("Content-Type", "text/html; charset=utf-8")
		if exErr != nil {
			io.WriteString(cw, "<html><body><h2>登录失败："+exErr.Error()+"</h2></body></html>")
			return
		}
		io.WriteString(cw, "<html><body><h2>登录成功，请关闭本页面并回到控制台</h2></body></html>")
	})

	srv := &http.Server{Handler: mux}
	go func() { _ = srv.Serve(ln) }()
	ls.srv = srv

	redirectURI := fmt.Sprintf("http://%s:%d%s", loginCallbackHost, port, callbackPath)
	loginURL := fmt.Sprintf("%s/portal#/login?source=electron&redirect_uri=%s&state=%s",
		a.cfg.LoginPortal, urlEscape(redirectURI), state)
	ls.url = loginURL

	a.loginState = ls
	a.loginStateMu.Unlock()

	log.Printf("login url: %s", loginURL)
	writeJSON(w, map[string]any{"ok": true, "url": loginURL})
}

// exchangeCode 用 authCode 换 token 并保存 auth 文件。
func (a *App) exchangeCode(code, uuid, firstKF string) (uid, nickname string, err error) {
	body := map[string]any{
		"authCode":      code,
		"firstKeyfrom":  firstKF,
		"latestKeyfrom": nowMillis(),
		"uuid":          uuid,
		"version":       "0.1.0",
	}
	raw, _ := json.Marshal(body)
	req, reqErr := http.NewRequest(http.MethodPost, a.cfg.UpstreamBase+"/api/auth/exchange", bytes.NewReader(raw))
	if reqErr != nil {
		return "", "", reqErr
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "LobsterAI/0.1.0")
	resp, doErr := http.DefaultClient.Do(req)
	if doErr != nil {
		return "", "", doErr
	}
	defer resp.Body.Close()
	respRaw, _ := io.ReadAll(resp.Body)
	log.Printf("exchange raw response: %s", truncate(string(respRaw), 500))
	if resp.StatusCode >= 400 {
		return "", "", fmt.Errorf("upstream %d: %s", resp.StatusCode, truncate(string(respRaw), 200))
	}
	var env struct {
		Code    int             `json:"code"`
		Message string          `json:"message"`
		Msg     string          `json:"msg"`
		Data    json.RawMessage `json:"data"`
	}
	_ = json.Unmarshal(respRaw, &env)
	if env.Code != 0 {
		m := env.Message
		if m == "" {
			m = env.Msg
		}
		return "", "", fmt.Errorf("code=%d %s", env.Code, m)
	}
	// token 可能在 data 里，也可能直接在顶层，两种都兼容
	payload := env.Data
	if len(payload) == 0 {
		payload = respRaw
	}
	var ex struct {
		AccessToken  string `json:"accessToken"`
		RefreshToken string `json:"refreshToken"`
		ExpiresIn    int64  `json:"expiresIn"`
		User         struct {
			ID       string `json:"id"`
			Yid      string `json:"yid"`
			UserId   string `json:"userId"`
			Nickname string `json:"nickname"`
		} `json:"user"`
	}
	if err := json.Unmarshal(payload, &ex); err != nil {
		return "", "", fmt.Errorf("parse: %w", err)
	}
	if ex.AccessToken == "" {
		// data 非对象（如字符串）时退回整体解析
		_ = json.Unmarshal(respRaw, &ex)
	}
	if ex.AccessToken == "" {
		return "", "", fmt.Errorf("no accessToken in response: %s", truncate(string(respRaw), 200))
	}
	uid = ex.User.ID
	if uid == "" {
		uid = ex.User.UserId
	}
	if uid == "" {
		uid = ex.User.Yid
	}
	if uid == "" {
		uid = jwtSub(ex.AccessToken)
	}
	expiresAt := int64(0)
	if ex.ExpiresIn > 0 {
		expiresAt = time.Now().Add(time.Duration(ex.ExpiresIn) * time.Second).Unix()
	} else if exp := jwtExpiry(ex.AccessToken); exp > 0 {
		expiresAt = exp
	}
	if err := a.saveAuth(&auth.Auth{
		AccessToken:   ex.AccessToken,
		RefreshToken:  ex.RefreshToken,
		ExpiresAt:     expiresAt,
		UID:           uid,
		UserId:        ex.User.UserId,
		Nickname:      ex.User.Nickname,
		Uuid:          uuid,
		FirstKeyfrom:  firstKF,
		LatestKeyfrom: nowMillis(),
	}); err != nil {
		return "", "", err
	}
	return uid, ex.User.Nickname, nil
}

// handleCookieImport 解析 Cookie/JSON 并保存为账号。
// 支持单条或多条（一行一条），逐条处理并返回每条结果。
func (a *App) handleCookieImport(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Cookie string   `json:"cookie"`
		Items  []string `json:"items"`
	}
	if err := decodeJSON(r, &in); err != nil {
		writeJSON(w, map[string]any{"ok": false, "error": err.Error()})
		return
	}

	// 组装输入行：显式 items 优先，否则按换行切分 cookie 字段
	var lines []string
	if len(in.Items) > 0 {
		lines = in.Items
	} else {
		raw := strings.ReplaceAll(in.Cookie, "\r\n", "\n")
		for _, ln := range strings.Split(raw, "\n") {
			lines = append(lines, ln)
		}
	}

	var results []map[string]any
	okCount := 0
	for _, ln := range lines {
		ln = strings.TrimSpace(ln)
		if ln == "" {
			continue
		}
		res := a.importOne(ln)
		if res["ok"] == true {
			okCount++
		}
		results = append(results, res)
	}
	if len(results) == 0 {
		writeJSON(w, map[string]any{"ok": false, "error": "没有可导入的内容"})
		return
	}
	a.reloadPool()
	a.refreshAllCredits()
	writeJSON(w, map[string]any{
		"ok":      okCount > 0,
		"total":   len(results),
		"success": okCount,
		"failed":  len(results) - okCount,
		"results": results,
	})
}

// importOne 导入单条 Cookie/JSON，返回结果 map。
func (a *App) importOne(val string) map[string]any {
	access, refresh := extractTokens(val)
	if access == "" {
		return map[string]any{"ok": false, "error": "未能识别出 accessToken", "input": truncate(val, 40)}
	}
	acct := &auth.Auth{
		AccessToken:   access,
		RefreshToken:  refresh,
		ExpiresAt:     jwtExpiry(access),
		Uuid:          newUUID(),
		FirstKeyfrom:  nowMillis(),
		LatestKeyfrom: nowMillis(),
	}
	uid, nickname, perr := a.probeAccount(acct)
	if perr != nil || uid == "" {
		uid = jwtSub(access)
		if uid == "" {
			return map[string]any{"ok": false, "error": "无法确定 UID: " + errStr(perr), "input": truncate(val, 40)}
		}
	}
	acct.UID = uid
	acct.Nickname = nickname
	if err := a.saveAuth(acct); err != nil {
		return map[string]any{"ok": false, "error": err.Error(), "input": truncate(val, 40)}
	}
	return map[string]any{"ok": true, "uid": uid, "nickname": nickname}
}

// handleClientImport 从本机 LobsterAI 客户端读取 token 并导入。
func (a *App) handleClientImport(w http.ResponseWriter, r *http.Request) {
	tokens, err := readClientTokens()
	if err != nil {
		writeJSON(w, map[string]any{"ok": false, "error": err.Error()})
		return
	}
	if len(tokens) == 0 {
		writeJSON(w, map[string]any{"ok": false, "error": "未在客户端数据中找到 token（请先在客户端登录）"})
		return
	}
	var results []map[string]any
	okCount := 0
	for _, t := range tokens {
		res := a.importOne("accessToken=" + t.Access + "; refreshToken=" + t.Refresh)
		if res["ok"] == true {
			okCount++
		}
		results = append(results, res)
	}
	a.reloadPool()
	a.refreshAllCredits()
	writeJSON(w, map[string]any{
		"ok":      okCount > 0,
		"total":   len(results),
		"success": okCount,
		"failed":  len(results) - okCount,
		"results": results,
	})
}

// clientToken 从客户端 sqlite 提取出的一组 token。
type clientToken struct {
	Access  string
	Refresh string
}

// readClientTokens 扫描本机 LobsterAI 客户端的 sqlite，提取 auth_tokens。
func readClientTokens() ([]clientToken, error) {
	candidates := clientDataPaths()
	var lastErr error
	for _, db := range candidates {
		if _, err := os.Stat(db); err != nil {
			continue
		}
		toks, err := extractTokensFromSQLite(db)
		if err != nil {
			lastErr = err
			continue
		}
		if len(toks) > 0 {
			return toks, nil
		}
	}
	if lastErr != nil {
		return nil, lastErr
	}
	return nil, fmt.Errorf("未找到 LobsterAI 客户端数据（路径: %s）", strings.Join(candidates, ", "))
}

func clientDataPaths() []string {
	var out []string
	if appData := os.Getenv("APPDATA"); appData != "" {
		out = append(out, filepath.Join(appData, "LobsterAI", "lobsterai.sqlite"))
	}
	if local := os.Getenv("LOCALAPPDATA"); local != "" {
		out = append(out, filepath.Join(local, "LobsterAI", "lobsterai.sqlite"))
	}
	return out
}

// extractTokensFromSQLite 复制 sqlite（避免文件锁）后在字节流里找 auth_tokens 的 JSON。
func extractTokensFromSQLite(dbPath string) ([]clientToken, error) {
	raw, err := os.ReadFile(dbPath)
	if err != nil {
		// 被占用时尝试复制副本
		tmp := filepath.Join(os.TempDir(), "lb2a-sqlite-copy.sqlite")
		if cerr := copyFile(dbPath, tmp); cerr != nil {
			return nil, fmt.Errorf("读取客户端数据库失败: %v", err)
		}
		raw, err = os.ReadFile(tmp)
		if err != nil {
			return nil, err
		}
	}
	text := string(raw)
	var out []clientToken
	seen := map[string]bool{}
	// 找 auth_tokens{"accessToken":"...","refreshToken":"..."}
	re := regexp.MustCompile(`auth_tokens\s*(\{[^}]*"accessToken"[^}]*\})`)
	for _, m := range re.FindAllStringSubmatch(text, -1) {
		var t struct {
			AccessToken  string `json:"accessToken"`
			RefreshToken string `json:"refreshToken"`
		}
		if json.Unmarshal([]byte(m[1]), &t) != nil || t.AccessToken == "" {
			continue
		}
		if seen[t.AccessToken] {
			continue
		}
		seen[t.AccessToken] = true
		out = append(out, clientToken{Access: t.AccessToken, Refresh: t.RefreshToken})
	}
	return out, nil
}

func copyFile(src, dst string) error {
	data, err := os.ReadFile(src)
	if err != nil {
		return err
	}
	return os.WriteFile(dst, data, 0o600)
}

// probeAccount 通过 profile-summary 探测账号，确认 token 有效。
func (a *App) probeAccount(acct *auth.Auth) (uid, nickname string, err error) {
	req, _ := http.NewRequest(http.MethodGet, a.cfg.UpstreamBase+"/api/user/profile-summary", nil)
	req.Header.Set("Authorization", "Bearer "+acct.AccessToken)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "LobsterAI/0.1.0")
	resp, dErr := http.DefaultClient.Do(req)
	if dErr != nil {
		return "", "", dErr
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if resp.StatusCode >= 400 {
		return "", "", fmt.Errorf("upstream %d", resp.StatusCode)
	}
	var env struct {
		Code int `json:"code"`
		Data struct {
			UserID   string `json:"userId"`
			UID      string `json:"uid"`
			Nickname string `json:"nickname"`
		} `json:"data"`
	}
	_ = json.Unmarshal(raw, &env)
	uid = env.Data.UID
	if uid == "" {
		uid = env.Data.UserID
	}
	return uid, env.Data.Nickname, nil
}

// saveAuth 把账号写入 auths/lobsterai-<uid>.json。
func (a *App) saveAuth(acct *auth.Auth) error {
	dir := a.cfg.AuthDir
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	acct.FilePath = filepath.Join(dir, fmt.Sprintf("lobsterai-%s.json", acct.UID))
	return acct.SaveAtomic()
}

// --- token 提取 ---

// extractTokens 从 Cookie 字符串或 JSON 中提取 access/refresh token。
func extractTokens(s string) (access, refresh string) {
	// 1) 先尝试整体按 JSON 解析
	if strings.HasPrefix(strings.TrimSpace(s), "{") {
		var m map[string]any
		if json.Unmarshal([]byte(s), &m) == nil {
			access = findToken(m, false)
			refresh = findToken(m, true)
		}
	}
	// 2) 解析 cookie 键值对
	pairs := parseCookiePairs(s)
	for k, v := range pairs {
		lk := strings.ToLower(k)
		if access == "" && isAccessKey(lk) {
			access = maybeJWTLift(v)
		}
		if refresh == "" && isRefreshKey(lk) {
			refresh = maybeJWTLift(v)
		}
	}
	// 3) 兜底：扫描所有值里的 JWT
	if access == "" || refresh == "" {
		for _, v := range pairs {
			t := maybeJWTLift(v)
			if !looksJWT(t) {
				continue
			}
			tk := tokenType(t)
			if tk == "refresh" && refresh == "" {
				refresh = t
			} else if access == "" && tk != "refresh" {
				access = t
			}
		}
	}
	// 4) 最后兜底：整串里直接找 JWT
	if access == "" || refresh == "" {
		for _, t := range findJWTs(s) {
			tk := tokenType(t)
			if tk == "refresh" && refresh == "" {
				refresh = t
			} else if access == "" && tk != "refresh" {
				access = t
			}
		}
	}
	return access, refresh
}

func parseCookiePairs(s string) map[string]string {
	out := map[string]string{}
	// 分号分隔，也支持换行
	repl := strings.NewReplacer("\r", ";", "\n", ";")
	for _, part := range strings.Split(repl.Replace(s), ";") {
		kv := strings.SplitN(strings.TrimSpace(part), "=", 2)
		if len(kv) != 2 {
			continue
		}
		k := strings.TrimSpace(kv[0])
		v := strings.Trim(strings.TrimSpace(kv[1]), `"`)
		if k != "" {
			out[k] = v
		}
	}
	return out
}

func isAccessKey(lk string) bool {
	for _, k := range []string{"accesstoken", "access_token", "token", "authorization", "auth_token"} {
		if lk == k {
			return true
		}
	}
	return false
}

func isRefreshKey(lk string) bool {
	for _, k := range []string{"refreshtoken", "refresh_token"} {
		if lk == k {
			return true
		}
	}
	return false
}

func findToken(m map[string]any, refresh bool) string {
	for k, v := range m {
		lk := strings.ToLower(k)
		ok := (refresh && isRefreshKey(lk)) || (!refresh && isAccessKey(lk))
		if !ok {
			continue
		}
		if s, ok := v.(string); ok {
			return maybeJWTLift(s)
		}
	}
	return ""
}

// maybeJWTLift 处理形如 "Bearer xxx" 或 URL 编码的值。
func maybeJWTLift(v string) string {
	v = strings.TrimSpace(v)
	v = strings.TrimPrefix(v, "Bearer ")
	v = strings.TrimPrefix(v, "bearer ")
	if dec, err := url.QueryUnescape(v); err == nil && strings.Count(dec, ".") == 2 {
		v = dec
	}
	return strings.Trim(v, `"`)
}

func looksJWT(s string) bool {
	return strings.Count(s, ".") == 2 && len(s) > 40
}

func findJWTs(s string) []string {
	var out []string
	for _, f := range strings.FieldsFunc(s, func(r rune) bool {
		return !(r == '.' || r == '-' || r == '_' || (r >= 'A' && r <= 'Z') || (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9'))
	}) {
		if looksJWT(f) {
			out = append(out, f)
		}
	}
	return out
}

// tokenType 解析 JWT payload 的 type 字段判断 access/refresh。
func tokenType(tok string) string {
	parts := strings.Split(tok, ".")
	if len(parts) != 3 {
		return ""
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return ""
	}
	var claims struct {
		Type string `json:"type"`
	}
	if json.Unmarshal(payload, &claims) != nil {
		return ""
	}
	return strings.ToLower(claims.Type)
}

func jwtSub(tok string) string {
	parts := strings.Split(tok, ".")
	if len(parts) != 3 {
		return ""
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return ""
	}
	var claims struct {
		Sub string `json:"sub"`
		UID string `json:"uid"`
	}
	if json.Unmarshal(payload, &claims) != nil {
		return ""
	}
	if claims.Sub != "" {
		return claims.Sub
	}
	return claims.UID
}

// schedulerRun 运行调度器直到 ctx 取消。
func schedulerRun(ctx context.Context, a *App) {
	s := scheduler.New(*a.sched)
	s.Run(ctx)
}
