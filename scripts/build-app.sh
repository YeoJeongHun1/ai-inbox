#!/bin/bash
# AI Inbox 릴리스 빌드 — 업데이터 서명(.sig)까지 만든다.
#
# 서명 키·암호는 ~/.ai-inbox/secrets/updater.key · updater.password 에서만 읽어(레포에 없다)
# 이 스크립트의 셸 변수로 export 한다 — 빌드 프로세스에만 상속되고 argv·로그·셸 기록에 값이 남지 않는다.
# 빌드 산출물은 src-tauri/.cargo/config.toml 의 target-dir(예: ~/.cache/ai-inbox/target) 로 간다.
#
# 사용: scripts/build-app.sh            # .app + .app.tar.gz + .sig 를 만들고 서명을 검증한다
set -euo pipefail
set +x   # 어떤 이유로든 값이 추적 출력에 찍히지 않게

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SECRETS="${AI_INBOX_SECRETS_DIR:-$HOME/.ai-inbox/secrets}"
KEY_FILE="$SECRETS/updater.key"
PW_FILE="$SECRETS/updater.password"

for f in "$KEY_FILE" "$PW_FILE"; do
  [ -f "$f" ] || { echo "서명 파일이 없습니다: $f" >&2; exit 1; }
  # 다른 사용자가 읽을 수 있으면 멈춘다(값은 찍지 않는다)
  perm=$(stat -f '%Lp' "$f" 2>/dev/null || stat -c '%a' "$f")
  case "$perm" in *00) ;; *) echo "권한이 넓습니다($perm): $f — chmod 600 하세요" >&2; exit 1;; esac
done

# 값은 이 셸 안에서만 — echo·printf·set -x 로 내보내지 않는다
TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_FILE")"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$PW_FILE")"
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD

export PATH="$HOME/.cargo/bin:$PATH"
cd "$ROOT"
[ -d node_modules ] || npm ci
npx tauri build --bundles app

# 산출물 위치 = cargo 가 알려 주는 target 디렉터리
TARGET="$(cd src-tauri && cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')"
BUNDLE="$TARGET/release/bundle/macos"
TGZ="$BUNDLE/AI Inbox.app.tar.gz"
[ -f "$TGZ" ] || { echo "업데이트 묶음이 없습니다: $TGZ" >&2; exit 1; }
[ -f "$TGZ.sig" ] || { echo "서명(.sig)이 만들어지지 않았습니다" >&2; exit 1; }

# 키 환경변수는 더 필요 없다 — 검증엔 공개키만 쓴다
unset TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD
python3 scripts/verify-sig.py "$TGZ" "$TGZ.sig" src-tauri/tauri.conf.json
echo "앱: $BUNDLE/AI Inbox.app"
echo "서명 확인 끝: $TGZ.sig"
