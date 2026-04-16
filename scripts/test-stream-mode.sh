#!/bin/bash
# ストリームモード動作確認スクリプト
#
# 使い方:
#   1. ペイン A で kizu を起動:
#        cd /tmp/kizu-stream-test && /path/to/kizu
#
#   2. ペイン B でこのスクリプトを実行:
#        bash scripts/test-stream-mode.sh
#
#   3. ペイン A の kizu で Tab を押してストリームモードに切替
#      → イベントが時系列で表示される

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KIZU_BIN="${KIZU_BIN:-${SCRIPT_DIR}/../target/release/kizu}"
REPO_DIR="/tmp/kizu-stream-test"

echo "=== ストリームモード動作確認 ==="
echo ""

# --- Step 0: テスト用 git repo を作成 ---
if [ ! -d "$REPO_DIR/.git" ]; then
    echo "[setup] テスト用リポジトリを作成: $REPO_DIR"
    mkdir -p "$REPO_DIR"
    cd "$REPO_DIR"
    git init
    git config user.email "test@test.com"
    git config user.name "Test"
    echo 'fn main() { println!("hello"); }' > main.rs
    echo 'def greet(): print("hello")' > app.py
    git add .
    git commit -m "initial"
    echo ""
    echo "[setup] 完了。別ペインで以下を実行してください:"
    echo "  cd $REPO_DIR && $KIZU_BIN"
    echo ""
    echo "kizu が起動したら Enter を押してください..."
    read -r
else
    cd "$REPO_DIR"
    echo "[setup] 既存リポジトリを使用: $REPO_DIR"
    echo ""
    echo "別ペインで kizu が起動していることを確認してください。"
    echo "Enter で続行..."
    read -r
fi

# --- Step 1: Write 操作をシミュレート ---
echo "[1/4] Write: main.rs にトークン検証を追加..."
cat > main.rs << 'RUST'
fn main() {
    let token = get_token();
    if verify_token(&token) {
        println!("authenticated");
    } else {
        println!("denied");
    }
}
RUST

echo '{"session_id":"test-session","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"'"$REPO_DIR"'/main.rs","content":"..."},"cwd":"'"$REPO_DIR"'"}' \
    | "$KIZU_BIN" hook-log-event

echo "  -> hook-log-event 完了"
sleep 1

# --- Step 2: Edit 操作をシミュレート ---
echo "[2/4] Edit: app.py に入力バリデーションを追加..."
cat > app.py << 'PYTHON'
def greet():
    print("hello")

def validate_input(data):
    if not data:
        raise ValueError("empty input")
    return data.strip()
PYTHON

echo '{"session_id":"test-session","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"'"$REPO_DIR"'/app.py","content":"..."},"cwd":"'"$REPO_DIR"'"}' \
    | "$KIZU_BIN" hook-log-event

echo "  -> hook-log-event 完了"
sleep 1

# --- Step 3: Write 新規ファイル ---
echo "[3/4] Write: tests.rs を新規作成..."
cat > tests.rs << 'RUST'
#[cfg(test)]
mod tests {
    #[test]
    fn test_verify_token() {
        assert!(verify_token("valid-token"));
    }
}
RUST

echo '{"session_id":"test-session","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"'"$REPO_DIR"'/tests.rs","content":"..."},"cwd":"'"$REPO_DIR"'"}' \
    | "$KIZU_BIN" hook-log-event

echo "  -> hook-log-event 完了"
sleep 1

# --- Step 4: Edit 再編集 ---
echo "[4/4] Edit: main.rs にエラーハンドリングを追加..."
cat > main.rs << 'RUST'
use std::process;

fn main() {
    let token = match get_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };
    if verify_token(&token) {
        println!("authenticated");
    } else {
        println!("denied");
        process::exit(1);
    }
}
RUST

echo '{"session_id":"test-session","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"'"$REPO_DIR"'/main.rs","content":"..."},"cwd":"'"$REPO_DIR"'"}' \
    | "$KIZU_BIN" hook-log-event

echo "  -> hook-log-event 完了"

echo ""
echo "=== 全 4 イベント送信完了 ==="
echo ""
echo "kizu 側で確認してください:"
echo "  1. Tab キーでストリームモードに切替"
echo "  2. 左ペインに 4 件のイベントが時系列で表示される"
echo "  3. j/k でイベントを選択すると右ペインに diff が表示される"
echo "  4. Tab で diff ビューに戻る"
echo ""
echo "イベントログの場所:"
ls -la ~/Library/Application\ Support/kizu/events/ 2>/dev/null \
    || ls -la "${XDG_STATE_HOME:-$HOME/.local/state}/kizu/events/" 2>/dev/null \
    || echo "  (events dir not found)"
