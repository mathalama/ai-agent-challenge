"""Собирает PDF-версии samples/requirements.md для сквозной проверки разбора PDF.

Два независимых способа, чтобы ловить расхождения разбора, а не особенности одного генератора:
  samples/requirements.pdf         pandoc -> HTML -> печать в PDF браузером (Chrome/Edge/Chromium)
  samples/requirements.pandoc.pdf  pandoc -> PDF напрямую, движок typst

Нужны pandoc >= 3.1.2, typst и Chrome/Edge/Chromium. Инструменты не входят в зависимости проекта.
Запуск из корня репозитория: python scripts/gen_requirements_pdfs.py
"""
from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gen_test_pdfs import find_browser, print_pdf  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "samples" / "requirements.md"
TITLE = "Уведомление о расходах по карте"

CSS = """
@page { size: A4; margin: 20mm 18mm 22mm 18mm;
  @bottom-center { content: "Страница " counter(page) " из " counter(pages); font-size: 8pt; } }
body { font-family: Arial, Helvetica, sans-serif; font-size: 10.5pt; line-height: 1.35; color: #000; max-width: none; margin: 0; padding: 0; }
h1 { font-size: 20pt; } h2 { font-size: 15pt; } h3 { font-size: 12.5pt; } h4 { font-size: 11pt; }
table { border-collapse: collapse; width: 100%; margin: 6pt 0 10pt; }
th, td { border: 0.8pt solid #000; padding: 3pt 5pt; vertical-align: top; text-align: left; }
th { background: #e6e6e6; }
pre, code { font-family: "Courier New", Courier, monospace; font-size: 9pt; }
pre { background: #f3f3f3; padding: 6pt; white-space: pre-wrap; }
"""


# Настройки typst для PDF «напрямую через pandoc». Линии таблиц и разные кегли заголовков нужны, чтобы
# результат был похож на документ из Word, а не на таблицы без границ; мелкий шрифт таблиц оставляет
# ячейкам место без переносов по слогам. Без этого разбор таблиц в PDF не отличим от разбора обычного текста.
TYPST_HEADER = """
#set table(stroke: 0.6pt + black)
#show table: set text(size: 9pt)
#show heading.where(level: 1): set text(size: 20pt)
#show heading.where(level: 2): set text(size: 16pt)
#show heading.where(level: 3): set text(size: 13pt)
#show heading.where(level: 4): set text(size: 11.5pt)
"""


def need(tool: str) -> str:
    found = shutil.which(tool)
    if not found:
        sys.exit(f"Не найден {tool}: установите его, он нужен только для генерации образцов")
    return found


def main() -> None:
    pandoc = need("pandoc")
    need("typst")
    browser = find_browser()
    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        css = tmp_dir / "print.css"
        css.write_text(CSS, encoding="utf-8")
        html = tmp_dir / "requirements.html"
        subprocess.run(
            [pandoc, str(SOURCE), "-f", "markdown", "-t", "html5", "-s", "--embed-resources",
             "--metadata", f"pagetitle={TITLE}", "--css", str(css), "-o", str(html)],
            check=True,
        )
        print_pdf(browser, html, ROOT / "samples" / "requirements.pdf", tmp_dir / "profile")
        header = tmp_dir / "header.typ"
        header.write_text(TYPST_HEADER, encoding="utf-8")
        subprocess.run(
            [pandoc, str(SOURCE), "-o", str(ROOT / "samples" / "requirements.pandoc.pdf"),
             "--columns=1000", "--pdf-engine=typst", "-V", "mainfont=Arial", f"--include-in-header={header}"],
            check=True,
        )
    for name in ("requirements.pdf", "requirements.pandoc.pdf"):
        print(f"ok  samples/{name}  {(ROOT / 'samples' / name).stat().st_size // 1024} КБ")


if __name__ == "__main__":
    main()
