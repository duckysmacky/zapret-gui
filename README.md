# zapret-gui

> **zapret-gui — неофициальная программа.** Это не проект zapret и не проект Flowseal. Об ошибках программы
> сообщайте в этот репозиторий, а не авторам zapret.

Простая программа для Windows, которая помогает открыть Discord и YouTube с помощью
[zapret-discord-youtube](https://github.com/Flowseal/zapret-discord-youtube) (Flowseal) без `.bat`-файлов
и командной строки:

- сама скачивает последнюю версию zapret с GitHub и обновляет её;
- сама подбирает рабочую стратегию (быстрый и полный подбор);
- включается одной кнопкой, живёт в трее, может запускаться вместе с Windows;
- следит, чтобы обход продолжал работать («сторож»), и сообщает о сбоях;
- умеет всё, что умеет `service.bat`: игровой режим, IPSet, свои списки, фейки, диагностика,
  удаление старых служб, обновление hosts, очистка кэша Discord, тесты стратегий.

## Установка

1. **Удалите старые установки zapret** и других обходов (GoodbyeDPI и т.п.), особенно если ставили их как
   службу (`service.bat` → *Remove Services*). Иначе обход не запустится.
2. Скачайте `zapret-gui-setup-<версия>.exe` со страницы
   [Releases](../../releases) и запустите.
3. При первом запуске мастер скачает zapret, подберёт стратегию и предложит автозапуск.

### Предупреждения Windows

- **SmartScreen** («Система Windows защитила ваш компьютер»): установщик пока не подписан. Нажмите
  «Подробнее» → «Выполнить в любом случае». SHA256 установщика указан на странице релиза.
- **Антивирус / Defender** может ругаться на `winws.exe` или драйвер WinDivert из zapret: это инструменты,
  перехватывающие сетевой трафик, поэтому антивирусы часто считают их подозрительными. Если файл удалён,
  «Диагностика» покажет проблему с `WinDivert64.sys` и предложит скачать версию заново.
- Программе нужны **права администратора** (драйвер WinDivert работает только так). При ручном запуске
  Windows спросит разрешение; при автозапуске — нет.

## Удаление

«Параметры Windows» → «Приложения» → zapret-gui → «Удалить». Удаляются программа, скачанные версии zapret,
задача автозапуска и служба WinDivert. Настройки удаляются по желанию.

## Благодарности

- [Flowseal/zapret-discord-youtube](https://github.com/Flowseal/zapret-discord-youtube) — стратегии и списки (MIT)
- [bol-van/zapret](https://github.com/bol-van/zapret) — движок winws (MIT)
- [WinDivert](https://github.com/basil00/WinDivert) — драйвер перехвата пакетов (LGPL-3.0)
- [hyperion-cs/dpi-checkers](https://github.com/hyperion-cs/dpi-checkers) — набор DPI-проверок (Apache-2.0)
- [Slint](https://slint.dev) — интерфейс

Лицензия: MIT.

---

## For developers

Rust workspace: `core` (all logic, platform seams, mocks) and `ui` (Slint app). On Linux the app runs with a
mock platform (simulated winws, prober, services).

```sh
cargo run -p zapret-ui                         # Linux: mock platform
ZAPRET_GUI_MOCK_SCENARIO=winws_exits cargo run -p zapret-ui   # also: nothing_works, download_fails, not_elevated
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Design docs live in `.context/` (see `AGENTS.md`). Windows release builds and the installer come from CI
(`.github/workflows/release.yml`, on tags `v*`).
