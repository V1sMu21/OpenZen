#!/usr/bin/env bash
# OpenZen 本地代码签名身份初始化（每台机器只需跑一次）
#
# 背景：macOS TCC（屏幕录制 / 辅助功能）把授权绑定在 app 的「代码签名身份」上。
# 之前 app 是 ad-hoc 签名，designated requirement 是一个裸 cdhash，每次
# `cargo tauri build` 重建都会变 → 系统设置里开关还开着，但 app 已经对不上授权，
# 于是 computer use 报「权限未开」。
#
# 本脚本创建一个稳定的自签名 code signing 证书 "OpenZen Local Dev"，
# 之后 DR 变成 `identifier "com.openzen.app" and certificate root = H"..."`，
# 与二进制内容无关，重建不再导致授权失效。
#
# 产物保存在 ~/.openzen-signing/（含私钥，chmod 700）—— 不要删除，
# 删了就要重新创建证书并重新授权一次。
#
# 用法: bash scripts/setup-signing.sh
set -euo pipefail

CN="OpenZen Local Dev"
WORK="$HOME/.openzen-signing"
KC="$HOME/Library/Keychains/login.keychain-db"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}==>${NC} $*"; }
warn() { echo -e "${YELLOW}!! ${NC} $*"; }
die()  { echo -e "${RED}!! ${NC}$*" >&2; exit 1; }

# ── 0. 已存在则直接退出（幂等） ─────────────────────────────
if security find-identity -v -p codesigning 2>/dev/null | grep -q "$CN"; then
  log "签名身份 '${CN}' 已存在，无需重建。"
  security find-identity -v -p codesigning | grep "$CN"
  exit 0
fi

command -v openssl >/dev/null || die "openssl 未安装"
[[ -f "$KC" ]] || die "找不到登录钥匙串: $KC"

# ── 1. 生成自签名证书（等价于钥匙串访问的 Self Signed Root + Code Signing） ──
log "生成自签名 code signing 证书: ${CN}"
mkdir -p "$WORK" && chmod 700 "$WORK"
cd "$WORK"

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout key.pem -out cert.pem -days 3650 -sha256 \
  -subj "/CN=${CN}/O=OpenZen/C=CN" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,digitalSignature,keyCertSign" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "subjectKeyIdentifier=hash" >/dev/null 2>&1

# macOS security import 能接受的 PKCS#12 算法组合
openssl pkcs12 -export -inkey key.pem -in cert.pem -name "$CN" -out identity.p12 \
  -passout pass:openzen-tmp \
  -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 >/dev/null 2>&1

chmod 600 key.pem identity.p12

# ── 2. 导入登录钥匙串，仅授权 codesign/security 使用 ────────
log "导入登录钥匙串"
security import "$WORK/identity.p12" -k "$KC" -P openzen-tmp \
  -T /usr/bin/codesign -T /usr/bin/security >/dev/null
rm -f "$WORK/identity.p12"

# ── 3. 设置代码签名信任（否则状态是 CSSMERR_TP_NOT_TRUSTED，codesign 用不了） ──
log "设置 Code Signing 信任"
security add-trusted-cert -r trustRoot -p codeSign -k "$KC" "$WORK/cert.pem"

# ── 4. 验证 ─────────────────────────────────────────────────
if security find-identity -v -p codesigning | grep -q "$CN"; then
  log "完成 ✓ 可用身份："
  security find-identity -v -p codesigning | grep "$CN"
  echo
  warn "证书/私钥保存在 ${WORK}，请勿删除（删除后需重建证书并重新授权）。"
  echo "    下一步: bash scripts/install-macos.sh"
else
  die "身份创建后仍不可用，请检查钥匙串信任设置。"
fi
