# Участие в разработке

## Ветки и PR

1. Создайте fork и создайте ветку от `dev`: `feature/краткое-описание`, `fix/краткое-описание` или `chore/краткое-описание`. Пишите описание латиницей в нижнем регистре через дефис, например `fix/update-dialog`.
2. Делайте одно связное изменение. Большую идею сначала обсудите в issue или через [форму](https://tally.so/r/per2zq).
3. Добавьте regression test для исправления ошибки, обновите документацию и проверьте затронутый слой. Версию приложения не повышайте. CHANGELOG и версию перед выпуском готовят позже.
4. Откройте PR в `dev`, до готовности к review оставьте draft. Опишите проблему, ожидаемое поведение, изменение и результаты проверок.

### Размер PR и изменение бюджетов

Ориентир: до 1000 добавленных и удалённых строк суммарно и до 15 изменённых файлов, включая бинарные. Это рекомендация, не запрет.

Изменение, удаление или переименование `tooling/perf/budgets.json` получает метку `budget-review` и сообщение с просьбой объяснить причину и приложить измерения. Это касается и release PR. При откате изменения бот снимает только эту метку. Она не является Git tag или одобрением повышения бюджета; решение принимает владелец, автоматизация merge не блокирует.

## Границы кода

- `core/`: Rust, сеть, хранение, выбор сервера, таймеры и расписания.
- `desktop/src-tauri/`: тонкие Tauri handlers и платформенная интеграция.
- `desktop/src/`: интерфейс и действия пользователя, без переноса сюда сети, сохранения, расписаний или произвольного shell/filesystem-доступа.
- Не добавляйте новый `unsafe`. Новый Win32 FFI сначала обсудите и проверьте на безопасность.

Все команды ниже выполняются из корня репозитория рядом с `Cargo.toml`, `desktop/` и `tooling/`.

Тексты интерфейса, Rust и установщика хранятся в `desktop/src/locales/<язык>.yml`; в коде используйте ключи. Новый язык требует полного каталога с теми же ключами и блоком `language`. После изменения:

```sh
node tooling/locales-sync.mjs write
node tooling/locales-sync.mjs check
```

## Проверки по изменению

Запустите проверки затронутого слоя. Перед integration-проверками ядра скачайте xray и sing-box для текущей ОС (Windows или Linux): `node tooling/sidecar.mjs`.

```sh
# Rust: имя integration-теста
cargo test --locked -p rift-core --test selection
cargo clippy --locked -p rift-core --all-targets -- -D warnings
cargo fmt --all -- --check

# UI: unit-тесты, типы, lint и сборка
pnpm --dir desktop typecheck
pnpm --dir desktop lint
pnpm --dir desktop format:check
pnpm --dir desktop test
pnpm --dir desktop build
```

Для интерактивного просмотра: `pnpm --dir desktop storybook`. Для browser/E2E-тестов и тестов Storybook установите Chromium: `pnpm --dir desktop exec playwright install chromium`, на Linux runner также системные зависимости: `pnpm --dir desktop exec playwright install --with-deps chromium`. Обычный Vite требует Tauri bridge; отдельно интерфейс смотрите в Storybook с тестовым backend.

