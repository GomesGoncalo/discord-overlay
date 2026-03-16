#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description='Generate a simple SVG coverage badge.')
    parser.add_argument('--input', help='Path to tarpaulin JSON report with a top-level coverage field.')
    parser.add_argument('--coverage', type=float, help='Coverage percentage to render directly.')
    parser.add_argument('--output', required=True, help='Output SVG path.')
    args = parser.parse_args()

    if args.input is None and args.coverage is None:
        parser.error('either --input or --coverage is required')

    return args


def load_coverage(args: argparse.Namespace) -> float:
    if args.coverage is not None:
        return args.coverage

    report = json.loads(Path(args.input).read_text())
    coverage = report.get('coverage')
    if coverage is None:
        raise SystemExit('coverage field not found in input report')

    return float(coverage)


def badge_color(coverage: float) -> str:
    if coverage >= 90:
        return '#4c1'
    if coverage >= 75:
        return '#97ca00'
    if coverage >= 60:
        return '#a4a61d'
    if coverage >= 45:
        return '#dfb317'
    if coverage >= 30:
        return '#fe7d37'
    return '#e05d44'


def text_width(text: str) -> int:
    return len(text) * 7 + 10


def render_badge(label: str, value: str, color: str) -> str:
    left_width = text_width(label)
    right_width = text_width(value)
    total_width = left_width + right_width

    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{total_width}" height="20" role="img" aria-label="{label}: {value}">
  <linearGradient id="smooth" x2="0" y2="100%">
    <stop offset="0" stop-color="#fff" stop-opacity=".7"/>
    <stop offset=".1" stop-color="#aaa" stop-opacity=".1"/>
    <stop offset=".9" stop-opacity=".3"/>
    <stop offset="1" stop-opacity=".5"/>
  </linearGradient>
  <clipPath id="round">
    <rect width="{total_width}" height="20" rx="3" fill="#fff"/>
  </clipPath>
  <g clip-path="url(#round)">
    <rect width="{left_width}" height="20" fill="#555"/>
    <rect x="{left_width}" width="{right_width}" height="20" fill="{color}"/>
    <rect width="{total_width}" height="20" fill="url(#smooth)"/>
  </g>
  <g fill="#fff" text-anchor="middle" font-family="DejaVu Sans,Verdana,Geneva,sans-serif" font-size="11">
    <text x="{left_width / 2:.1f}" y="15" fill="#010101" fill-opacity=".3">{label}</text>
    <text x="{left_width / 2:.1f}" y="14">{label}</text>
    <text x="{left_width + right_width / 2:.1f}" y="15" fill="#010101" fill-opacity=".3">{value}</text>
    <text x="{left_width + right_width / 2:.1f}" y="14">{value}</text>
  </g>
</svg>
'''


def main() -> None:
    args = parse_args()
    coverage = load_coverage(args)
    value = f'{coverage:.2f}%'
    svg = render_badge('coverage', value, badge_color(coverage))
    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(svg)


if __name__ == '__main__':
    main()
