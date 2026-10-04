# -*- coding: utf-8 -*-
# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller 打包配置：单文件、无控制台窗口图标依赖"""

block_cipher = None

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=[
        ('app/static/index.html', 'app/static'),
        ('app/static/style.css', 'app/static'),
        ('app/static/app.js', 'app/static'),
        ('app/static/aigw.ico', 'app/static'),
    ],
    # 新模块多在函数内动态导入，显式声明避免 PyInstaller 静态分析漏掉
    hiddenimports=[
        'app.gwlogin', 'app.accounts', 'app.gwextra',
        'app.cdp', 'app.browser_cookie', 'app.aesgcm',
        'app.catalog', 'app.upstreams', 'app.router',
        'app.store', 'app.gwclient', 'app.sitecheck',
        'app.notify', 'app.scheduler', 'app.tray',
        'app.bundled_models',   # 内置模型倍率表
        'app.tools',          # 工具调用（function calling）执行器
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
