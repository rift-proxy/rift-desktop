; Rift NSIS hooks (tauri.conf.json: bundle.windows.nsis.installerHooks).
;
; Tauri's own WebView2 section skips the install whenever the EdgeUpdate `pv` value is merely
; non-empty. Microsoft counts the runtime as installed only when `pv` is above 0.0.0.0 in HKLM
; or HKCU: a missing, empty or 0.0.0.0 value means it is not installed (see "Detect if a
; WebView2 Runtime is already installed" in the WebView2 distribution docs). A leftover
; 0.0.0.0 would therefore make the installer skip the download.
; Not covered: a valid `pv` whose version folder does not exist (seen in Windows Sandbox,
; where the Microsoft bootstrapper fails too); only the registry is read here.
; NSIS_HOOK_PREINSTALL runs after that section, so it repeats the check with Microsoft's rule
; and downloads the Evergreen bootstrapper only if no usable runtime is registered.

; Sets ${_OUT} to 1 when HKLM or HKCU holds a usable `pv`, else 0. Clobbers nothing else;
; ${_OUT} must not be $0.
!macro RIFT_WEBVIEW2_USABLE _OUT
  Push $0
  StrCpy ${_OUT} 0
  ${If} ${RunningX64}
    ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${Else}
    ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    StrCpy ${_OUT} 1
  ${EndIf}
  ReadRegStr $0 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    StrCpy ${_OUT} 1
  ${EndIf}
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; An update runs inside an installed Rift, so its runtime is present (as in Tauri's section).
  ${If} $UpdateMode <> 1
    Push $0
    Push $1
    !insertmacro RIFT_WEBVIEW2_USABLE $1
    ${If} $1 = 0
      Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      DetailPrint "$(webview2Downloading)"
      NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
      Pop $0
      ${If} $0 == "success"
        DetailPrint "$(webview2DownloadSuccess)"
      ${Else}
        DetailPrint "$(webview2DownloadError)"
        Abort "$(webview2AbortError)"
      ${EndIf}
      DetailPrint "$(installingWebview2)"
      ExecWait '"$TEMP\MicrosoftEdgeWebview2Setup.exe" /silent /install' $0
      ${If} $0 = 0
        DetailPrint "$(webview2InstallSuccess)"
      ${Else}
        DetailPrint "$(webview2InstallError)"
        Abort "$(webview2AbortError)"
      ${EndIf}
    ${EndIf}
    Pop $1
    Pop $0
  ${EndIf}
!macroend

; rift:// links (a browser, a QR scanner app, a provider's page) open Rift: the add-subscription
; and add-server dialogs are shown with the link, nothing is added without a confirmation.
; The scheme belongs to the current user (installMode: currentUser, so SHCTX is HKCU) and, like
; the rest of the installation, needs no administrator rights. Rift itself checks the link
; (`rift://add?url=…` or `rift://add?uri=…` only), whatever the system passes in "%1".
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr SHCTX "Software\Classes\rift" "" "URL:Rift"
  WriteRegStr SHCTX "Software\Classes\rift" "URL Protocol" ""
  WriteRegStr SHCTX "Software\Classes\rift\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
  WriteRegStr SHCTX "Software\Classes\rift\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
!macroend

; The scheme goes with the program; the data folder is not touched here.
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey SHCTX "Software\Classes\rift"
!macroend
