# -*- coding: utf-8 -*-
# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller 打包配置：单文件、无控制台窗口图标依赖"""

block_cipher = None

import os

HERE = os.path.abspath(os.path.dirname(__file__) if '__file__' in dir()
                        else os.getcwd())
CLI_PROXY_EXE = os.path.join(HERE, '..', 'cli-proxy', 'cli-proxy-api.exe')

# 内置 CLIProxyAPI（71 MB Go 单二进制）。
# 收进 datas 后打包时会被压进单 EXE；运行时由 app/localproxy.ensure_binary()
# 释放到 data/cliproxy/bin/ 并用 size+stamp 记账，避免每次启动重解压 71 MB。
# 想做「轻量版」就把 include_cliproxy 改成 False。
include_cliproxy = os.path.exists(CLI_PROXY_EXE)

datas = [
    ('app/static/index.html', 'app/static'),
    ('app/static/style.css', 'app/static'),
    ('app/static/app.js', 'app/static'),
    ('app/static/aigw.ico', 'app/static'),
]
if include_cliproxy:
    datas.append((CLI_PROXY_EXE, 'cliproxy'))
    print('[build.spec] 内置 cli-proxy-api.exe（%.1f MB）'
          % (os.path.getsize(CLI_PROXY_EXE) / 1048576))
else:
    print('[build.spec] 未找到 cli-proxy-api.exe，打包为轻量版（不含本地上游）')

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=datas,
    # 新模块多在函数内动态导入，显式声明避免 PyInstaller 静态分析漏掉
    hiddenimports=[
        'app.gwlogin', 'app.accounts', 'app.gwextra',
        'app.cdp', 'app.browser_cookie', 'app.aesgcm',
        'app.catalog', 'app.upstreams', 'app.router',
        'app.store', 'app.gwclient', 'app.sitecheck',
        'app.notify', 'app.scheduler', 'app.tray',
        'app.bundled_models',   # 内置模型倍率表
        'app.tools',          # 工具调用（function calling）执行器
        'app.autocheckin',    # APP 平台定时自动签到
        'app.localproxy',     # 本机反代上游（CLIProxyAPI）管理
        'app.bundled_yuanbao', # 元宝内置模型清单（从 APK 提取）
        'app.sources',       # 接入源分类（本地AI / 平台API / 网页对话）
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
    strip=False,
    upx=False,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon='app/static/aigw.ico',
    version=None,
)
