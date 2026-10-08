# -*- coding: utf-8 -*-
# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller 打包配置：单文件、跨平台（Windows/Linux/macOS）"""

block_cipher = None

import os
import sys

HERE = os.path.abspath(os.path.dirname(__file__) if '__file__' in dir()
                        else os.getcwd())

# 检测当前平台
IS_WINDOWS = sys.platform == 'win32'
IS_MACOS = sys.platform == 'darwin'
IS_LINUX = sys.platform.startswith('linux')

# CLIProxyAPI 二进制（按平台选择）
if IS_WINDOWS:
    CLI_PROXY_BIN = os.path.join(HERE, '..', 'cli-proxy', 'cli-proxy-api.exe')
elif IS_MACOS:
    CLI_PROXY_BIN = os.path.join(HERE, '..', 'cli-proxy', 'cli-proxy-api-darwin')
else:
    CLI_PROXY_BIN = os.path.join(HERE, '..', 'cli-proxy', 'cli-proxy-api-linux')

# 内置 CLIProxyAPI（71 MB Go 单二进制）。
# 收进 datas 后打包时会被压进单 EXE；运行时由 app/localproxy.ensure_binary()
# 释放到 data/cliproxy/bin/ 并用 size+stamp 记账，避免每次启动重解压 71 MB。
# 想做「轻量版」就把 include_cliproxy 改成 False。
include_cliproxy = os.path.exists(CLI_PROXY_BIN)

datas = [
    ('app/static/index.html', 'app/static'),
    ('app/static/style.css', 'app/static'),
    ('app/static/app.js', 'app/static'),
    ('app/static/vendor/qrcode.min.js', 'app/static/vendor'),
    ('app/static/aigw.ico', 'app/static'),
    # web-deepseek 的 POW 求解器：Node worker + sha3 wasm（app/web_relays.py
    # 会释放到 _MEIPASS 供 node 读取；缺任一 POW 无法通过 DeepSeek 校验）
    ('app/webpow/pow_worker.js', 'app/webpow'),
    ('app/webpow/wasm/sha3_wasm_bg.wasm', 'app/webpow/wasm'),
]
if include_cliproxy:
    datas.append((CLI_PROXY_BIN, 'cliproxy'))
    print('[build.spec] 内置 cli-proxy（%.1f MB）'
          % (os.path.getsize(CLI_PROXY_BIN) / 1048576))
else:
    print('[build.spec] 未找到 cli-proxy 二进制，打包为轻量版（不含本地上游）')

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=datas,
    # 新模块多在函数内动态导入，显式声明避免 PyInstaller 静态分析漏掉
    hiddenimports=[
        'app.gwlogin', 'app.accounts', 'app.gwextra', 'app.web_relays',
        'app.native_relay', 'app.doubao_relay', 'app.acct_pool', 'app.tlogin',
        'app.cdp', 'app.browser_cookie', 'app.aesgcm',
        'app.catalog', 'app.upstreams', 'app.router',
        'app.store', 'app.gwclient', 'app.sitecheck',
        'app.notify', 'app.scheduler', 'app.tray',
        'app.bundled_models',   # 内置模型倍率表
        'app.tools',          # 工具调用（function calling）执行器
        'app.autocheckin',    # APP 平台定时自动签到
        'app.localproxy',     # 本机反代上游（CLIProxyAPI）管理
        'app.bundled_yuanbao', # 元宝内置模型清单（从 APK 提取）
        'app.bundled_doubao',  # 豆包内置模型清单（从网页版 model_list 解析）
        'app.bundled_trae',   # Trae aigw.app 网关模型清单（base.apk dex 提取）
        'app.bundled_go',     # Go 网关模型能力表（libgojni.so 内嵌 JSON 提取）
        'app.model_extract',  # 模型清单实时提取 + 内置回退
        'app.native_relay',  # 原生对话中继（端点静态提取自 APK/EXE）
        'app.doubao_relay',  # 豆包原生中继（samantha 协议 + 账号池轮转）
        'app.sources',       # 接入源分类（本地AI / 平台API / 网页对话）
        'app.tencent',       # 腾讯 CodeBuddy 直连（移植自 workbuddy-gateway 规格）
        'app.tlogin',        # 原生登录链路（抓包实测：enterprise 端点发 token）
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        'tkinter', 'matplotlib', 'numpy', 'PIL', 'PyQt5', 'PySide2',
        'pytest', 'IPython', 'notebook',
        # 注意：sqlite3 **不能**排除 —— 读取本机浏览器 Cookie 依赖它
    ],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

# Windows 用 GUI 模式（无控制台窗口），Linux/macOS 用控制台模式
console_mode = not IS_WINDOWS

# 图标只在 Windows 上使用
icon_file = 'app/static/aigw.ico' if IS_WINDOWS else None

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name='aigw-panel',
    debug=False,
    bootloader_ignore_signals=False,
    strip=IS_LINUX,  # Linux 上 strip 减小体积
    upx=False,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=console_mode,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=icon_file,
    version=None,
)
