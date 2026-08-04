#!/bin/sh
set -eu

REPOSITORY="wsdx233/ipmt"
INSTALL_DIR="${IPMT_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    printf 'ipmt installer: %s\n' "$*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || fail "需要 curl，请先安装 curl"
command -v tar >/dev/null 2>&1 || fail "需要 tar，请先安装 tar"
command -v sha256sum >/dev/null 2>&1 || fail "需要 sha256sum，请先安装 coreutils"

[ "$(uname -s)" = "Linux" ] || fail "此脚本目前仅支持 Linux"
case "$(uname -m)" in
    x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
    *) fail "暂不支持当前 CPU 架构：$(uname -m)" ;;
esac

archive="ipmt-${target}.tar.gz"
checksum="ipmt-${target}.sha256"
release_url="https://github.com/${REPOSITORY}/releases/latest/download"
tmp_dir="$(mktemp -d)" || fail "无法创建临时目录"
install_tmp=""

cleanup() {
    rm -rf "$tmp_dir"
    if [ -n "$install_tmp" ]; then
        rm -f "$install_tmp"
    fi
}
trap cleanup EXIT HUP INT TERM

printf '正在下载 ipmt 最新版本（%s）...\n' "$target"
curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error \
    --output "$tmp_dir/$archive" "$release_url/$archive"
curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error \
    --output "$tmp_dir/$checksum" "$release_url/$checksum"

(
    cd "$tmp_dir"
    sha256sum --check "$checksum"
) || fail "SHA-256 校验失败"

tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"
[ -f "$tmp_dir/ipmt" ] || fail "发布包中没有找到 ipmt"

mkdir -p "$INSTALL_DIR"
install_tmp="$INSTALL_DIR/.ipmt.install.$$"
install -m 0755 "$tmp_dir/ipmt" "$install_tmp"
mv -f "$install_tmp" "$INSTALL_DIR/ipmt"
install_tmp=""

case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        printf '安装完成：%s/ipmt\n' "$INSTALL_DIR"
        printf '运行 ipmt 即可启动。\n'
        ;;
    *)
        profile="$HOME/.profile"
        case "${SHELL:-}" in
            */bash) profile="$HOME/.bashrc" ;;
            */zsh) profile="$HOME/.zshrc" ;;
        esac

        if [ "$INSTALL_DIR" = "$HOME/.local/bin" ]; then
            path_line='export PATH="$HOME/.local/bin:$PATH"'
            if ! grep -Fqx "$path_line" "$profile" 2>/dev/null; then
                printf '\n# Added by the ipmt installer\n%s\n' "$path_line" >> "$profile"
            fi
            printf '安装完成：%s/ipmt\n' "$INSTALL_DIR"
            printf '已将 ~/.local/bin 写入 %s；请重新打开终端，或执行：\n' "$profile"
            printf '  export PATH="$HOME/.local/bin:$PATH"\n'
        else
            printf '安装完成：%s/ipmt\n' "$INSTALL_DIR"
            printf '请将该目录加入 PATH 后运行 ipmt：\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
        fi
        ;;
esac
