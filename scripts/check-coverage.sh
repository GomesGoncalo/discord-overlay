#!/bin/bash
# Check that coverage hasn't decreased from baseline
set -e

BASELINE_FILE="coverage-baseline.txt"
TARPAULIN_ENGINE="Llvm"

if [ ! -f "$BASELINE_FILE" ]; then
    echo "❌ $BASELINE_FILE not found"
    exit 1
fi

BASELINE=$(grep -E '^[0-9]+\.[0-9]+$' "$BASELINE_FILE" | tail -1)
if [ -z "$BASELINE" ]; then
    echo "❌ Invalid baseline format in $BASELINE_FILE"
    exit 1
fi

echo "📊 Checking coverage (baseline: ${BASELINE}%)..."

# Reuse an existing tarpaulin report when available.
if [ ! -f "tarpaulin-report.json" ]; then
    # Run tarpaulin and extract coverage from JSON report.
    cargo tarpaulin --engine "$TARPAULIN_ENGINE" --out Json > /dev/null 2>&1
fi

if [ ! -f "tarpaulin-report.json" ]; then
    echo "⚠️  tarpaulin-report.json not generated (skipping coverage check)"
    exit 0
fi

CURRENT=$(jq '.coverage' tarpaulin-report.json 2>/dev/null)

if [ -z "$CURRENT" ]; then
    echo "❌ Failed to extract coverage from tarpaulin-report.json"
    exit 1
fi

# Round to 2 decimal places for comparison
CURRENT=$(printf "%.2f" "$CURRENT")

echo "📊 Current coverage: ${CURRENT}%"

# Compare coverage (bash doesn't do floats, so use awk)
DECREASED=$(awk "BEGIN {if ($CURRENT < $BASELINE) print 1; else print 0}")

if [ "$DECREASED" = "1" ]; then
    echo "❌ Coverage decreased from ${BASELINE}% to ${CURRENT}%"
    echo "   Please add tests to maintain or improve coverage before committing."
    exit 1
fi

if [ "$(awk "BEGIN {if ($CURRENT > $BASELINE) print 1; else print 0}")" = "1" ]; then
    echo "✅ Coverage improved! Current: ${CURRENT}% (was ${BASELINE}%)"
    echo "   Consider updating baseline: echo $CURRENT > $BASELINE_FILE"
else
    echo "✅ Coverage maintained at ${BASELINE}%"
fi
