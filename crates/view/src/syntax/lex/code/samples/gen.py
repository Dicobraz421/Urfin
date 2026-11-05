#!/usr/bin/env python3
"""Случайные тексты по грамматике tree-sitter для сверки подсветки кода (ADR-0024 §8).

Обход `src/grammar.json` крейта грамматики случайным выводом: корпус там, где настоящих файлов
нет на стенде (Java, C#). Текст с ошибкой в дереве сверка пропускает сама.

    python gen.py <grammar.json> <папка> <расширение> <сколько> [зерно]
    HL_PACK=<DLL> HL_CORPUS=<папка> cargo test -p urfin-view --lib java_corpus -- --nocapture
"""
import json
import os
import random
import re
import sys

NAMES = ["foo", "bar", "baz", "qux", "Foo", "Bar", "x", "y", "item", "value", "T", "MAX_SIZE"]
CLASS_CHARS = "abcxyzABC_019 "
# Через запятую: аннотации внутри типов, `yield` как выражение и т. п.
SKIP = set(filter(None, os.environ.get("GEN_SKIP", "").split(",")))


def sample(rx, rnd):
    """Текст по регулярке (подмножество, которого хватает грамматикам)."""
    pos = 0

    def alt():
        nonlocal pos
        opts = [seq()]
        while pos < len(rx) and rx[pos] == "|":
            pos += 1
            opts.append(seq())
        return rnd.choice(opts)

    def seq():
        nonlocal pos
        out = []
        while pos < len(rx) and rx[pos] not in "|)":
            atom_out = atom()
            lo, hi = quant()
            out.append(atom_out * rnd.randint(lo, hi))
        return "".join(out)

    def quant():
        nonlocal pos
        if pos >= len(rx):
            return 1, 1
        c = rx[pos]
        if c == "?":
            pos += 1
            return 0, 1
        if c == "*":
            pos += 1
            return 0, 2
        if c == "+":
            pos += 1
            return 1, 2
        if c == "{":
            m = re.match(r"\{(\d+)(?:,(\d*))?\}", rx[pos:])
            if m:
                pos += m.end()
                lo = int(m.group(1))
                hi = int(m.group(2)) if m.group(2) else lo
                return lo, min(hi, lo + 2)
        return 1, 1

    def atom():
        nonlocal pos
        c = rx[pos]
        pos += 1
        if c == "(":
            if rx[pos:pos + 2] == "?:":
                pos += 2
            s = alt()
            pos += 1
            return s
        if c == "[":
            return klass()
        if c == ".":
            return "a"
        if c == "\\":
            e = rx[pos]
            pos += 1
            if e in "dD":
                return "1"
            if e in "wW":
                return "a"
            if e in "sS":
                return " "
            if e in "pPuU":
                if pos < len(rx) and rx[pos] == "{":
                    pos = rx.index("}", pos) + 1
                return "a"
            return {"n": "\n", "r": "", "t": "\t"}.get(e, e)
        return c

    def klass():
        nonlocal pos
        neg = rx[pos] == "^"
        if neg:
            pos += 1
        items = []
        first = True
        while first or rx[pos] != "]":
            first = False
            c = rx[pos]
            pos += 1
            if c == "\\":
                c = rx[pos]
                pos += 1
                if c == "p":
                    pos = rx.index("}", pos) + 1
                    items.append("a")
                    continue
                c = {"d": "1", "w": "a", "s": " ", "n": "\n", "t": "\t", "r": "\r"}.get(c, c)
            if pos + 1 < len(rx) and rx[pos] == "-" and rx[pos + 1] != "]":
                hi = rx[pos + 1]
                pos += 2
                if hi == "\\":
                    hi = rx[pos]
                    pos += 1
                items.extend(chr(x) for x in range(ord(c), min(ord(hi), ord(c) + 40) + 1))
            else:
                items.append(c)
        pos += 1
        if neg:
            ok = [x for x in CLASS_CHARS if x not in items]
            return rnd.choice(ok) if ok else "a"
        return rnd.choice(items)

    return alt()


class Gen:
    def __init__(self, grammar, rnd):
        self.rules = grammar["rules"]
        self.word = grammar.get("word")
        self.rnd = rnd
        self.cost = {}
        self.costs()

    def weight(self, r, seen):
        t = r["type"]
        if t in ("SYMBOL", "ALIAS", "FIELD", "PREC", "PREC_LEFT", "PREC_RIGHT", "PREC_DYNAMIC",
                 "TOKEN", "IMMEDIATE_TOKEN", "REPEAT1", "RESERVED"):
            if t == "SYMBOL":
                return self.cost.get(r["name"], 10**6 if r["name"] in self.rules else 1)
            return self.weight(r["content"], seen)
        if t == "SEQ":
            return 1 + sum(self.weight(m, seen) for m in r["members"])
        if t == "CHOICE":
            return 1 + min(self.weight(m, seen) for m in r["members"])
        return 1

    def costs(self):
        for _ in range(60):
            for name, r in self.rules.items():
                self.cost[name] = min(self.cost.get(name, 10**6), self.weight(r, None))

    def skipped(self, r):
        """Правило, которого в обычном коде нет (`GEN_SKIP`)."""
        if r["type"] == "SYMBOL":
            return r["name"] in SKIP
        if r["type"] in ("ALIAS", "FIELD", "PREC", "PREC_LEFT", "PREC_RIGHT", "PREC_DYNAMIC"):
            return self.skipped(r["content"])
        return False

    def gen(self, r, depth, out, glue=False):
        t = r["type"]
        if t == "SYMBOL":
            name = r["name"]
            # Мягкие ключевые слова как имена — неоднозначность, которой в коде нет.
            if name == self.word or name in (
                "identifier",
                "_identifier_token",
                "_reserved_identifier",
                "reserved_identifier",
                "contextual_keyword",
            ):
                out.append(self.rnd.choice(NAMES))
            elif name in self.rules:
                self.gen(self.rules[name], depth + 1, out)
        elif t in ("ALIAS", "FIELD", "PREC", "PREC_LEFT", "PREC_RIGHT", "PREC_DYNAMIC", "RESERVED"):
            self.gen(r["content"], depth, out)
        elif t in ("TOKEN", "IMMEDIATE_TOKEN"):
            part = []
            self.gen(r["content"], depth, part)
            s = "".join(part)
            if t == "IMMEDIATE_TOKEN" and out:
                out[-1] += s
            else:
                out.append(s)
        elif t == "SEQ":
            for m in r["members"]:
                self.gen(m, depth, out)
        elif t == "CHOICE":
            ms = [m for m in r["members"] if not self.skipped(m)] or r["members"]
            if depth > 24:
                m = min(ms, key=lambda x: self.weight(x, None))
            else:
                m = self.rnd.choice(ms)
            self.gen(m, depth, out)
        elif t in ("REPEAT", "REPEAT1"):
            lo = 0 if t == "REPEAT" else 1
            n = lo if depth > 20 else self.rnd.randint(lo, 3)
            if self.skipped(r["content"]):
                return
            for _ in range(n):
                self.gen(r["content"], depth + 1, out)
        elif t == "STRING":
            out.append(r["value"])
        elif t == "PATTERN":
            out.append(sample(r["value"], self.rnd))


def layout(tokens, rnd):
    """Токены в текст: переводы строк там, где они в обычном коде."""
    out = []
    for t in tokens:
        if not t:
            continue
        out.append(t)
        if t in (";", "{", "}"):
            out.append("\n")
        elif t == "," and rnd.random() < 0.4:
            out.append("\n")
        else:
            out.append(" ")
    return "".join(out)


def main():
    path, folder, ext, count = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
    seed = int(sys.argv[5]) if len(sys.argv) > 5 else 1
    grammar = json.load(open(path, encoding="utf8"))
    os.makedirs(folder, exist_ok=True)
    start = next(iter(grammar["rules"].values()))
    for i in range(count):
        rnd = random.Random(seed * 100003 + i)
        g = Gen(grammar, rnd)
        tokens = []
        for _ in range(rnd.randint(3, 12)):
            g.gen(start, 0, tokens)
        text = layout(tokens, rnd)
        with open(os.path.join(folder, f"g{i}.{ext}"), "w", encoding="utf8", newline="\n") as f:
            f.write(text)


if __name__ == "__main__":
    sys.setrecursionlimit(20000)
    main()
