#!/usr/bin/env python3
"""装配可分发的产物。

两个东西：
1. `<name>-<version>-<target>.dbxp` —— DBX 插件包（zip）
2. `devbox-try-<target>/` —— 便携试用套件，解压双击即可跑，不依赖 DBX

不调用任何外部打包工具：本地没有 dbx-plugin CLI，这里只做"按约定摆放文件 + 压缩"。
正式上架前必须由官方流程重新签名。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"


def run(cmd: list[str], cwd: Path) -> None:
    print("  $", " ".join(cmd))
    subprocess.run(cmd, cwd=cwd, check=True)


def build(target: str) -> None:
    print("→ 编译 release")
    run(["cargo", "build", "--release", "--manifest-path", "backend/sidecar/Cargo.toml"], ROOT)
    run(["cargo", "build", "--release", "--manifest-path", "backend/engine/Cargo.toml"], ROOT)


def manifest_for(exe: str) -> dict:
    data = json.loads((ROOT / "manifest.json").read_text(encoding="utf-8"))
    data["entrypoints"]["backend"]["executable"] = f"bin/{exe}"
    return data


def stage_plugin(target: str, exe: str, version: str) -> Path:
    out = DIST / f"devbox-plugin-{version}-{target}"
    if out.exists():
        shutil.rmtree(out)
    (out / "bin").mkdir(parents=True)

    shutil.copy2(ROOT / "README.md", out / "README.md")
    shutil.copytree(ROOT / "assets", out / "assets")
    shutil.copytree(ROOT / "ui", out / "ui", ignore=shutil.ignore_patterns("node_modules", "*.map"))
    shutil.copytree(ROOT / "catalog", out / "catalog")
    shutil.copy2(ROOT / "backend" / "sidecar" / "target" / "release" / exe, out / "bin" / exe)

    (out / "manifest.json").write_text(
        json.dumps(manifest_for(exe), ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return out


def zip_dir(src: Path, dest: Path) -> None:
    if dest.exists():
        dest.unlink()
    with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as zf:
        for path in sorted(src.rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(src).as_posix())


TRY_CMD = r"""@echo off
chcp 65001 >nul
setlocal enabledelayedexpansion
set "HERE=%~dp0"
set "DATA=%HERE%data"
set "CAT=%HERE%catalog\jdk.yaml"
set "INSTALL=%HERE%devbox-install.exe"
set "SHIM=%HERE%devbox-shim.exe"

echo ============================================
echo  dev-box 试用
echo  数据目录: %DATA%
echo ============================================
echo.

echo [1/4] 安装 Node.js 22.11.0（约 33 MB）
"%INSTALL%" --catalog "%CAT%" --component node --version 22.11.0 --data "%DATA%" --activate
if errorlevel 1 goto fail
echo.

echo [2/4] 安装 Maven 3.9.9（约 9 MB）
"%INSTALL%" --catalog "%CAT%" --component maven --version 3.9.9 --data "%DATA%" --activate
if errorlevel 1 goto fail
echo.

echo [3/4] 生成命令行入口
"%SHIM%" --data "%DATA%" --catalog "%CAT%"
if errorlevel 1 goto fail
echo.

echo [4/4] 通过入口执行
"%DATA%\bin\node.cmd" -v
if errorlevel 1 goto fail

if not defined JAVA_HOME (
  for /f "tokens=2 delims==" %%J in ('java -XshowSettings:properties -version 2^>^&1 ^| findstr /C:"java.home ="') do set "JAVA_HOME=%%J"
  set "JAVA_HOME=!JAVA_HOME:~1!"
)
if defined JAVA_HOME (
  "%DATA%\bin\mvn.cmd" -v 2>nul | findstr /C:"Apache Maven"
) else (
  echo   (未找到 JDK，跳过 mvn 验证。装上 JDK 后再试：%DATA%\bin\mvn.cmd -v)
)
echo.

echo ============================================
echo  完成。试试点这些：
echo    切换 Maven 版本（入口不用重配）:
echo      "%INSTALL%" --catalog "%CAT%" --component maven --version 3.8.8 --data "%DATA%" --activate
echo      "%DATA%\bin\mvn.cmd" -v
echo.
echo    卸载干净: 直接删掉 "%DATA%" 整个目录
echo ============================================
pause
exit /b 0

:fail
echo.
echo 安装失败，把上面的错误信息发回来即可。
pause
exit /b 1
"""

TRY_DOC = """# dev-box 试用说明

这个目录是**便携试用版**，不依赖 DBX，解压后双击 `开始试用.cmd` 即可。

## 它会做什么

1. 从目录里取出 Node.js 22.11.0 的地址和校验值，下载 → 校验 sha256 → 解压 → 原子落盘
2. 同样装 Maven 3.9.9
3. 在 `data\\bin` 生成命令行入口（`node` / `npm` / `npx` / `mvn`）
4. 通过入口执行 `node -v` 和 `mvn -v`

全部数据都在本目录的 `data\\` 下，**删掉这个目录就等于卸载干净**。

## 值得试的几个点

| 想验证的 | 怎么做 |
|---|---|
| 切换版本 | 装 3.8.8 并激活，同一个 `mvn.cmd` 会立刻变成 3.8.8（入口不用重新生成） |
| 校验拦截 | 把 `catalog\\jdk.yaml` 里 maven 的 sha512 改错一位再装，会拒绝落盘且不留垃圾 |
| 断点续传 | 下载中途 Ctrl+C，再跑一次，会从断点继续 |
| 未验证配方 | 装 JDK 会被拒绝，提示"校验值仍是占位符"——这是刻意的 |

## 还没做到的部分

- **JDK 还装不了**：配方里的校验值还是占位符，引擎会直接拒绝。Maven 的运行验证因此需要你系统里已有 JDK。
- **只在 Windows 上验过**：Linux/macOS 的代码路径（symlink、执行位）从未被执行过。
- **不会自动改你的 PATH**：入口目录是 `data\\bin`，需要你自己加。这是刻意不做的事。
- 服务类组件（MySQL/Redis 等）还没有实例管理，目前只有工具链。

## 已知限制

- `devbox-install.exe` / `devbox-shim.exe` 是命令行工具，没有图形界面；图形界面在 `.dbxp` 包里，需要 DBX 宿主加载。
- 安装包的签名：这个包是本地手工装配的，没有经过商店签名。
"""


def stage_try(target: str) -> Path:
    out = DIST / f"devbox-try-{target}"
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    for name in ["devbox-install.exe", "devbox-shim.exe"]:
        shutil.copy2(ROOT / "backend" / "engine" / "target" / "release" / name, out / name)
    shutil.copytree(ROOT / "catalog", out / "catalog")

    (out / "开始试用.cmd").write_text(TRY_CMD.replace("\n", "\r\n"), encoding="utf-8")
    (out / "试用说明.md").write_text(TRY_DOC, encoding="utf-8")
    return out


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--target", default="windows-x64")
    ap.add_argument("--skip-build", action="store_true")
    args = ap.parse_args()

    version = json.loads((ROOT / "manifest.json").read_text(encoding="utf-8"))["version"]
    exe = "devbox-sidecar.exe" if args.target.startswith("windows") else "devbox-sidecar"

    if not args.skip_build:
        build(args.target)

    DIST.mkdir(exist_ok=True)
    print("→ 装配插件包")
    plugin_dir = stage_plugin(args.target, exe, version)
    dbxp = DIST / f"devbox-{version}-{args.target}.dbxp"
    zip_dir(plugin_dir, dbxp)
    print(f"  {dbxp.relative_to(ROOT)}  {dbxp.stat().st_size / 1_048_576:.1f} MB")

    print("→ 装配试用套件")
    try_dir = stage_try(args.target)
    kit = DIST / f"devbox-try-{version}-{args.target}.zip"
    zip_dir(try_dir, kit)
    print(f"  {kit.relative_to(ROOT)}  {kit.stat().st_size / 1_048_576:.1f} MB")

    print()
    print("sha256")
    print(f"  {dbxp.name}  {sha256(dbxp)}")
    print(f"  {kit.name}  {sha256(kit)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
