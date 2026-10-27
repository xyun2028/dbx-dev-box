#!/usr/bin/env python3
"""装配可分发的产物。

两个东西：
1. `<id>-<version>-<target>.dbxp` —— DBX 插件包（zip，含 checksums.json）
2. `devbox-try-<target>/` —— 便携试用套件，解压双击即可跑，不依赖 DBX

DBX 插件包的硬约定（从宿主安装器与官方打包脚本反推，见 docs §24）：
- `manifest.entrypoints.backend.executable` 必须指向包内真实存在的文件，
  官方布局是 `bin/<target>/<binary>[.exe]`，宿主不做任何替换；
- `checksums.json` 必须存在，形如 `{ "algorithm": "sha256", "files": { path: hex } }`，
  且 files **精确覆盖**除自身与 signature.json 之外的全部文件；
- zip 要带 Unix 权限位（bin 下 0o755、其余 0o644），否则 Unix 上侧车起不来。
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

# 打进包里的目录（都在包根下）
PACK_DIRS = ["assets", "ui", "catalog"]
SKIP_NAMES = {".DS_Store", "Thumbs.db"}


def run(cmd: list[str], cwd: Path) -> None:
    print("  $", " ".join(cmd))
    subprocess.run(cmd, cwd=cwd, check=True)


def build() -> None:
    print("→ 编译 release")
    run(["cargo", "build", "--release", "--manifest-path", "backend/sidecar/Cargo.toml"], ROOT)
    run(["cargo", "build", "--release", "--manifest-path", "backend/engine/Cargo.toml"], ROOT)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def walk(folder: Path, prefix: str) -> list[tuple[str, bytes]]:
    out: list[tuple[str, bytes]] = []
    for entry in sorted(folder.iterdir(), key=lambda p: p.name):
        if entry.name.startswith((".", "_")) or entry.name in SKIP_NAMES:
            continue
        rel = f"{prefix}/{entry.name}"
        if entry.is_dir():
            out.extend(walk(entry, rel))
        else:
            out.append((rel, entry.read_bytes()))
    return out


def stage_plugin(target: str, exe: str, version: str) -> Path:
    out = DIST / f"devbox-plugin-{version}-{target}"
    if out.exists():
        shutil.rmtree(out)
    (out / "bin" / target).mkdir(parents=True)

    shutil.copytree(ROOT / "assets", out / "assets")
    shutil.copytree(ROOT / "ui", out / "ui", ignore=shutil.ignore_patterns("node_modules", "*.map"))
    shutil.copytree(ROOT / "catalog", out / "catalog")
    shutil.copy2(ROOT / "backend" / "sidecar" / "target" / "release" / exe, out / "bin" / target / exe)

    # manifest 里的 executable 必须与包内真实路径一字不差
    data = json.loads((ROOT / "manifest.json").read_text(encoding="utf-8"))
    data["entrypoints"]["backend"]["executable"] = f"bin/{target}/{exe}"
    (out / "manifest.json").write_text(
        json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    return out


def collect(plugin_dir: Path, target: str, exe: str) -> list[tuple[str, bytes]]:
    files: list[tuple[str, bytes]] = [
        ("manifest.json", (plugin_dir / "manifest.json").read_bytes()),
        (f"bin/{target}/{exe}", (plugin_dir / "bin" / target / exe).read_bytes()),
    ]
    for name in PACK_DIRS:
        folder = plugin_dir / name
        if folder.is_dir():
            files.extend(walk(folder, name))
    return files


def write_dbxp(files: list[tuple[str, bytes]], exe_rel: str, dest: Path) -> None:
    checksums = {"algorithm": "sha256", "files": {name: sha256_bytes(data) for name, data in files}}
    payload = files + [
        ("checksums.json", (json.dumps(checksums, indent=2) + "\n").encode("utf-8"))
    ]

    if dest.exists():
        dest.unlink()
    with zipfile.ZipFile(dest, "w") as zf:
        for name, data in payload:
            info = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
            info.create_system = 3  # Unix，宿主据此读权限位
            # 可执行文件 0o755，其余 0o644；必须带 S_IFREG，格式与 zip crate 一致
            mode = 0o100755 if name == exe_rel else 0o100644
            info.external_attr = mode << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(info, data)


def verify_dbxp(path: Path) -> tuple[int, bool]:
    """自检：checksums 必须精确覆盖包内除自身与 signature.json 之外的所有文件。"""
    with zipfile.ZipFile(path) as zf:
        names = zf.namelist()
        manifest = json.loads(zf.read("manifest.json"))
        exe = manifest["entrypoints"]["backend"]["executable"]
        covered = json.loads(zf.read("checksums.json"))["files"]
        actual = {
            n for n in names if n not in ("checksums.json", "signature.json") and not n.endswith("/")
        }
        exact = set(covered) == actual
        missing = actual - set(covered)
        extra = set(covered) - actual
        if missing:
            print(f"  ! checksums 未覆盖: {sorted(missing)}")
        if extra:
            print(f"  ! checksums 多出: {sorted(extra)}")
        if exe not in names:
            print(f"  ! manifest 指向的可执行文件不在包里: {exe}")
            exact = False
    return len(names), exact


# ---------------------------------------------------------------- 试用套件

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


def zip_plain(src: Path, dest: Path) -> None:
    if dest.exists():
        dest.unlink()
    with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as zf:
        for path in sorted(src.rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(src).as_posix())


def sha256_file(path: Path) -> str:
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

    manifest = json.loads((ROOT / "manifest.json").read_text(encoding="utf-8"))
    version = manifest["version"]
    plugin_id = manifest["id"]
    exe = "devbox-sidecar.exe" if args.target.startswith("windows") else "devbox-sidecar"

    if not args.skip_build:
        build()

    DIST.mkdir(exist_ok=True)

    print("→ 装配插件包")
    plugin_dir = stage_plugin(args.target, exe, version)
    files = collect(plugin_dir, args.target, exe)
    exe_rel = f"bin/{args.target}/{exe}"
    dbxp = DIST / f"{plugin_id}-{version}-{args.target}.dbxp"
    write_dbxp(files, exe_rel, dbxp)
    count, exact = verify_dbxp(dbxp)
    flag = "✓ 精确覆盖" if exact else "✗ 覆盖不完整"
    print(f"  {dbxp.relative_to(ROOT)}  {dbxp.stat().st_size / 1_048_576:.1f} MB  {count} 个文件  {flag}")

    (DIST / f"{dbxp.stem}.artifact.json").write_text(
        json.dumps(
            {
                "target": args.target,
                "url": dbxp.name,
                "sha256": sha256_file(dbxp),
                "size": dbxp.stat().st_size,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    print("→ 装配试用套件")
    try_dir = stage_try(args.target)
    kit = DIST / f"devbox-try-{version}-{args.target}.zip"
    zip_plain(try_dir, kit)
    print(f"  {kit.relative_to(ROOT)}  {kit.stat().st_size / 1_048_576:.1f} MB")

    print()
    print("sha256")
    print(f"  {dbxp.name}  {sha256_file(dbxp)}")
    print(f"  {kit.name}  {sha256_file(kit)}")

    return 0 if exact else 1


if __name__ == "__main__":
    sys.exit(main())
