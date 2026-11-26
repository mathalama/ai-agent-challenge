# Агент проверки требований

Rust CLI для статической проверки требований к финтех-проектам. Агент выделяет требования, анализирует их с Gemini, сверяет цитаты и формирует отчёт на русском. Поддерживаются Markdown, обычный текст (UTF‑8), PDF и DOCX. [Задание хакатона](https://github.com/BCC-HUB/ai-requirements-validator-task)

## Быстрый старт

Нужны Rust 1.97 или новее ([установка](https://www.rust-lang.org/tools/install)) и ключ Gemini. Версия 1.97 указана в поле `rust-version` зависимости `pdfsink-rs`, проверено на Rust 1.100. На Linux для сборки нужны компилятор C, `pkg-config` и `libssl-dev` (как в [Dockerfile](Dockerfile)).

1. **Получите ключ.** Создайте ключ в [Google AI Studio](https://aistudio.google.com/apikey) в разделе API Keys.
2. **Положите ключ в `.env`.** Из корня репозитория выполните `cp .env.example .env` (в PowerShell `Copy-Item .env.example .env`) и впишите ключ в `GEMINI_API_KEY`. Без `.env` ключ можно сохранить командой `cargo run -p cli -- config`. Модель, лимиты запросов и другие провайдеры описаны в разделах ниже.
3. **Запустите проверку.** Первый аргумент — документ (Markdown, TXT, PDF или DOCX), второй — путь к отчёту.

   ```sh
   cargo run -p cli -- <INPUT> <REPORT>
   ```

4. **Заберите отчёт** по пути `<REPORT>`. Недостающие каталоги создаются сами. Расширение `.json` даёт JSON, любое другое — Markdown. Рядом с отчётом лежит файл ответов `*.answers.json`, если вы отвечали на вопросы агента.

Пример на документе организаторов: `cargo run -p cli -- samples/requirements.md reports/report.md`. Без ключа тот же прогон работает на заглушке: добавьте `--provider mock`. Если ключа нет, а провайдер Gemini, команда останавливается с ошибкой «Не задана или некорректна переменная GEMINI_API_KEY».

Короткая форма равна `cargo run -p cli -- check <INPUT> -o <REPORT>`. Если путь совпадает с именем подкоманды (`check`, `chat` и так далее), выполняется подкоманда.

### Установка команды `rs`

```sh
cargo install --path crates/cli
rs <INPUT> <REPORT>
```

Длинная форма `rs check <INPUT> -o <REPORT>` делает то же самое. Холодная установка с нуля занимает около минуты на Apple Silicon, когда пакеты уже скачаны. На другой машине и при первой загрузке пакетов время зависит от процессора и сети. Папка `~/.cargo/bin` должна быть в `PATH`; установщик Rust обычно добавляет её сам.

Установленный `rs` ищет `.env` в текущем каталоге и выше, а `web/dist` и `samples/` относительно текущего каталога. Запускайте его из корня репозитория.

### Windows

Эти шаги описаны по документации Rust и коду проекта и **не проверены на Windows**.

- Установите Rust через `rustup-init.exe`. Для стандартного набора MSVC нужны Microsoft C++ Build Tools.
- Команды выполняйте в PowerShell из корня репозитория. Пути можно писать с обратными слэшами: `cargo run -p cli -- samples\requirements.md reports\report.md --provider mock`.
- `.env` копируется командой `Copy-Item .env.example .env`.
- Настройки из `rs config` лежат в `%APPDATA%\rs\config.json`.
- Если сборка на Windows не получается, соберите и запустите образ через Docker: `docker compose up --build` (раздел ниже).

### Чат в терминале и браузере

`rs` и `rs chat` открывают чат. В терминале доступны строка статуса и подсказки команд по `/`.
Ctrl+C прерывает текущую команду. Повторное нажатие завершает чат.
При перенаправлении ввода используйте rs chat для построчного чтения команд.
Для прогона без ключа используйте `cargo run -p cli -- chat --provider mock`. Для чата нужен терминал: при перенаправленном вводе `rs` без команды завершается с ошибкой.
Введите `/load samples/requirements.md`, затем `/check`. После проверки доступны `/findings`, `/questions`,
`/verdict`, свободные вопросы и `/ask <вопрос>`. Ответ на уточнение можно отправить командой
`/answer <ID> <текст>`. Терминал записывает ответы рядом с документом в `*.answers.json`.
`/export md` и `/export json` выгружают отчёт в `reports/`, `/exit` завершает сессию.

### Веб-интерфейс

Нужен Node.js (в Dockerfile используется Node 24, на другой версии не проверялось). Из корня репозитория выполните.

```sh
npm --prefix web ci
npm --prefix web run build
cargo run -p cli -- serve --provider mock
```

Откройте [localhost:3333](http://127.0.0.1:3333), прикрепите документ и нажмите «Проверить». Для настоящей модели уберите `--provider mock` и задайте ключ, как в быстром старте.

- Порт и адрес задаются флагом `--listen`, по умолчанию `127.0.0.1:3333`: `cargo run -p cli -- serve --listen 127.0.0.1:8080`.
- Сервер раздаёт `web/dist` относительно текущего каталога. Другой каталог задаётся флагом `--web-dir`.
- Если `web/dist` нет, сервер всё равно запускается и пишет «Сервер доступен», но страница отвечает 404. Сначала выполните `npm --prefix web run build`.

История завершённых проверок хранится в браузере. Активные веб-сессии хранятся в памяти сервера, после перезагрузки страница возвращается к своей сессии, в том числе к идущей проверке. Режим разработки `npm --prefix web run dev` подключается к серверу
на порту 3333 и показывает демонстрацию на готовых данных, когда сервер недоступен.

До запуска проверки чат отвечает по шаблонам. Сбор рассуждений включается командой `/reasoning on`
и выключается командой `/reasoning off`. OpenAI-совместимые модели могут передавать рассуждение по частям.
Gemini и кэшированные ответы передают его после запроса.

### Настройки провайдера

Запустите `rs config` для интерактивной настройки или откройте «Настройки» в локальном веб-чате.
Команда `rs config show` показывает маскированные ключи, а `rs config test` проверяет соединение.
Для автоматизации используйте `rs config set provider mock`, `rs config set model <ID>` и
`rs config set api-key` с ключом на stdin.

Настройки сохраняются в `%APPDATA%/rs/config.json` на Windows и
`$XDG_CONFIG_HOME/rs/config.json` или `~/.config/rs/config.json` на Linux и macOS.
Флаг `--provider` перекрывает `RS_PROVIDER`. Переменные окружения и `.env`
имеют приоритет над файлом настроек. В чате `/provider` и `/model` меняют параметры текущей сессии.

Пресет OpenRouter загружает каталог текстовых моделей и предлагает фильтр «Только бесплатные».
Формат JSON выбирается по возможностям модели. Проверка соединения запрашивает статус ключа
и остаток дневной квоты через `/key`. Для OpenRouter укажите `OPENROUTER_API_KEY` либо задайте
`OPENAI_API_KEY` вместе с `OPENAI_BASE_URL=https://openrouter.ai/api/v1`. [Документация OpenRouter](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key)

Ключ из профиля используется только с сохранённым адресом API. При смене адреса введите новый ключ.
Ключи `OPENAI_API_KEY` и `GEMINI_API_KEY` привязаны к адресу из соответствующей переменной
`*_BASE_URL` или к адресу по умолчанию.

### Запуск в Docker

Образ собирает веб-интерфейс и бинарь `rs`, Rust и Node на машине не нужны.

```sh
docker compose up --build
```

По умолчанию в паспорт отчёта записывается версия пакета. Чтобы указать ревизию Git,
соберите образ командой `docker compose build --build-arg AGENT_VERSION="$(git rev-parse --short HEAD)"`
и запустите его через `docker compose up`. В PowerShell используйте тот же синтаксис подстановки.

Веб-чат откроется на [localhost:3333](http://127.0.0.1:3333) с провайдером `mock`. Для настоящей модели
заполните `.env` по образцу `.env.example` и укажите провайдер: `PROVIDER=gemini docker compose up`
или строку `PROVIDER=openai` в `.env`. Порт открыт только для localhost, другой порт задаётся
переменной `PORT`, например `PORT=8080 docker compose up`. Отчёты пишутся в `reports/`,
кэш ответов модели хранится в томе `llm-cache`.
В Compose провайдер задаётся через `PROVIDER`. Окно настроек доступно при прямом запуске сервера на localhost.

Чат в терминале и разовые команды запускаются в том же образе. Примеры из `samples/` и разметка `eval/` уже внутри.

```sh
docker compose run --rm app chat --provider mock
docker compose run --rm app check samples/requirements.md -o reports/report.md --provider mock
```

### Команды для скриптов

```text
rs <INPUT> <REPORT> [--provider gemini|openai|mock] [--no-cache] [--trace <PATH>] [--reasoning] [--events <PATH>]
rs check <INPUT> [-o <OUTPUT>] [--format md|json] [--provider gemini|openai|mock] [--no-cache] [--trace <PATH>] [--reasoning] [--events <PATH>]
rs ask <INPUT> [-o <PATH>] [--provider gemini|openai|mock] [--no-cache]
rs eval [--golden eval/] [--provider gemini|openai|mock] [--no-cache]
```

CLI отправляет разобранный текст документа выбранному провайдеру и записывает отчёт в `reports/report.md` или `reports/report.json` в зависимости от формата. `--trace <PATH>` пишет JSON с порядком и длительностью выполнения стадий, а также расходом токенов по вызовам LLM. `--no-cache` отключает чтение и запись кэша `.cache/llm/`. Во время прогона CLI печатает в stderr ход по стадиям: время, расход токенов, число требований и находок, а также повторы запросов к LLM с причиной и паузой. `--events <PATH>` пишет те же события в JSON Lines по мере выполнения, формат описан в [docs/api/README.md](docs/api/README.md#события-прогресса). Команда `eval` рассчитывает precision, recall и список пропусков по эталонным разметкам в `eval/` (формат описан в [eval/README.md](eval/README.md)).

Флаг `--reasoning` сохраняет резюме рассуждений Gemini и рассуждения, которые возвращают OpenAI-совместимые модели. Текст сохраняется в кэше и записывается в поле `reasoning` каждого элемента `llm_calls` при использовании `--trace`. Если провайдер не вернул текст в ответе, значение поля будет `null`. По умолчанию сбор выключен. Для Gemini с этим флагом нужна [модель с поддержкой рассуждений](https://ai.google.dev/gemini-api/docs/generate-content/thinking).

Для OpenAI-совместимых API режим рассуждений определяется моделью и настройками провайдера. Например, в OpenRouter он [может быть включён по умолчанию для конкретной модели](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens#discovering-per-model-reasoning-options).

`rs ask` показывает открытые вопросы по одному. Можно выбрать вариант командой `#1`, написать свой ответ или нажать Enter для пропуска. Если все варианты — целые числа, ввод `15000` сохраняет это число как ответ. При текстовых вариантах число без префикса выбирает вариант, а `=15000` сохраняет собственный числовой ответ. Ответы сразу сохраняются по ID вопроса в `reports/report.answers.json`. Для собственного пути отчёта передайте одинаковый `-o` в обе команды. Например, для `reports/review.md` файл ответов — `reports/review.answers.json`. Повторный `rs check` показывает текст ответов под связанными находками. Исправления в документе проверяются отдельно при каждом прогоне. Если сохранённый ответ больше не совпадает с вопросом, CLI предупреждает об этом и сохраняет ответ в JSON-файле и в отчёте.

## Настройка Gemini

1. Войдите в Google AI Studio и создайте ключ в разделе API Keys. Выберите проект Google Cloud, если Studio запросит проект. [Google AI Studio](https://aistudio.google.com/apikey) · [Инструкция Google](https://ai.google.dev/gemini-api/docs/api-key)
2. Скопируйте `.env.example` в `.env` в корне проекта и запишите ключ в `GEMINI_API_KEY`. По умолчанию используется `gemini-3.1-flash-lite`. Чтобы выбрать другую модель, задайте её в `GEMINI_MODEL`. [Каталог моделей Google](https://ai.google.dev/gemini-api/docs/models)

`LLM_REQUESTS_PER_MINUTE` задаёт локальный темп запросов к Gemini и по умолчанию равен 5. Сверьте значение с активной квотой проекта и модели. При HTTP 429 агент учитывает задержку из ответа Gemini и ждёт 60 секунд, если сервер её не указал. `RUST_LOG` управляет уровнем логов и по умолчанию равен `warn`. [Лимиты Gemini API](https://ai.google.dev/gemini-api/docs/rate-limits) · [Активные квоты в AI Studio](https://aistudio.google.com/rate-limit)

## Другие модели (API, совместимый с OpenAI)

`--provider openai` подключает любой сервис с API OpenAI Chat Completions: OpenAI, OpenRouter, Groq, DeepSeek и другие. Задайте в `.env` адрес, ключ и модель.

| Переменная | Значение |
| --- | --- |
| `OPENAI_API_KEY` | Ключ сервиса, обязательно |
| `OPENAI_MODEL` | Имя модели в каталоге сервиса, обязательно |
| `OPENAI_BASE_URL` | Адрес API без `/chat/completions`, по умолчанию `https://api.openai.com/v1` |
| `OPENAI_RESPONSE_FORMAT` | `json_schema` (по умолчанию) или `json_object` для сервисов без строгой схемы, например DeepSeek |
| `OPENAI_REASONING_EFFORT` | Необязательно: `reasoning_effort` для рассуждающих моделей, например `low` или `none` |
| `OPENAI_MAX_TOKENS` | Необязательный положительный лимит токенов ответа. Передаётся как `max_tokens`. Пустое значение оставляет настройку сервиса |
| `OPENAI_TIMEOUT_SECS` | Таймаут одного запроса в секундах, по умолчанию 180; для локальных моделей увеличьте до 600–900 |

Локальная модель через [Ollama](https://ollama.com/): `OPENAI_BASE_URL=http://localhost:11434/v1`, любой непустой `OPENAI_API_KEY`, модель с контекстом не меньше 12K токенов, `OPENAI_TIMEOUT_SECS=900`, для qwen3 — `OPENAI_REASONING_EFFORT=none`, иначе модель долго рассуждает и тратит токены.

Адреса API: OpenRouter — `https://openrouter.ai/api/v1`, Groq — `https://api.groq.com/openai/v1`, DeepSeek — `https://api.deepseek.com`. Лимит `LLM_REQUESTS_PER_MINUTE`, повторы и кэш работают так же, как для Gemini. Название модели попадает в паспорт прогона.

## Конвейер

```text
triage → segment → facts → rules → requirement_checks → set_checks
       → consistency → ambiguity → verify → clarify
```

| Крейт | Назначение |
| --- | --- |
| `doc` | Модель документа, координаты блоков, реестр парсеров |
| `llm` | Контракт провайдера, mock, HTTP-клиенты Gemini и OpenAI-совместимых API, кэш, повторы, лимит и трасса |
| `review` | Доменные типы, стадии и проверки, сводка и форматы отчёта |
| `agent` | Чат-сессия, маршрутизация команд и общий запуск проверки |
| `server` | Локальный HTTP-сервер, REST, SSE и раздача веб-интерфейса |
| `cli` | Конфигурация, команды `rs`, чтение и запись файлов |

[Архитектура](docs/architecture.md) · [Решения](docs/decisions.md)

## Демо и защита

Материалы для показа лежат в [docs/demo/](docs/demo/): [сценарий на 4–5 минут](docs/demo/scenario.md) с вариантами на реальной модели и на `--provider mock`, [слайды](docs/demo/slides.pdf) (исходник [slides.md](docs/demo/slides.md)) и [ответы на вопросы жюри](docs/demo/qa.md).

Запасная запись демо: _ссылка на видео — добавить после записи_. Видео в репозиторий не добавляется.

## Проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
