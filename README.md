# Агент проверки требований

Локальный Rust CLI для статической проверки требований к финтех-проектам. В каркасе mock-прогон проходит девять стадий, переносит один пример требования из документа и формирует отчёт на русском. Поддерживаются Markdown и обычный текст в UTF‑8. [Задание хакатона](https://github.com/BCC-HUB/ai-requirements-validator-task)

## Запуск

Установите стабильный [Rust](https://www.rust-lang.org/tools/install) с поддержкой Edition 2024 и запустите CLI из корня репозитория.

```sh
cargo run -p cli -- check samples/requirements.md -o reports/report.md --provider mock
```

Команда создаёт `reports/report.md`. Mock работает локально и не требует ключа. Файл `samples/requirements.md` взят из репозитория задания. [Исходный документ организаторов](https://github.com/BCC-HUB/ai-requirements-validator-task/blob/main/docs/requirements.md)

```text
tz-agent check <INPUT> [-o <OUTPUT>] [--format md|json] [--provider gemini|mock] [--no-cache] [--trace <PATH>]
tz-agent ask <INPUT>
tz-agent eval [--golden eval/]
```

По умолчанию CLI использует `mock` и записывает отчёт в `reports/report.md` или `reports/report.json` в зависимости от формата. `--trace <PATH>` пишет JSON с порядком и длительностью выполнения стадий, а также расходом токенов по вызовам LLM. `--no-cache` отключает чтение и запись кэша `.cache/llm/`. Команды `ask` и `eval` пока возвращают сообщение о том, что они в разработке.

## Настройка Gemini

1. Войдите в Google AI Studio и создайте ключ в разделе API Keys. Выберите проект Google Cloud, если Studio запросит проект. [Google AI Studio](https://aistudio.google.com/apikey) · [Инструкция Google](https://ai.google.dev/gemini-api/docs/api-key)
2. Скопируйте `.env.example` в `.env` в корне проекта. Запишите ключ в `GEMINI_API_KEY`, а идентификатор выбранной модели в `GEMINI_MODEL`. [Каталог моделей Google](https://ai.google.dev/gemini-api/docs/models)

Флаг `--provider gemini` проверяет конфигурацию. HTTP-клиент Gemini находится в списке следующих задач. Для прогона каркаса используйте `--provider mock`.

`LLM_REQUESTS_PER_MINUTE` задаёт темп запросов на один локальный прогон и по умолчанию равен 60. `RUST_LOG` управляет уровнем логов и по умолчанию равен `warn`.

## Конвейер

```text
segment → facts → rules → requirement_checks → set_checks
        → consistency → ambiguity → verify → clarify
```

| Крейт | Назначение |
| --- | --- |
| `doc` | Модель документа, координаты блоков, реестр парсеров |
| `llm` | Контракт провайдера, mock, конфигурация Gemini, кэш, повторы, лимит и трасса |
| `review` | Доменные типы, стадии и проверки, сводка и форматы отчёта |
| `cli` | Конфигурация, команды `tz-agent`, чтение и запись файлов |

[Архитектура](docs/architecture.md) · [Решения](docs/decisions.md) · [Черновик PR и следующие задачи](docs/project-skeleton-pr.md)

## Проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
