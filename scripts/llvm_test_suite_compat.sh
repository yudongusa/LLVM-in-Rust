#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="${LLVM_COMPAT_WORK_DIR:-"$ROOT_DIR/target/llvm-test-suite-compat"}"
SUITE_DIR="${LLVM_TEST_SUITE_DIR:-"$WORK_DIR/llvm-test-suite"}"
REPORT_DIR="${LLVM_COMPAT_REPORT_DIR:-"$WORK_DIR/report"}"
CORPUS_DIR="${LLVM_COMPAT_CORPUS_DIR:-"$WORK_DIR/ir-corpus"}"
LLVM_TEST_SUITE_REF="${LLVM_TEST_SUITE_REF:-main}"
LLVM_COMPAT_LIMIT="${LLVM_COMPAT_LIMIT:-0}"
LLVM_COMPAT_UPDATE="${LLVM_COMPAT_UPDATE:-1}"
LLVM_COMPAT_SPARSE="${LLVM_COMPAT_SPARSE:-1}"
LLVM_COMPAT_GENERATE_IR="${LLVM_COMPAT_GENERATE_IR:-1}"
LLVM_COMPAT_CLANG="${LLVM_COMPAT_CLANG:-}"

mkdir -p "$WORK_DIR" "$REPORT_DIR" "$CORPUS_DIR"

checkout_corpus_files() {
    if [[ "$LLVM_COMPAT_SPARSE" != "1" || ! -d "$SUITE_DIR/.git" ]]; then
        return
    fi

    local paths_file="$WORK_DIR/llvm-test-suite-corpus-files.txt"
    git -C "$SUITE_DIR" ls-tree -r --name-only HEAD > "$paths_file.all"
    if [[ "$LLVM_COMPAT_GENERATE_IR" == "1" ]]; then
        grep -E '\.(ll|c|cc|cpp|cxx|C|h|hh|hpp|hxx|inc)$' "$paths_file.all" > "$paths_file" || true
    else
        grep -E '\.ll$' "$paths_file.all" > "$paths_file" || true
    fi

    if [[ -s "$paths_file" ]]; then
        echo "[llvm-test-suite-compat] sparse-checking out IR corpus inputs"
        git -C "$SUITE_DIR" sparse-checkout set --no-cone --stdin < "$paths_file"
    else
        echo "[llvm-test-suite-compat] no corpus inputs found in git tree"
    fi
}

if [[ -d "$SUITE_DIR" && ! -d "$SUITE_DIR/.git" ]]; then
    echo "[llvm-test-suite-compat] using existing non-git suite directory: $SUITE_DIR"
elif [[ ! -d "$SUITE_DIR/.git" ]]; then
    echo "[llvm-test-suite-compat] cloning llvm-test-suite@$LLVM_TEST_SUITE_REF"
    git clone --depth 1 --filter=blob:none --sparse --branch "$LLVM_TEST_SUITE_REF" \
        https://github.com/llvm/llvm-test-suite.git "$SUITE_DIR"
    checkout_corpus_files
elif [[ "$LLVM_COMPAT_UPDATE" == "1" ]]; then
    echo "[llvm-test-suite-compat] updating existing llvm-test-suite checkout"
    git -C "$SUITE_DIR" fetch --depth 1 origin "$LLVM_TEST_SUITE_REF"
    git -C "$SUITE_DIR" checkout --detach FETCH_HEAD
    checkout_corpus_files
fi

rm -rf "$CORPUS_DIR"
mkdir -p "$CORPUS_DIR"

while IFS= read -r -d '' ll_file; do
    rel="${ll_file#"$SUITE_DIR"/}"
    dest="$CORPUS_DIR/prebuilt/$rel"
    mkdir -p "$(dirname "$dest")"
    cp "$ll_file" "$dest"
done < <(find "$SUITE_DIR" -type f -name '*.ll' -print0)

prebuilt_ll_count="$(find "$CORPUS_DIR" -type f -name '*.ll' | wc -l | tr -d '[:space:]')"
echo "[llvm-test-suite-compat] copied $prebuilt_ll_count prebuilt .ll files"

find_clang() {
    if [[ -n "$LLVM_COMPAT_CLANG" ]]; then
        command -v "$LLVM_COMPAT_CLANG"
    elif command -v clang-19 >/dev/null 2>&1; then
        command -v clang-19
    elif command -v clang >/dev/null 2>&1; then
        command -v clang
    else
        return 1
    fi
}

if [[ "$prebuilt_ll_count" == "0" && "$LLVM_COMPAT_GENERATE_IR" == "1" ]]; then
    if clang_bin="$(find_clang)"; then
        failures_file="$REPORT_DIR/ir-generation-failures.txt"
        : > "$failures_file"
        generated=0
        failed=0
        echo "[llvm-test-suite-compat] no prebuilt .ll files found; generating LLVM IR with $clang_bin"
        while IFS= read -r -d '' source_file; do
            if [[ "$LLVM_COMPAT_LIMIT" != "0" && "$generated" -ge "$LLVM_COMPAT_LIMIT" ]]; then
                break
            fi
            rel="${source_file#"$SUITE_DIR"/}"
            out="$CORPUS_DIR/generated/${rel%.*}.ll"
            mkdir -p "$(dirname "$out")"
            if "$clang_bin" -S -emit-llvm -O0 -Wno-everything -I"$(dirname "$source_file")" \
                "$source_file" -o "$out" >/dev/null 2>> "$failures_file"; then
                generated=$((generated + 1))
            else
                failed=$((failed + 1))
                rm -f "$out"
                echo "[failed] $rel" >> "$failures_file"
            fi
        done < <(find "$SUITE_DIR" -type f \( -name '*.c' -o -name '*.cc' -o -name '*.cpp' -o -name '*.cxx' -o -name '*.C' \) -print0 | sort -z)
        echo "[llvm-test-suite-compat] generated $generated .ll files ($failed source files failed IR generation)"
    else
        echo "[llvm-test-suite-compat] no clang found; cannot generate .ll fallback corpus"
    fi
fi

ll_count="$(find "$CORPUS_DIR" -type f -name '*.ll' | wc -l | tr -d '[:space:]')"
echo "[llvm-test-suite-compat] running compatibility scan over $ll_count .ll files"

if [[ "$LLVM_COMPAT_LIMIT" != "0" && "$prebuilt_ll_count" != "0" ]]; then
    cargo +stable run -p llvm-in-rust-ir-parser --example llvm_test_suite_compat -- \
        --input-dir "$CORPUS_DIR" \
        --report-md "$REPORT_DIR/report.md" \
        --report-json "$REPORT_DIR/report.json" \
        --limit "$LLVM_COMPAT_LIMIT"
else
    cargo +stable run -p llvm-in-rust-ir-parser --example llvm_test_suite_compat -- \
        --input-dir "$CORPUS_DIR" \
        --report-md "$REPORT_DIR/report.md" \
        --report-json "$REPORT_DIR/report.json"
fi

if [[ -s "$REPORT_DIR/ir-generation-failures.txt" ]]; then
    {
        echo
        echo "## IR Generation Failures"
        echo
        echo "Some LLVM test-suite source files could not be converted to .ll before parser testing. First 20 lines:"
        echo
        echo '```text'
        sed -n '1,20p' "$REPORT_DIR/ir-generation-failures.txt"
        echo '```'
    } >> "$REPORT_DIR/report.md"
fi

echo "[llvm-test-suite-compat] report written to $REPORT_DIR/report.md"
