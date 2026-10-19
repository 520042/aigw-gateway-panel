; aigw-panel-rs 安装包（对齐 dsh-our-free-model 的 setup.exe 形态）
;
; 说明：本项目的 Rust 二进制已内置 --install 自检安装（复制 exe+静态资源、
;       建快捷方式 + 写 HKCU 自启、结束后启动面板）。此 NSIS 脚本作为
;       传统安装器备选，提供更标准的「下一步」向导体验。
;
; 用法：makensis aigw-panel.nsi   （需 NSIS 3.x，且同目录有 aigw-panel.exe / app / webpow）

!define APPNAME "AI 资源网关面板"
!define EXE "aigw-panel.exe"
!define INSTDIR "$LOCALAPPDATA\aigw-panel-rs"

Unicode true
RequestExecutionLevel user
InstallDir "${INSTDIR}"
Name "${APPNAME}"
OutFile "AI.资源网关面板_setup.exe"
Icon "app\static\aigw.ico"

Section "Install"
  SetOutPath "$INSTDIR"
  File "${EXE}"
  File /r "app"
  File /r "webpow"

  ; 开始菜单
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\app\static\aigw.ico"
  ; 桌面
  CreateShortCut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\app\static\aigw.ico"
  ; 开机自启（指向安装位 --tray）
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "AigwPanel" '"$INSTDIR\${EXE}" --tray'

  ; 启动面板
  Exec '"$INSTDIR\${EXE}"'
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  Delete "$DESKTOP\${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "AigwPanel"
  ; 保留数据目录（data\）不删；如需彻底清理用 Rust 的 --purge
  Delete "$INSTDIR\${EXE}"
  RMDir /r "$INSTDIR\app"
  RMDir /r "$INSTDIR\webpow"
  RMDir "$INSTDIR"
SectionEnd
