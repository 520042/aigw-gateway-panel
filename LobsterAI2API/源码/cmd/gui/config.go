// config.go gui 版配置：首次运行生成 config.json，含内置上游地址与本地密钥。
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Config 管理界面 + 代理运行配置。
type Config struct {
	Listen    string `json:"listen"`
	APIKey    string `json:"api_key"`
	AuthDir   string `json:"auth_dir"`
	StateFile string `json:"state_file"`

	UpstreamBase string `json:"upstream_base"`
	LoginPortal  string `json:"login_portal"`

	Cooldown struct {
		HardCredit  string `json:"hard_credit"`
		SoftRate    string `json:"soft_rate"`
		ErrThresh   int    `json:"err_threshold"`
		ErrCooldown string `json:"err_cooldown"`
	} `json:"cooldown"`

	Schedule struct {
		CheckinHours   []int `json:"checkin_hours"`
		KeepaliveHours []int `json:"keepalive_hours"`
	} `json:"schedule"`

	Upstream struct {
		TimeoutSeconds int `json:"timeout_seconds"`
	} `json:"upstream"`

	HardCreditDur  time.Duration `json:"-"`
	SoftRateDur    time.Duration `json:"-"`
	ErrCooldownDur time.Duration `json:"-"`
}

// LoadConfig 读取 root/config.json，不存在则写默认文件。
func LoadConfig(root string) (*Config, error) {
	path := filepath.Join(root, "config.json")
	cfg := defaultConfig()

	raw, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			if werr := saveConfig(path, cfg); werr != nil {
				return nil, werr
			}
			if nerr := normalizeConfig(cfg); nerr != nil {
				return nil, nerr
			}
			return cfg, nil
		}
		return nil, err
	}
	if err := json.Unmarshal(raw, cfg); err != nil {
		return nil, fmt.Errorf("parse config: %w", err)
	}
	// 补全缺失字段
	if cfg.UpstreamBase == "" {
		cfg.UpstreamBase = defaultUpstreamBase
	}
	if cfg.LoginPortal == "" {
		cfg.LoginPortal = defaultLoginPortal
	}
	if cfg.Listen == "" {
		cfg.Listen = defaultProxyListen
	}
	if cfg.APIKey == "" {
		cfg.APIKey = defaultAPIKey
	}
	if err := normalizeConfig(cfg); err != nil {
		return nil, err
	}
	return cfg, nil
}

func defaultConfig() *Config {
	c := &Config{
		Listen:       defaultProxyListen,
		APIKey:       defaultAPIKey,
		UpstreamBase: defaultUpstreamBase,
		LoginPortal:  defaultLoginPortal,
	}
	c.Cooldown.HardCredit = "12h"
	c.Cooldown.SoftRate = "60s"
	c.Cooldown.ErrThresh = 3
	c.Cooldown.ErrCooldown = "10m"
	c.Schedule.CheckinHours = []int{9, 21}
	c.Schedule.KeepaliveHours = []int{22}
	c.Upstream.TimeoutSeconds = 180
	return c
}

func normalizeConfig(c *Config) error {
	var err error
	if c.HardCreditDur, err = time.ParseDuration(c.Cooldown.HardCredit); err != nil {
		return fmt.Errorf("cooldown.hard_credit: %w", err)
	}
	if c.SoftRateDur, err = time.ParseDuration(c.Cooldown.SoftRate); err != nil {
		return fmt.Errorf("cooldown.soft_rate: %w", err)
	}
	if c.ErrCooldownDur, err = time.ParseDuration(c.Cooldown.ErrCooldown); err != nil {
		return fmt.Errorf("cooldown.err_cooldown: %w", err)
	}
	if c.Cooldown.ErrThresh <= 0 {
		c.Cooldown.ErrThresh = 3
	}
	if c.Upstream.TimeoutSeconds <= 0 {
		c.Upstream.TimeoutSeconds = 180
	}
	os.Setenv("LB2A_UPSTREAM_BASE", c.UpstreamBase)
	os.Setenv("LB2A_LOGIN_PORTAL", c.LoginPortal)
	return nil
}

func saveConfig(path string, cfg *Config) error {
	raw, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, raw, 0o600)
}
