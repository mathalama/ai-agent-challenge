# Агент проверки требований

Rust CLI для статической проверки требований к финтех-проектам. Агент выделяет требования, анализирует их с Gemini, сверяет цитаты и формирует отчёт на русском. Поддерживаются Markdown и обычный текст в UTF‑8. [Задание хакатона](https://github.com/BCC-HUB/ai-requirements-validator-task)

## Запуск

Установите стабильный Rust с поддержкой Edition 2024, настройте Gemini по инструкции ниже и запустите CLI из корня репозитория. [Установка Rust](https://www.rust-lang.org/tools/install)

```sh
cargo run -p cli -- check samples/requirements.md -o reports/report.md
```

Команда отправляет разобранный текст в Gemini и создаёт `reports/report.md`. Для локального демонстрационного прогона без ключа добавьте `--provider mock`. Файл `samples/requirements.md` взят из репозитория задания. [Исходный документ организаторов](https://github.com/BCC-HUB/ai-requirements-validator-task/blob/main/docs/requirements.md)

```text
tz-agent check <INPUT> [-o <OUTPUT>] [--format md|json] [--provider gemini|mock] [--no-cache] [--trace <PATH>]
tz-agent ask <INPUT>
tz-agent eval [--golden eval/]
```

По умолчанию CLI использует `gemini` и записывает отчёт в `reports/report.md` или `reports/report.json` в зависимости от формата. `--trace <PATH>` пишет JSON с порядком и длительностью выполнения стадий, а также расходом токенов по вызовам LLM. `--no-cache` отключает чтение и запись кэша `.cache/llm/`. Команды `ask` и `eval` находятся в разработке.

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
| `cli` | Конфигурация, команды `tz-agent`, чтение и запись файлов |

[Архитектура](docs/architecture.md) · [Решения](docs/decisions.md)

## Проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
