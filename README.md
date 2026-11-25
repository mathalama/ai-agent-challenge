# Агент проверки требований

Rust CLI для статической проверки требований к финтех-проектам. Агент выделяет требования, анализирует их с Gemini, сверяет цитаты и формирует отчёт на русском. Поддерживаются Markdown, обычный текст (UTF‑8), PDF и DOCX. [Задание хакатона](https://github.com/BCC-HUB/ai-requirements-validator-task)

## Запуск

Установите стабильный Rust с поддержкой Edition 2024 и настройте Gemini по инструкции ниже. [Установка Rust](https://www.rust-lang.org/tools/install)

Из корня репозитория запустите через Cargo.

```text
cargo run -p cli -- check <INPUT> -o <OUTPUT>
```

Чтобы установить команду `rs`, выполните из корня репозитория.

```sh
cargo install --path crates/cli
```

После установки запустите проверку командой.

```text
rs check <INPUT> -o <OUTPUT>
```

Подставьте путь к документу требований вместо `<INPUT>` и путь к отчёту вместо `<OUTPUT>`. Для локального демонстрационного прогона без ключа используйте команду.

```sh
cargo run -p cli -- check samples/requirements.md -o reports/report.md --provider mock
```

Команда создаёт `reports/report.md`. Файл `samples/requirements.md` взят из репозитория задания. [Исходный документ организаторов](https://github.com/BCC-HUB/ai-requirements-validator-task/blob/main/docs/requirements.md)

```text
rs check <INPUT> [-o <OUTPUT>] [--format md|json] [--provider gemini|mock] [--no-cache] [--trace <PATH>]
rs ask <INPUT> [-o <PATH>] [--provider gemini|mock] [--no-cache]
rs eval [--golden eval/] [--provider gemini|mock] [--no-cache]
```

По умолчанию CLI отправляет разобранный текст документа в Gemini и записывает отчёт в `reports/report.md` или `reports/report.json` в зависимости от формата. `--trace <PATH>` пишет JSON с порядком и длительностью выполнения стадий, а также расходом токенов по вызовам LLM. `--no-cache` отключает чтение и запись кэша `.cache/llm/`. Команда `eval` рассчитывает precision, recall и список пропусков по эталонным разметкам в `eval/` (формат описан в [eval/README.md](eval/README.md)).

`rs ask` показывает открытые вопросы по одному. Можно выбрать вариант командой `#1`, написать свой ответ или нажать Enter для пропуска. Если все варианты — целые числа, ввод `15000` сохраняет это число как ответ. При текстовых вариантах число без префикса выбирает вариант, а `=15000` сохраняет собственный числовой ответ. Ответы сразу сохраняются по ID вопроса в `reports/report.answers.json`. Для собственного пути отчёта передайте одинаковый `-o` в обе команды. Например, для `reports/review.md` файл ответов — `reports/review.answers.json`. Повторный `rs check` показывает текст ответов под связанными находками. Исправления в документе проверяются отдельно при каждом прогоне. Если сохранённый ответ больше не совпадает с вопросом, CLI предупреждает об этом и сохраняет ответ в JSON-файле и в отчёте.

## Настройка Gemini

1. Войдите в Google AI Studio и создайте ключ в разделе API Keys. Выберите проект Google Cloud, если Studio запросит проект. [Google AI Studio](https://aistudio.google.com/apikey) · [Инструкция Google](https://ai.google.dev/gemini-api/docs/api-key)
2. Скопируйте `.env.example` в `.env` в корне проекта и запишите ключ в `GEMINI_API_KEY`. По умолчанию используется `gemini-3.1-flash-lite`. Чтобы выбрать другую модель, задайте её в `GEMINI_MODEL`. [Каталог моделей Google](https://ai.google.dev/gemini-api/docs/models)

`LLM_REQUESTS_PER_MINUTE` задаёт локальный темп запросов к Gemini и по умолчанию равен 5. Сверьте значение с активной квотой проекта и модели. При HTTP 429 агент учитывает задержку из ответа Gemini и ждёт 60 секунд, если сервер её не указал. `RUST_LOG` управляет уровнем логов и по умолчанию равен `warn`. [Лимиты Gemini API](https://ai.google.dev/gemini-api/docs/rate-limits) · [Активные квоты в AI Studio](https://aistudio.google.com/rate-limit)

## Конвейер

```text
segment → facts → rules → requirement_checks → set_checks
        → consistency → ambiguity → verify → clarify
```

| Крейт | Назначение |
| --- | --- |
| `doc` | Модель документа, координаты блоков, реестр парсеров |
| `llm` | Контракт провайдера, mock, HTTP-клиент Gemini, кэш, повторы, лимит и трасса |
| `review` | Доменные типы, стадии и проверки, сводка и форматы отчёта |
| `cli` | Конфигурация, команды `rs`, чтение и запись файлов |

[Архитектура](docs/architecture.md) · [Решения](docs/decisions.md)

## Проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
