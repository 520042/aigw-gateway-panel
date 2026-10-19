// main.go gui 版入口：单 exe，内置 Web 管理界面。
//
// 启动后：
//   - 首次运行生成 config.json（已内置上游地址）
//   - 自动打开浏览器到本地管理页面
//   - 可在页面里登录账号 / 导入 Cookie / 查看额度 / 启停 OpenAI 兼容代理
package main

import (
	"context"
	"embed"
	"encoding/json"
	"flag"
	"fmt"
	"io/fs"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"time"

	"lobsterai2api/internal/pool"
	"lobsterai2api/internal/scheduler"
	"lobsterai2api/internal/upstream"
)

//go:embed web/*
var webFS embed.FS

const (
	defaultUpstreamBase = "https://lobsterai-server.youdao.com"
	defaultLoginPortal  = "https://lobsterai.youdao.com"
	defaultAPIKey       = "sk-lobster-local"
	defaultProxyListen  = ":8367"
	defaultGUIPort      = 8370
)

// App 全局运行时状态。
type App struct {
	cfg          *Config
	root         string
	pool         *pool.Pool
	up           *upstream.Client
	sched        *scheduler.Config
	proxySrv     *http.Server
	proxyRun     bool
	schedCancel  context.CancelFunc
	loginStateMu sync.Mutex
	loginState   *loginSession
}

func main() {
	initDefaultEnv()

	dataDir := flag.String("data", "", "data directory (default: ./lobsterai2api-data)")
	noBrowser := flag.Bool("no-browser", false, "do not open browser automatically")
	flag.Parse()

	root := *dataDir
	if root == "" {
		root = defaultDataDir()
	}
	if err := os.MkdirAll(root, 0o755); err != nil {
		fatal("mkdir data dir: %v", err)
	}

	cfg, err := LoadConfig(root)
	if err != nil {
		fatal("load config: %v", err)
	}

	app := newApp(cfg, root)

	guiListen := fmt.Sprintf("127.0.0.1:%d", defaultGUIPort)
	target := fmt.Sprintf("http://%s/", guiListen)
	if !*noBrowser {
		go func() {
			time.Sleep(800 * time.Millisecond)
			openBrowser(target)
		}()
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	// 启动后延迟刷新一次额度
	go func() {
		time.Sleep(3 * time.Second)
		app.refreshAllCredits()
	}()

	srv := &http.Server{Addr: guiListen, Handler: app.routes(), ReadHeaderTimeout: 30 * time.Second}
	go func() {
		<-ctx.Done()
		shutCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutCtx)
		app.stopProxy()
	}()

	log.Printf("LobsterAI2API 控制台: %s", target)
	log.Printf("数据目录: %s", root)
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		fatal("gui http: %v", err)
	}
	log.Printf("bye")
}

func newApp(cfg *Config, root string) *App {
	app := &App{cfg: cfg, root: root}
	cfg.AuthDir = filepath.Join(root, "auths")
	auths, _ := loadAuths(filepath.Join(root, "auths"))
	app.pool = pool.New(filepath.Join(root, "data", "state.json"))
	for _, a := range auths {
		app.pool.Add(a)
	}
	app.up = upstream.New()
	app.up.HTTP.Timeout = time.Duration(cfg.Upstream.TimeoutSeconds) * time.Second
	app.sched = &scheduler.Config{
		Pool:           app.pool,
		Upstream:       app.up,
		CheckinHours:   cfg.Schedule.CheckinHours,
		KeepaliveHours: cfg.Schedule.KeepaliveHours,
	}
	return app
}

func (a *App) routes() http.Handler {
	mux := http.NewServeMux()

	sub, _ := fs.Sub(webFS, "web")
	mux.Handle("/", http.FileServer(http.FS(sub)))

	mux.HandleFunc("/api/state", a.handleState)
	mux.HandleFunc("/api/accounts/refresh", a.handleRefreshAccounts)
	mux.HandleFunc("/api/accounts/delete", a.handleDeleteAccount)
	mux.HandleFunc("/api/login/start", a.handleLoginStart)
	mux.HandleFunc("/api/login/status", a.handleLoginStatus)
	mux.HandleFunc("/api/cookie/import", a.handleCookieImport)
	mux.HandleFunc("/api/client/import", a.handleClientImport)
	mux.HandleFunc("/api/proxy/start", a.handleProxyStart)
	mux.HandleFunc("/api/proxy/stop", a.handleProxyStop)
	mux.HandleFunc("/api/config", a.handleConfig)
	return mux
}

func defaultDataDir() string {
	exe, err := os.Executable()
	if err != nil {
		return "lobsterai2api-data"
	}
	return filepath.Join(filepath.Dir(exe), "lobsterai2api-data")
}

// initDefaultEnv 内置默认上游/门户/密钥，避免用户配环境变量。
func initDefaultEnv() {
	if os.Getenv("LB2A_UPSTREAM_BASE") == "" {
		os.Setenv("LB2A_UPSTREAM_BASE", defaultUpstreamBase)
	}
	if os.Getenv("LB2A_LOGIN_PORTAL") == "" {
		os.Setenv("LB2A_LOGIN_PORTAL", defaultLoginPortal)
	}
}

func openBrowser(url string) {
	var cmd *exec.Cmd
	switch runtime.GOOS {
	case "windows":
		cmd = exec.Command("rundll32", "url.dll,FileProtocolHandler", url)
	case "darwin":
		cmd = exec.Command("open", url)
	default:
		cmd = exec.Command("xdg-open", url)
	}
	_ = cmd.Start()
}

func fatal(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "gui: "+format+"\n", args...)
	os.Exit(1)
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	_ = json.NewEncoder(w).Encode(v)
}

func freePort() int {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0
	}
	defer ln.Close()
	return ln.Addr().(*net.TCPAddr).Port
}

func trim(s string) string { return strings.TrimSpace(s) }
