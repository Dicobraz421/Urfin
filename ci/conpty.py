#!/usr/bin/env python3
"""Свой ConPTY для дистрибутива (S6-02b, решение №76): python3 ci/conpty.py <каталог>.

Скачивает пакет NuGet `Microsoft.Windows.Console.ConPTY` закреплённой версии,
сверяет SHA-256 и кладёт в <каталог> рядом с `urfin.exe`: `conpty.dll`,
`OpenConsole.exe` (x64) и `conpty-LICENSE.txt` (MIT). В git файлов нет; сеть —
только на шаге сборки, рантайм офлайн. Новая версия — правкой VERSION и SHA256.

Локально (dev-сборка со своим ConPTY): python ci/conpty.py target/debug
Вывод — ASCII: консоль windows-раннера не UTF-8.
"""

import hashlib
import io
import os
import sys
import urllib.request
import zipfile

VERSION = "1.25.260930003"
SHA256 = "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee"
URL = (
    "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/"
    f"{VERSION}/microsoft.windows.console.conpty.{VERSION}.nupkg"
)
FILES = {
    "runtimes/win-x64/native/conpty.dll": "conpty.dll",
    "build/native/runtimes/x64/OpenConsole.exe": "OpenConsole.exe",
}
LICENSE = f"""conpty.dll, OpenConsole.exe: Microsoft.Windows.Console.ConPTY {VERSION}
https://github.com/microsoft/terminal

MIT License

Copyright (c) Microsoft Corporation. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: conpty.py <dir>", file=sys.stderr)
        return 2
    dest = sys.argv[1]
    os.makedirs(dest, exist_ok=True)
    with urllib.request.urlopen(URL, timeout=120) as r:
        data = r.read()
    got = hashlib.sha256(data).hexdigest()
    if got != SHA256:
        print(f"conpty: sha256 mismatch {got} != {SHA256}", file=sys.stderr)
        return 1
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for src, name in FILES.items():
            with open(os.path.join(dest, name), "wb") as f:
                f.write(z.read(src))
    with open(os.path.join(dest, "conpty-LICENSE.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(LICENSE)
    print(f"conpty {VERSION} -> {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
