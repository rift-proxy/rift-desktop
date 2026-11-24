# Сборка из исходников

Команды выполняются из корня рядом с `Cargo.toml`, `desktop/` и `tooling/`. Корневого `package.json` нет, используйте `pnpm --dir desktop`.

## Linux

Нужны Node.js 24, Git, Docker и сеть.

```sh
node tooling/docker-build.mjs
```

Docker собирает **Windows-установщик NSIS**:

```text
dist/windows-x64/bundle/nsis/Rift_<версия>_x64-setup.exe
```

## Windows

Установите Git, Node.js 24, pnpm 10.34.5, Rust stable с MSVC target, Visual Studio Build Tools с C++ workload и Windows SDK. Интерфейсу нужен WebView2 Evergreen Runtime. В Docker закреплён Rust 1.98.1.

```powershell
pnpm --dir desktop install --frozen-lockfile
node tooling/sidecar.mjs windows
pnpm --dir desktop tauri dev
```

Скрипт загрузки проверяет SHA-256 архивов и бинарников sing-box 1.14.1 и Xray 26.10.21, включая уже скачанные файлы. Версии и hashes закреплены в `tooling/sidecar.mjs`.

Сборка установщика:

```powershell
node tooling/tauri-build.mjs -- --locked
```

Установщик: `target/release/bundle/nsis/Rift_<версия>_x64-setup.exe`. Отдельного `rift-desktop.exe` для работы недостаточно.
