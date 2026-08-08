#!/usr/bin/env python3
"""统计当前目录所有 .txt 文件的行数，并输出到 CSV。"""

import argparse
import csv
from pathlib import Path


def count_lines(filepath: Path) -> tuple[int, str]:
    """使用迭代器方式统计文件行数（内存友好），返回 (行数, 状态信息)。"""
    try:
        with open(filepath, "r", encoding="utf-8") as f:
            line_count = sum(1 for _ in f)  # 迭代器逐行计数，不一次性加载到内存
        return line_count, "✅"
    except FileNotFoundError:
        return 0, "❌ 文件不存在"
    except PermissionError:
        return 0, "❌ 权限不足"
    except UnicodeDecodeError:
        return 0, "❌ 编码错误（非 UTF-8）"
    except Exception as e:
        return 0, f"❌ {type(e).__name__}: {e}"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="统计指定目录中所有 .txt 文件的行数，并输出结果到 CSV。"
    )
    parser.add_argument(
        "-d", "--directory",
        type=Path,
        default=Path.cwd(),
        help="目标目录（默认为当前工作目录）",
    )
    parser.add_argument(
        "-o", "--output",
        type=Path,
        default=Path("summary.csv"),
        help="输出 CSV 文件路径（默认: summary.csv）",
    )
    args = parser.parse_args()

    # 验证目录
    if not args.directory.exists():
        print(f"[错误] 目录不存在: {args.directory}")
        return
    if not args.directory.is_dir():
        print(f"[错误] 路径不是目录: {args.directory}")
        return

    # 查找所有 .txt 文件
    txt_files = sorted(args.directory.glob("*.txt"))
    if not txt_files:
        print(f"[提示] 目录 '{args.directory}' 中没有找到任何 .txt 文件。")

    # 统计每一行
    results: list[tuple[str, int | str, str]] = []
    for txt_file in txt_files:
        line_count, status = count_lines(txt_file)
        results.append((txt_file.name, line_count, status))

    # 写入 CSV
    with open(args.output, "w", newline="", encoding="utf-8") as csvfile:
        writer = csv.writer(csvfile)
        writer.writerow(["文件", "行数", "状态"])
        writer.writerows(results)

    # 终端预览
    print(f"\n📊 统计结果 ({len(txt_files)} 个文件):")
    print(f"{'文件':<25} {'行数':>8} {'状态'}")
    print("-" * 42)
    for name, lines, status in results:
        print(f"{name:<25} {str(lines):>8} {status}")

    print(f"\n✅ 结果已保存到: {args.output}")


if __name__ == "__main__":
    main()
