#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - "$@" <<'PY'
import io
import pathlib
import re
import subprocess
import sys
import tokenize

MODE_STRIP = "--strip" in sys.argv[1:]

EXCLUDED_FILES = {
    "assets/ThirdPartyLicenses.html",
    "site/pnpm-lock.yaml",
}


def tracked_files() -> list[pathlib.Path]:
    out = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard"], text=True)
    files = []
    for rel in out.splitlines():
        if not rel or rel in EXCLUDED_FILES:
            continue
        p = pathlib.Path(rel)
        if not p.is_file():
            continue
        if "stageshow" in p.parts:
            continue
        if p.suffix in {".rs", ".m", ".swift", ".ts", ".tsx", ".mjs", ".js", ".jsx", ".css", ".astro", ".html", ".sh", ".py", ".toml", ".yml"}:
            files.append(p)
    return files


def is_ident_char(ch: str) -> bool:
    return ch.isalnum() or ch == "_"


def scan_rust(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch in ("r", "b", "c") and (i == 0 or not is_ident_char(text[i - 1])):
            j = i
            if ch in ("b", "c") and j + 1 < n and text[j + 1] == "r":
                j += 1
            if text[j] == "r":
                k = j + 1
                while k < n and text[k] == "#":
                    k += 1
                if k < n and text[k] == '"':
                    hashes = k - (j + 1)
                    end_seq = '"' + ("#" * hashes)
                    pos = text.find(end_seq, k + 1)
                    i = n if pos == -1 else pos + len(end_seq)
                    continue
            if ch == "b" and i + 1 < n and text[i + 1] == "'":
                i += 2
                if i < n and text[i] == "\\":
                    i += 2
                while i < n and text[i] != "'":
                    if text[i] == "\\":
                        i += 2
                    else:
                        i += 1
                i += 1
                continue
        if ch == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            continue
        if ch == "'":
            if i + 2 < n and text[i + 1] == "\\":
                k = i + 2
                while k < n and text[k] not in ("'", "\n") and (k - i) <= 12:
                    k += 1
                if k < n and text[k] == "'":
                    i = k + 1
                    continue
            elif i + 2 < n and text[i + 1] not in ("\\", "\n", "'") and text[i + 2] == "'":
                i += 3
                continue
            i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "/":
            start = i
            nl = text.find("\n", i + 2)
            i = n if nl == -1 else nl
            spans.append((start, i))
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "*":
            start = i
            depth = 1
            i += 2
            while i < n and depth > 0:
                if text[i] == "/" and i + 1 < n and text[i + 1] == "*":
                    depth += 1
                    i += 2
                elif text[i] == "*" and i + 1 < n and text[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    i += 1
            spans.append((start, i))
            continue
        i += 1
    return spans


def scan_c_like(text: str, nested_block: bool = False, triple_quote: bool = False, single_quote: bool = True) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if triple_quote and text[i : i + 3] == '"""':
            i += 3
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i : i + 3] == '"""':
                    i += 3
                    break
                else:
                    i += 1
            continue
        if ch == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            continue
        if single_quote and ch == "'":
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i] == "'":
                    i += 1
                    break
                else:
                    i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "/":
            start = i
            nl = text.find("\n", i + 2)
            i = n if nl == -1 else nl
            spans.append((start, i))
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "*":
            start = i
            depth = 1
            i += 2
            while i < n and depth > 0:
                if nested_block and text[i] == "/" and i + 1 < n and text[i + 1] == "*":
                    depth += 1
                    i += 2
                elif text[i] == "*" and i + 1 < n and text[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    i += 1
            spans.append((start, i))
            continue
        i += 1
    return spans


def scan_js(text: str, offset: int = 0) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    n = len(text)
    regex_prev_keywords = {"return", "case", "throw", "in", "of", "typeof", "void", "delete", "await", "yield", "else", "do", "new"}

    def allows_regex(pos: int) -> bool:
        j = pos - 1
        while j >= 0 and text[j] in " \t\r\n":
            j -= 1
        if j < 0:
            return True
        c = text[j]
        if c in "([,{;:!?=&|^~+-*%<>":
            return True
        if c.isalnum() or c in "_$":
            k = j
            while k >= 0 and (text[k].isalnum() or text[k] in "_$"):
                k -= 1
            return text[k + 1 : j + 1] in regex_prev_keywords
        return False

    def walk(i: int, stop_brace: bool) -> int:
        brace_depth = 0
        while i < n:
            ch = text[i]
            if stop_brace:
                if ch == "{":
                    brace_depth += 1
                elif ch == "}":
                    if brace_depth == 0:
                        return i + 1
                    brace_depth -= 1
            if ch in ('"', "'"):
                q = ch
                i += 1
                while i < n:
                    if text[i] == "\\":
                        i += 2
                    elif text[i] == q:
                        i += 1
                        break
                    else:
                        i += 1
                continue
            if ch == "`":
                i += 1
                while i < n:
                    if text[i] == "\\":
                        i += 2
                    elif text[i] == "`":
                        i += 1
                        break
                    elif text[i] == "$" and i + 1 < n and text[i + 1] == "{":
                        i = walk(i + 2, True)
                    else:
                        i += 1
                continue
            if ch == "{" and text[i : i + 3] == "{/*":
                end = text.find("*/}", i + 3)
                if end != -1:
                    spans.append((offset + i, offset + end + 3))
                    i = end + 3
                    continue
            if ch == "/" and i + 1 < n and text[i + 1] == "/":
                start = i
                nl = text.find("\n", i + 2)
                i = n if nl == -1 else nl
                spans.append((offset + start, offset + i))
                continue
            if ch == "/" and i + 1 < n and text[i + 1] == "*":
                start = i
                end = text.find("*/", i + 2)
                i = n if end == -1 else end + 2
                spans.append((offset + start, offset + i))
                continue
            if ch == "/" and allows_regex(i):
                j = i + 1
                in_class = False
                closed = False
                while j < n and text[j] != "\n":
                    if text[j] == "\\":
                        j += 2
                        continue
                    if text[j] == "[":
                        in_class = True
                    elif text[j] == "]":
                        in_class = False
                    elif text[j] == "/" and not in_class:
                        j += 1
                        while j < n and text[j].isalpha():
                            j += 1
                        closed = True
                        break
                    j += 1
                if closed:
                    i = j
                    continue
            i += 1
        return i

    walk(0, False)
    return spans


def scan_css(text: str, offset: int = 0) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch in ('"', "'"):
            q = ch
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i] == q:
                    i += 1
                    break
                else:
                    i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "*":
            start = i
            end = text.find("*/", i + 2)
            i = n if end == -1 else end + 2
            spans.append((offset + start, offset + i))
            continue
        i += 1
    return spans


def scan_html_or_astro(text: str, is_astro: bool) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    pos = 0
    if is_astro and text.startswith("---\n"):
        end_fm = text.find("\n---", 4)
        if end_fm != -1:
            spans.extend(scan_js(text[4:end_fm], 4))
            pos = end_fm + 4
    n = len(text)
    i = pos
    while i < n:
        if text.startswith("<!--", i):
            end = text.find("-->", i + 4)
            end_i = n if end == -1 else end + 3
            spans.append((i, end_i))
            i = end_i
            continue
        m_style = re.match(r"<style\b[^>]*>", text[i:], re.IGNORECASE)
        if m_style:
            body_start = i + m_style.end()
            close = re.search(r"</style\s*>", text[body_start:], re.IGNORECASE)
            body_end = n if not close else body_start + close.start()
            spans.extend(scan_css(text[body_start:body_end], body_start))
            i = n if not close else body_start + close.end()
            continue
        m_script = re.match(r"<script\b[^>]*>", text[i:], re.IGNORECASE)
        if m_script:
            body_start = i + m_script.end()
            close = re.search(r"</script\s*>", text[body_start:], re.IGNORECASE)
            body_end = n if not close else body_start + close.start()
            spans.extend(scan_js(text[body_start:body_end], body_start))
            i = n if not close else body_start + close.end()
            continue
        i += 1
    return spans


def scan_python(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    lines = text.splitlines(keepends=True)
    line_offsets = [0]
    for ln in lines:
        line_offsets.append(line_offsets[-1] + len(ln))
    for tok in tokenize.generate_tokens(io.StringIO(text).readline):
        if tok.type == tokenize.COMMENT:
            row, col = tok.start
            if row == 1 and col == 0 and tok.string.startswith("#!"):
                continue
            start = line_offsets[row - 1] + col
            end = start + len(tok.string)
            spans.append((start, end))
    return spans


def scan_shell(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    if text.startswith("#!"):
        nl = text.find("\n")
        i = n if nl == -1 else nl + 1
    in_sq = False
    in_dq = False
    pending_heredocs: list[tuple[str, bool]] = []
    while i < n:
        ch = text[i]
        if ch == "\n" and pending_heredocs and not in_sq and not in_dq:
            i += 1
            for delim, strip_tabs in pending_heredocs:
                while i < n:
                    nl = text.find("\n", i)
                    line = text[i:] if nl == -1 else text[i:nl]
                    check = line.lstrip("\t") if strip_tabs else line
                    i = n if nl == -1 else nl + 1
                    if check == delim:
                        break
            pending_heredocs.clear()
            continue
        if in_sq:
            if ch == "'":
                in_sq = False
            i += 1
            continue
        if in_dq:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_dq = False
            i += 1
            continue
        if ch == "\\":
            i += 2
            continue
        if ch == "'":
            in_sq = True
            i += 1
            continue
        if ch == '"':
            in_dq = True
            i += 1
            continue
        if text[i : i + 2] == "<<" and (i + 2 >= n or text[i + 2] != "<"):
            j = i + 2
            strip_tabs = False
            if j < n and text[j] == "-":
                strip_tabs = True
                j += 1
            while j < n and text[j] in " \t":
                j += 1
            m = re.match(r"""(?:'([^']+)'|"([^"]+)"|\\?([A-Za-z_][A-Za-z0-9_]*))""", text[j:])
            if m:
                delim = m.group(1) or m.group(2) or m.group(3)
                pending_heredocs.append((delim, strip_tabs))
                i = j + m.end()
                continue
        if ch == "#" and (i == 0 or text[i - 1] in " \t\n;|&(){}"):
            start = i
            nl = text.find("\n", i + 1)
            i = n if nl == -1 else nl
            spans.append((start, i))
            continue
        i += 1
    return spans


def scan_toml(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if text[i : i + 3] == '"""':
            i += 3
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i : i + 3] == '"""':
                    i += 3
                    break
                else:
                    i += 1
            continue
        if text[i : i + 3] == "'''":
            end = text.find("'''", i + 3)
            i = n if end == -1 else end + 3
            continue
        if ch == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            continue
        if ch == "'":
            end = text.find("'", i + 1)
            i = n if end == -1 else end + 1
            continue
        if ch == "#":
            start = i
            nl = text.find("\n", i + 1)
            i = n if nl == -1 else nl
            spans.append((start, i))
            continue
        i += 1
    return spans


def scan_yaml(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                elif text[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            continue
        if ch == "'":
            i += 1
            while i < n:
                if text[i : i + 2] == "''":
                    i += 2
                elif text[i] == "'":
                    i += 1
                    break
                else:
                    i += 1
            continue
        if ch == "#" and (i == 0 or text[i - 1] in " \t\n"):
            start = i
            nl = text.find("\n", i + 1)
            i = n if nl == -1 else nl
            spans.append((start, i))
            continue
        i += 1
    return spans


def scan_file(path: pathlib.Path, text: str) -> list[tuple[int, int]]:
    ext = path.suffix
    if ext == ".rs":
        return scan_rust(text)
    if ext == ".m":
        return scan_c_like(text, nested_block=False, triple_quote=False, single_quote=True)
    if ext == ".swift":
        return scan_c_like(text, nested_block=True, triple_quote=True, single_quote=False)
    if ext in {".ts", ".tsx", ".mjs", ".js", ".jsx"}:
        return scan_js(text)
    if ext == ".css":
        return scan_css(text)
    if ext in {".astro", ".html"}:
        return scan_html_or_astro(text, is_astro=(ext == ".astro"))
    if ext == ".py":
        return scan_python(text)
    if ext == ".sh":
        return scan_shell(text)
    if ext == ".toml":
        return scan_toml(text)
    if ext == ".yml":
        return scan_yaml(text)
    return []


def strip_spans(text: str, spans: list[tuple[int, int]]) -> str:
    if not spans:
        return text
    spans = sorted(spans)
    out = []
    cur = 0
    n = len(text)
    for start, end in spans:
        if start < cur:
            continue
        before = text[cur:start]
        line_start = before.rfind("\n") + 1
        only_ws_before = before[line_start:].strip(" \t") == ""
        after_end = end
        while after_end < n and text[after_end] in " \t":
            after_end += 1
        eol_after = after_end == n or text[after_end] == "\n"
        if only_ws_before and eol_after:
            out.append(before[:line_start])
            cur = after_end + 1 if after_end < n and text[after_end] == "\n" else after_end
        else:
            out.append(before.rstrip(" \t") if eol_after else before)
            cur = end
    out.append(text[cur:])
    result = "".join(out)
    result = re.sub(r"\n{3,}", "\n\n", result)
    result = re.sub(r"^\n+", "", result)
    if result and not result.endswith("\n"):
        result += "\n"
    return result


def line_number(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


hits = []
stripped_files = 0
stripped_comments = 0

for path in tracked_files():
    text = path.read_text()
    spans = scan_file(path, text)
    if not spans:
        continue
    if MODE_STRIP:
        new_text = strip_spans(text, spans)
        if new_text != text:
            path.write_text(new_text)
            stripped_files += 1
            stripped_comments += len(spans)
    else:
        for start, end in spans:
            ln = line_number(text, start)
            preview = text[start:end].splitlines()[0][:80]
            hits.append(f"{path.as_posix()}:{ln}: {preview}")

if MODE_STRIP:
    print(f"Stripped {stripped_comments} comments across {stripped_files} files.")
else:
    if hits:
        for h in hits:
            print(h, file=sys.stderr)
        print(f"Found {len(hits)} code comment(s). Code must not contain comments.", file=sys.stderr)
        sys.exit(1)
PY
